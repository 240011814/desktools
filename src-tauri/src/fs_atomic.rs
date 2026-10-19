//! 原子写盘：先写同目录临时文件，再 `rename` 到目标路径。
//!
//! 直接覆写目标文件时，进程被杀 / 断电会留下写了一半的文件；而 config.rs、history.rs
//! 的读回策略都是「解析失败按默认值处理」，等于一次崩溃就静默丢掉全部配置 / 历史
//! （config 还会连带丢 deviceId 与凭据）。同目录 rename 在 macOS / Windows / Linux
//! 上都是原子替换，能保证目标文件「要么是旧的完整内容、要么是新的完整内容」。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 临时文件序号（同一进程内保证临时文件名唯一）
static SEQ: AtomicU64 = AtomicU64::new(0);

/// `path.toml` → 同目录下 `<原文件名>.tmp.<进程id>.<序号>`（要求与目标同一卷，保证 rename 原子性）
fn tmp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".into());
    path.with_file_name(format!(
        "{name}.tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// 原子写入一段文本。失败时抛出 io 错误，并尽力清理残留的临时文件。
///
/// 目录不存在时直接返回错误——建目录是调用方（config / history）自己的前置职责，
/// 这里只做「写临时文件 + 原子替换」一件事。
pub fn write_text(path: &Path, text: &str) -> Result<(), io::Error> {
    let tmp = tmp_path(path);
    let write = || -> Result<(), io::Error> {
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    };
    if let Err(err) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "desktools-fsat-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_then_read_back() {
        let dir = temp_dir();
        let path = dir.join("data.json");
        write_text(&path, r#"{"a":1}"#).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"a":1}"#);

        // 覆盖写入：目标被整体替换，不留临时文件
        write_text(&path, r#"{"a":2}"#).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"a":2}"#);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dir_is_an_error_and_leaves_nothing_behind() {
        let dir = temp_dir().join("no-such-dir");
        let path = dir.join("data.json");
        assert!(write_text(&path, "x").is_err());
        assert!(!path.exists());
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }
}
