use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::logger;

/// 全局剪贴板实例（进程内单例，所有线程共用）。
///
/// 不走 `tauri-plugin-clipboard-manager`：该插件按 arboard 建议在 `RunEvent::Exit`
/// 时 `take()` 掉内部实例（`cleanup()`），之后任何并发调用（700ms 轮询的剪贴板
/// 监听线程、消息处理线程写剪贴板等）都会命中插件内部的 `.as_mut().unwrap()` 并
/// panic；release 构建 `panic = "abort"`，表现为退出瞬间 SIGABRT 崩溃
/// （1.0.14 崩溃报告实证：崩溃线程 clipboard-watcher，主线程正卡在退出清理
/// `NSStatusBar _removeStatusItem`）。自持实例退出前不做 take，由进程统一回收，
/// 从结构上消除这个竞态窗口。
#[cfg(desktop)]
static CLIPBOARD: OnceLock<Mutex<arboard::Clipboard>> = OnceLock::new();

/// 在全局实例上执行一次剪贴板操作（串行化所有线程的访问；错误统一转 String）
#[cfg(desktop)]
fn with_clipboard<R>(
    op: impl FnOnce(&mut arboard::Clipboard) -> Result<R, arboard::Error>,
) -> Result<R, String> {
    if CLIPBOARD.get().is_none() {
        let clipboard =
            arboard::Clipboard::new().map_err(|e| format!("初始化剪贴板失败：{e}"))?;
        let _ = CLIPBOARD.set(Mutex::new(clipboard));
    }
    let mutex = CLIPBOARD
        .get()
        .ok_or_else(|| "初始化剪贴板失败".to_string())?;
    let mut clipboard = mutex.lock().unwrap();
    op(&mut clipboard).map_err(|e| e.to_string())
}

/// 读取剪贴板文本（监听线程轮询用；剪贴板里不是文本时返回 Err，调用方忽略）
pub fn read_text() -> Result<String, String> {
    #[cfg(desktop)]
    {
        with_clipboard(|clipboard| clipboard.get_text())
    }
    #[cfg(not(desktop))]
    {
        Err("当前平台不支持剪贴板操作".into())
    }
}

/// 读取剪贴板图片，返回 (宽, 高, RGBA8 原始数据)（文件中转站「粘贴图片」用）
pub fn read_image() -> Result<(u32, u32, Vec<u8>), String> {
    #[cfg(desktop)]
    {
        with_clipboard(|clipboard| clipboard.get_image()).map(|image| {
            (
                image.width as u32,
                image.height as u32,
                image.bytes.into_owned(),
            )
        })
    }
    #[cfg(not(desktop))]
    {
        Err("当前平台不支持剪贴板操作".into())
    }
}

/// 写入剪贴板并读回校验，失败自动重试。
///
/// macOS 上 `NSPasteboard#writeObjects` 与其它进程（用户复制、pbcopy、其它 app）
/// 并发时会间歇性返回 false，导致写入丢失；Windows 的 `OpenClipboard` 也有类似竞争。
/// 因此写入后读回比对，不一致则退避重试，保证远端内容最终落到剪贴板。
///
/// 返回成功时的尝试次数（>1 说明发生过竞争，可写日志观察）。
pub fn write_text_verified(text: &str) -> Result<u32, String> {
    #[cfg(desktop)]
    {
        write_with_retry(
            text,
            || with_clipboard(|clipboard| clipboard.set_text(text)),
            || with_clipboard(|clipboard| clipboard.get_text()),
            4,
            60,
        )
    }
    #[cfg(not(desktop))]
    {
        let _ = text;
        Err("当前平台不支持剪贴板操作".into())
    }
}

/// 将文本写入系统剪贴板（前端复制统一走后端，跨平台一致）。
/// 写入在剪贴板闸门内完成，与监听线程 / 消息处理线程的决策互斥；
/// 不更新 `last_clip`，由监听线程把点击复制的内容当作新复制发布到同步 topic（保持跨端同步）。
pub fn copy(app: &AppHandle, text: &str) -> Result<u32, String> {
    let attempts = app
        .state::<crate::mqtt::SyncState>()
        .with_clip_gate(|| write_text_verified(text))?;
    logger::log(
        app,
        logger::INFO,
        format!(
            "已复制到剪贴板（{} 字符{}）",
            text.chars().count(),
            if attempts > 1 {
                format!("，第 {attempts} 次尝试成功")
            } else {
                String::new()
            }
        ),
    );
    Ok(attempts)
}

/// 可注入读写实现的重试逻辑（纯逻辑，便于单元测试）。
/// 每次尝试：写入 → 读回比对；不一致或写失败则退避后重试，全部失败返回最后一次错误。
pub fn write_with_retry<W, R>(
    text: &str,
    mut write: W,
    mut read: R,
    max_attempts: u32,
    base_delay_ms: u64,
) -> Result<u32, String>
where
    W: FnMut() -> Result<(), String>,
    R: FnMut() -> Result<String, String>,
{
    let mut last_err = "未知错误".to_string();
    for attempt in 1..=max_attempts {
        match write() {
            Err(err) => last_err = err,
            Ok(()) => match read() {
                Ok(current) if current == text => return Ok(attempt),
                Ok(_) => last_err = "写入后校验不一致（可能被其它程序覆盖或写入丢失）".into(),
                Err(err) => last_err = format!("写入后校验读取失败：{err}"),
            },
        }
        if attempt < max_attempts {
            std::thread::sleep(Duration::from_millis(base_delay_ms * u64::from(attempt)));
        }
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn succeeds_on_first_attempt() {
        let attempts = write_with_retry(
            "hello",
            || Ok(()),
            || Ok("hello".into()),
            3,
            0,
        )
        .unwrap();
        assert_eq!(attempts, 1);
    }

    #[test]
    fn retries_until_write_lands() {
        let reads = std::cell::Cell::new(0u32);
        let attempts = write_with_retry(
            "hello",
            || Ok(()),
            // 前两次读回是旧内容（写入被竞争挤掉），第三次成功
            || {
                let n = reads.get() + 1;
                reads.set(n);
                if n < 3 {
                    Ok("old".into())
                } else {
                    Ok("hello".into())
                }
            },
            5,
            0,
        )
        .unwrap();
        assert_eq!(attempts, 3);
    }

    #[test]
    fn reports_error_when_all_attempts_fail() {
        let result = write_with_retry(
            "hello",
            || Err("writeObjects returned false".into()),
            || Ok(String::new()),
            3,
            0,
        );
        assert_eq!(result, Err("writeObjects returned false".to_string()));
    }

    #[test]
    fn reports_mismatch_when_read_never_matches() {
        let result = write_with_retry("hello", || Ok(()), || Ok("other".into()), 2, 0);
        assert!(result.unwrap_err().contains("校验不一致"));
    }
}
