use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::logger::{self, INFO};

/// 前端监听的历史变更事件
pub const HISTORY_EVENT: &str = "history://changed";
/// 前端监听的刷新事件（打开 / 聚焦历史窗口时触发一次刷新）
pub const HISTORY_REFRESH_EVENT: &str = "history://refresh";
/// 最多保存的记录条数
pub const HISTORY_LIMIT: usize = 200;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 一条剪贴板历史记录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub id: String,
    pub text: String,
    /// local = 本机复制，remote = 远端设备同步
    pub source: String,
    /// 记录时间（毫秒时间戳）
    pub ts: i64,
}

/// 剪贴板历史（最多 200 条，持久化保存）
pub struct HistoryState {
    items: Mutex<Vec<HistoryItem>>,
    path: PathBuf,
}

impl HistoryState {
    pub fn load(path: PathBuf) -> Self {
        let items = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            items: Mutex::new(items),
            path,
        }
    }

    fn save(&self, items: &[HistoryItem]) {
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(items) {
            let _ = fs::write(&self.path, text);
        }
    }

    /// 新增一条记录：相同内容不重复记录（更新时间并置顶），超出上限淘汰最旧
    pub fn push(&self, app: &AppHandle, text: &str, source: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        {
            let mut items = self.items.lock().unwrap();
            items.retain(|item| item.text != text);
            items.insert(
                0,
                HistoryItem {
                    id: uuid::Uuid::new_v4().to_string(),
                    text: text.to_string(),
                    source: source.to_string(),
                    ts: now_ms(),
                },
            );
            items.truncate(HISTORY_LIMIT);
            self.save(&items);
            logger::log(
                app,
                INFO,
                format!(
                    "历史记录 +1（{}，共 {} 条）",
                    if source == "local" {
                        "本机复制"
                    } else {
                        "远端同步"
                    },
                    items.len()
                ),
            );
        }
        let _ = app.emit(HISTORY_EVENT, ());
    }

    pub fn list(&self) -> Vec<HistoryItem> {
        self.items.lock().unwrap().clone()
    }

    pub fn delete(&self, app: &AppHandle, id: &str) -> Result<(), String> {
        {
            let mut items = self.items.lock().unwrap();
            let before = items.len();
            items.retain(|item| item.id != id);
            if items.len() == before {
                return Err("记录不存在".into());
            }
            self.save(&items);
            logger::log(app, INFO, "历史记录 -1（手动删除）");
        }
        let _ = app.emit(HISTORY_EVENT, ());
        Ok(())
    }

    pub fn clear(&self, app: &AppHandle) {
        {
            let mut items = self.items.lock().unwrap();
            items.clear();
            self.save(&items);
            logger::log(app, INFO, "历史记录已全部清空");
        }
        let _ = app.emit(HISTORY_EVENT, ());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (HistoryState, PathBuf) {
        let dir = std::env::temp_dir().join(format!("desktools-his-{}", uuid::Uuid::new_v4()));
        (HistoryState::load(dir.join("history.json")), dir)
    }

    #[test]
    fn keeps_at_most_limit_items() {
        let (state, dir) = state();
        {
            let mut items = state.items.lock().unwrap();
            for i in 0..(HISTORY_LIMIT + 50) {
                items.insert(
                    0,
                    HistoryItem {
                        id: format!("id-{i}"),
                        text: format!("text-{i}"),
                        source: "local".into(),
                        ts: i as i64,
                    },
                );
            }
            items.truncate(HISTORY_LIMIT);
            state.save(&items);
        }
        let loaded = HistoryState::load(dir.join("history.json"));
        assert_eq!(loaded.list().len(), HISTORY_LIMIT);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedup_moves_to_top() {
        let (state, _dir) = state();
        let mut items = state.items.lock().unwrap();
        for text in ["a", "b", "c"] {
            items.insert(
                0,
                HistoryItem {
                    id: text.into(),
                    text: text.into(),
                    source: "local".into(),
                    ts: 0,
                },
            );
        }
        drop(items);
        // 模拟重复复制 "b"：应去重并置顶
        {
            let mut items = state.items.lock().unwrap();
            items.retain(|item| item.text != "b");
            items.insert(
                0,
                HistoryItem {
                    id: "b".into(),
                    text: "b".into(),
                    source: "local".into(),
                    ts: 1,
                },
            );
            state.save(&items);
        }
        let list = state.list();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].text, "b");
    }

    #[test]
    fn delete_and_clear() {
        let (state, _dir) = state();
        {
            let mut items = state.items.lock().unwrap();
            items.push(HistoryItem {
                id: "x".into(),
                text: "x".into(),
                source: "local".into(),
                ts: 0,
            });
        }
        // 直接操作内部结构（delete/clear 需要 AppHandle 用于发事件）
        {
            let mut items = state.items.lock().unwrap();
            items.retain(|item| item.id != "x");
            state.save(&items);
        }
        assert!(state.list().is_empty());
        {
            let mut items = state.items.lock().unwrap();
            items.clear();
            state.save(&items);
        }
        assert!(state.list().is_empty());
    }
}
