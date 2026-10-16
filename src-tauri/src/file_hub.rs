//! 文件中转站：通过 MinIO（S3 兼容接口）在固定 bucket `hub` 里上传 / 下载 / 删除文件
//!
//! - 连接固定 path-style（MinIO 必需），签名 region 使用 us-east-1（MinIO 默认配置不校验）
//! - bucket 不存在时自动创建
//! - 大文件由 rust-s3 自动分片（阈值 8 MiB，读写流式，不整份进内存）
//! - 上传 / 下载过程发送 `filehub://progress` 事件（200ms 节流），失败时清理下载的半成品文件

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

use s3::bucket::Bucket;
use s3::creds::Credentials;
use s3::region::Region;
use s3::BucketConfiguration;

use crate::config::{self, MinioConfig};
use crate::logger::{self, INFO, WARN};

/// 文件中转站固定使用的 bucket（不暴露给用户配置）
pub const BUCKET: &str = "hub";
/// SigV4 签名 region（MinIO 未配置 region 时接受任意值，us-east-1 兼容性最好）
const REGION: &str = "us-east-1";
/// 传输进度事件名（前端 listen）
pub const PROGRESS_EVENT: &str = "filehub://progress";
/// 进度事件最小发送间隔，避免高频 IPC 事件拖慢前端
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// 返回给前端的文件条目
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubFile {
    pub key: String,
    pub size: u64,
    pub last_modified: String,
}

/// 传输进度事件负载
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload {
    transfer_id: String,
    key: String,
    /// upload / download
    direction: &'static str,
    /// progress / done / error
    phase: &'static str,
    transferred: u64,
    total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// 未配置时给出明确指引（设置页入口）
fn require_configured(cfg: &MinioConfig) -> Result<(), String> {
    if cfg.configured() {
        Ok(())
    } else {
        Err("尚未配置 MinIO，请先到「系统设置 → 文件中转站」完成配置".into())
    }
}

/// 用配置构建 bucket 客户端（path-style；region / 凭据一并返回供建桶复用）
fn make_bucket(cfg: &MinioConfig) -> Result<(Box<Bucket>, Region, Credentials), String> {
    let (host, port, tls) = config::normalize_endpoint(&cfg.host, cfg.port, cfg.tls);
    let scheme = if tls { "https" } else { "http" };
    let region = Region::Custom {
        region: REGION.to_string(),
        endpoint: format!("{scheme}://{host}:{port}"),
    };
    let creds = Credentials::new(Some(&cfg.access_key), Some(&cfg.secret_key), None, None, None)
        .map_err(|e| format!("凭据无效：{e}"))?;
    let bucket = Bucket::new(BUCKET, region.clone(), creds.clone())
        .map_err(|e| format!("初始化 MinIO 客户端失败：{e}"))?
        .with_path_style();
    Ok((bucket, region, creds))
}

/// 确保 bucket 存在：先做一次廉价的列举探测（max-keys=1），失败则尝试创建
/// （已存在 / 并发创建视为成功；其余失败原样上报，便于定位权限或网络问题）
async fn ensure_bucket(cfg: &MinioConfig, app: &AppHandle) -> Result<Box<Bucket>, String> {
    let (bucket, region, creds) = make_bucket(cfg)?;
    if bucket
        .list_page(String::new(), None, None, None, Some(1))
        .await
        .is_ok()
    {
        return Ok(bucket);
    }
    match Bucket::create_with_path_style(BUCKET, region, creds, BucketConfiguration::default()).await
    {
        Ok(resp) if resp.success() => {
            logger::log(
                app,
                INFO,
                format!("文件中转站：bucket「{BUCKET}」不存在，已自动创建"),
            );
            Ok(bucket)
        }
        Ok(resp)
            if resp.response_code == 409 || resp.response_text.contains("BucketAlready") =>
        {
            Ok(bucket)
        }
        Ok(resp) => Err(format!(
            "连接 MinIO 失败（HTTP {}）：{}",
            resp.response_code,
            resp.response_text.trim()
        )),
        Err(err) => Err(format!("连接 MinIO 失败：{err}")),
    }
}

/// 列出 bucket 内全部文件（过滤“文件夹”占位对象，按修改时间倒序）
pub async fn list(app: &AppHandle, cfg: &MinioConfig) -> Result<Vec<HubFile>, String> {
    require_configured(cfg)?;
    let bucket = ensure_bucket(cfg, app).await?;
    let pages = bucket
        .list(String::new(), None)
        .await
        .map_err(|e| format!("读取文件列表失败：{e}"))?;
    let mut files: Vec<HubFile> = pages
        .iter()
        .flat_map(|page| page.contents.iter())
        .filter(|obj| !obj.key.is_empty() && !obj.key.ends_with('/'))
        .map(|obj| HubFile {
            key: obj.key.clone(),
            size: obj.size,
            last_modified: obj.last_modified.clone(),
        })
        .collect();
    files.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    Ok(files)
}

/// 上传本地文件（远端 key = 文件名），返回上传后的 key
pub async fn upload(
    app: &AppHandle,
    cfg: &MinioConfig,
    path: &str,
    transfer_id: &str,
) -> Result<String, String> {
    require_configured(cfg)?;
    let key = object_key_from_path(path)?;
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("读取文件失败：{e}"))?;
    if !meta.is_file() {
        return Err("只支持上传文件（不能上传文件夹）".into());
    }
    let total = meta.len();
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("打开文件失败：{e}"))?;
    let bucket = ensure_bucket(cfg, app).await?;
    let mut reader = ProgressReader {
        inner: file,
        app: app.clone(),
        transfer_id: transfer_id.to_string(),
        key: key.clone(),
        direction: "upload",
        transferred: 0,
        total,
        last_emit: Instant::now(),
    };
    let result = bucket
        .put_object_stream_with_content_type(&mut reader, &key, guess_content_type(&key))
        .await;
    match result {
        Ok(resp) if resp.status_code() < 300 => {
            emit_progress(
                app, transfer_id, &key, "upload", "done", total, total, None,
            );
            Ok(key)
        }
        Ok(resp) => {
            let message = format!("上传失败（HTTP {}）", resp.status_code());
            emit_progress(
                app,
                transfer_id,
                &key,
                "upload",
                "error",
                reader.transferred,
                total,
                Some(message.clone()),
            );
            Err(message)
        }
        Err(err) => {
            let message = format!("上传失败：{err}");
            emit_progress(
                app,
                transfer_id,
                &key,
                "upload",
                "error",
                reader.transferred,
                total,
                Some(message.clone()),
            );
            Err(message)
        }
    }
}

/// 下载文件到本地指定路径；失败时清理不完整的半成品文件
pub async fn download(
    app: &AppHandle,
    cfg: &MinioConfig,
    key: &str,
    target_path: &str,
    transfer_id: &str,
) -> Result<(), String> {
    require_configured(cfg)?;
    let bucket = ensure_bucket(cfg, app).await?;
    // 先 HEAD 取文件大小（进度用）并确认对象存在
    let (head, status) = bucket
        .head_object(key)
        .await
        .map_err(|e| format!("读取文件信息失败：{e}"))?;
    if status == 404 {
        return Err("文件不存在（可能已被其他设备删除），请刷新列表".into());
    }
    if status >= 300 {
        return Err(format!("读取文件信息失败（HTTP {status}）"));
    }
    let total = head.content_length.unwrap_or(0).max(0) as u64;

    if let Some(dir) = std::path::Path::new(target_path).parent() {
        if !dir.as_os_str().is_empty() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| format!("创建目录失败：{e}"))?;
        }
    }
    let file = tokio::fs::File::create(target_path)
        .await
        .map_err(|e| format!("创建文件失败：{e}"))?;
    let mut writer = ProgressWriter {
        inner: file,
        app: app.clone(),
        transfer_id: transfer_id.to_string(),
        key: key.to_string(),
        direction: "download",
        transferred: 0,
        total,
        last_emit: Instant::now(),
    };
    match bucket.get_object_to_writer(key, &mut writer).await {
        Ok(status) if status < 300 => {
            if let Err(err) = writer.flush().await {
                let message = format!("写入文件失败：{err}");
                let transferred = writer.transferred;
                drop(writer);
                cleanup_partial(app, target_path).await;
                emit_progress(
                    app,
                    transfer_id,
                    key,
                    "download",
                    "error",
                    transferred,
                    total,
                    Some(message.clone()),
                );
                return Err(message);
            }
            emit_progress(app, transfer_id, key, "download", "done", total, total, None);
            Ok(())
        }
        Ok(status) => {
            let message = format!("下载失败（HTTP {status}）");
            let transferred = writer.transferred;
            drop(writer);
            cleanup_partial(app, target_path).await;
            emit_progress(
                app,
                transfer_id,
                key,
                "download",
                "error",
                transferred,
                total,
                Some(message.clone()),
            );
            Err(message)
        }
        Err(err) => {
            let message = format!("下载失败：{err}");
            let transferred = writer.transferred;
            drop(writer);
            cleanup_partial(app, target_path).await;
            emit_progress(
                app,
                transfer_id,
                key,
                "download",
                "error",
                transferred,
                total,
                Some(message.clone()),
            );
            Err(message)
        }
    }
}

/// 删除一个文件
pub async fn delete(app: &AppHandle, cfg: &MinioConfig, key: &str) -> Result<(), String> {
    require_configured(cfg)?;
    let bucket = ensure_bucket(cfg, app).await?;
    let resp = bucket
        .delete_object(key)
        .await
        .map_err(|e| format!("删除失败：{e}"))?;
    let code = resp.status_code();
    if code >= 300 {
        return Err(format!("删除失败（HTTP {code}）"));
    }
    Ok(())
}

/// 测试连接：确保 bucket 可用并返回一句话结果（设置页展示）
pub async fn test_connection(app: &AppHandle, cfg: &MinioConfig) -> Result<String, String> {
    require_configured(cfg)?;
    let bucket = ensure_bucket(cfg, app).await?;
    let pages = bucket
        .list(String::new(), None)
        .await
        .map_err(|e| format!("列举文件失败：{e}"))?;
    let count: usize = pages.iter().map(|page| page.contents.len()).sum();
    Ok(format!(
        "连接成功：bucket「{BUCKET}」可用，当前共有 {count} 个文件"
    ))
}

/// 删除下载中断产生的不完整文件（best-effort，失败只记日志）
async fn cleanup_partial(app: &AppHandle, path: &str) {
    if let Err(err) = tokio::fs::remove_file(path).await {
        logger::log(
            app,
            WARN,
            format!("文件中转站：清理未完成文件失败（{path}）：{err}"),
        );
    }
}

/// 发送进度事件（事件发送失败只影响进度展示，不影响传输本身）
#[allow(clippy::too_many_arguments)]
fn emit_progress(
    app: &AppHandle,
    transfer_id: &str,
    key: &str,
    direction: &'static str,
    phase: &'static str,
    transferred: u64,
    total: u64,
    message: Option<String>,
) {
    let payload = ProgressPayload {
        transfer_id: transfer_id.to_string(),
        key: key.to_string(),
        direction,
        phase,
        transferred,
        total,
        message,
    };
    let _ = app.emit(PROGRESS_EVENT, payload);
}

/// 从本地路径提取上传 key（只取文件名，兼容 Windows 反斜杠路径）
fn object_key_from_path(path: &str) -> Result<String, String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if name.is_empty() {
        return Err("无法从路径中提取文件名".into());
    }
    Ok(name.to_string())
}

/// 按扩展名猜 Content-Type（未知类型用 application/octet-stream）
fn guess_content_type(name: &str) -> &'static str {
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "txt" | "log" | "md" => "text/plain; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml",
        "html" | "htm" => "text/html; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}

/// 计数 Reader：上传时统计已读字节并节流发送进度事件
struct ProgressReader {
    inner: tokio::fs::File,
    app: AppHandle,
    transfer_id: String,
    key: String,
    direction: &'static str,
    transferred: u64,
    total: u64,
    last_emit: Instant,
}

impl ProgressReader {
    fn maybe_emit(&mut self) {
        if self.transferred >= self.total || self.last_emit.elapsed() >= PROGRESS_INTERVAL {
            self.last_emit = Instant::now();
            emit_progress(
                &self.app,
                &self.transfer_id,
                &self.key,
                self.direction,
                "progress",
                self.transferred,
                self.total,
                None,
            );
        }
    }
}

impl AsyncRead for ProgressReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            let read = (buf.filled().len() - before) as u64;
            this.transferred += read;
            this.maybe_emit();
        }
        result
    }
}

/// 计数 Writer：下载时统计已写字节并节流发送进度事件
struct ProgressWriter {
    inner: tokio::fs::File,
    app: AppHandle,
    transfer_id: String,
    key: String,
    direction: &'static str,
    transferred: u64,
    total: u64,
    last_emit: Instant,
}

impl ProgressWriter {
    fn maybe_emit(&mut self) {
        if self.transferred >= self.total || self.last_emit.elapsed() >= PROGRESS_INTERVAL {
            self.last_emit = Instant::now();
            emit_progress(
                &self.app,
                &self.transfer_id,
                &self.key,
                self.direction,
                "progress",
                self.transferred,
                self.total,
                None,
            );
        }
    }
}

impl AsyncWrite for ProgressWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &result {
            this.transferred += *n as u64;
            this.maybe_emit();
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_key_takes_file_name() {
        assert_eq!(
            object_key_from_path("/Users/hyy/我的 文件.pdf").unwrap(),
            "我的 文件.pdf"
        );
        assert_eq!(
            object_key_from_path("C:\\Users\\hyy\\report.docx").unwrap(),
            "report.docx"
        );
        assert_eq!(object_key_from_path("plain.txt").unwrap(), "plain.txt");
        // 目录路径 / 空路径无法提取文件名
        assert!(object_key_from_path("/Users/hyy/").is_err());
        assert!(object_key_from_path("").is_err());
    }

    #[test]
    fn content_type_by_extension() {
        assert_eq!(guess_content_type("a.png"), "image/png");
        assert_eq!(guess_content_type("A.JPEG"), "image/jpeg");
        assert_eq!(guess_content_type("notes.md"), "text/plain; charset=utf-8");
        assert_eq!(
            guess_content_type("noext"),
            "application/octet-stream"
        );
    }

    /// 真实 MinIO 端到端冒烟测试（默认跳过）：
    /// `DESKTOOLS_MINIO_HOST=127.0.0.1:9000 DESKTOOLS_MINIO_KEY=xx DESKTOOLS_MINIO_SECRET=yy \
    ///  cargo test file_hub::tests::e2e_minio_roundtrip -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn e2e_minio_roundtrip() {
        let cfg = MinioConfig {
            host: std::env::var("DESKTOOLS_MINIO_HOST").expect("缺少 DESKTOOLS_MINIO_HOST"),
            port: 9000,
            tls: false,
            access_key: std::env::var("DESKTOOLS_MINIO_KEY").expect("缺少 DESKTOOLS_MINIO_KEY"),
            secret_key: std::env::var("DESKTOOLS_MINIO_SECRET")
                .expect("缺少 DESKTOOLS_MINIO_SECRET"),
        };
        let (bucket, _, _) = make_bucket(&cfg).unwrap();
        let key = format!(
            "desktools-e2e-{}.txt",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

        let mut cursor = std::io::Cursor::new(b"hello-desktools".to_vec());
        let resp = bucket
            .put_object_stream_with_content_type(&mut cursor, &key, "text/plain")
            .await
            .unwrap();
        assert!(resp.status_code() < 300, "上传状态码 {}", resp.status_code());

        let pages = bucket.list(String::new(), None).await.unwrap();
        assert!(
            pages
                .iter()
                .flat_map(|page| page.contents.iter())
                .any(|obj| obj.key == key),
            "列表应包含 {key}"
        );

        let data = bucket.get_object(&key).await.unwrap();
        assert_eq!(data.bytes().as_ref(), b"hello-desktools".as_slice());

        let resp = bucket.delete_object(&key).await.unwrap();
        assert!(resp.status_code() < 300, "删除状态码 {}", resp.status_code());
    }
}
