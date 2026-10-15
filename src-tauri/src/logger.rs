use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::history::now_ms;

/// 前端监听的日志变更事件
pub const LOG_EVENT: &str = "log://changed";
/// 内存中保留的最大日志条数
pub const MAX_ENTRIES: usize = 1000;
/// 日志文件超过该大小后轮转（保留 1 个 .old 备份）
const MAX_FILE_BYTES: u64 = 1024 * 1024;

pub const INFO: &str = "INFO";
pub const WARN: &str = "WARN";
pub const ERROR: &str = "ERROR";

/// 一条运行日志
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    /// 毫秒时间戳
    pub ts: i64,
    /// INFO / WARN / ERROR
    pub level: String,
    pub message: String,
}

/// 运行日志：内存（最近 1000 条）+ JSONL 文件持久化（自动轮转）
pub struct LogState {
    entries: Mutex<Vec<LogEntry>>,
    path: PathBuf,
}

impl LogState {
    pub fn load(path: PathBuf) -> Self {
        // 启动时从文件恢复最近的日志，便于重启后回看
        let entries = fs::read_to_string(&path)
            .map(|text| {
                text.lines()
                    .filter_map(|line| serde_json::from_str::<LogEntry>(line).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Self {
            entries: Mutex::new(entries),
            path,
        }
    }

    pub fn append(&self, app: &AppHandle, level: &str, message: &str) {
        let entry = LogEntry {
            ts: now_ms(),
            level: level.to_string(),
            message: message.to_string(),
        };
        {
            let mut entries = self.entries.lock().unwrap();
            entries.push(entry.clone());
            let len = entries.len();
            if len > MAX_ENTRIES {
                entries.drain(0..len - MAX_ENTRIES);
            }
        }
        self.write_line(&entry);
        let _ = app.emit(LOG_EVENT, ());
    }

    fn write_line(&self, entry: &LogEntry) {
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        // 文件过大时轮转，最多保留 1 个备份
        if let Ok(meta) = fs::metadata(&self.path) {
            if meta.len() > MAX_FILE_BYTES {
                let backup = self.path.with_extension("jsonl.old");
                let _ = fs::rename(&self.path, backup);
            }
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.path) {
            if let Ok(line) = serde_json::to_string(entry) {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    pub fn list(&self) -> Vec<LogEntry> {
        self.entries.lock().unwrap().clone()
    }

    pub fn clear(&self, app: &AppHandle) {
        self.entries.lock().unwrap().clear();
        let _ = fs::write(&self.path, "");
        let _ = app.emit(LOG_EVENT, ());
    }
}

/// 截断长文本用于日志展示（超过 500 字符只记前 500 字符并标注）
pub fn preview(text: &str) -> String {
    const MAX: usize = 500;
    let total = text.chars().count();
    if total <= MAX {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX).collect();
    format!("{head}…（已截断，共 {total} 字符）")
}

/// 记录一条日志（LogState 未初始化时静默忽略）
pub fn log(app: &AppHandle, level: &str, message: impl AsRef<str>) {
    if let Some(state) = app.try_state::<LogState>() {
        state.append(app, level, message.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("desktools-log-{}", uuid::Uuid::new_v4()))
            .join("desktools.jsonl")
    }

    #[test]
    fn write_and_reload() {
        let path = temp_path();
        let state = LogState::load(path.clone());
        state.write_line(&LogEntry {
            ts: 1,
            level: INFO.into(),
            message: "启动".into(),
        });
        state.write_line(&LogEntry {
            ts: 2,
            level: ERROR.into(),
            message: "出错".into(),
        });
        let loaded = LogState::load(path.clone());
        let entries = loaded.list();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].level, INFO);
        assert_eq!(entries[1].message, "出错");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn keeps_at_most_max_entries() {
        let path = temp_path();
        let state = LogState::load(path.clone());
        {
            let mut entries = state.entries.lock().unwrap();
            for i in 0..(MAX_ENTRIES + 200) {
                entries.push(LogEntry {
                    ts: i as i64,
                    level: INFO.into(),
                    message: format!("m{i}"),
                });
            }
            let len = entries.len();
            entries.drain(0..len - MAX_ENTRIES);
            assert_eq!(entries.len(), MAX_ENTRIES);
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn rotates_when_file_too_large() {
        let path = temp_path();
        let state = LogState::load(path.clone());
        // 写入超过阈值的内容触发轮转
        let big = "x".repeat(64 * 1024);
        for _ in 0..20 {
            state.write_line(&LogEntry {
                ts: 0,
                level: INFO.into(),
                message: big.clone(),
            });
        }
        assert!(state.path.with_extension("jsonl.old").exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn clear_truncates_file() {
        let path = temp_path();
        let state = LogState::load(path.clone());
        state.write_line(&LogEntry {
            ts: 1,
            level: INFO.into(),
            message: "x".into(),
        });
        state.entries.lock().unwrap().clear();
        fs::write(&state.path, "").unwrap();
        assert!(LogState::load(path.clone()).list().is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
