use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Manager};

use crate::history::now_ms;
use crate::logger::{self, ERROR, INFO, WARN};

/// CSI 人体感知通知主题（wol_rs / ESP32 固定发布此主题，消息内含设备 id）
pub const CSI_NOTIFY_TOPIC: &str = "csi/notify";

/// 两次自动锁屏之间的最短间隔，避免连续 leave 反复锁屏
const LOCK_COOLDOWN_MS: i64 = 60_000;

/// csi/notify 消息：{"id":"...","cmd":"csi","event":"approach|leave","slope":..,"amp":..,"rs":..,"frames":..}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsiMessage {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub event: String,
}

/// 自动锁屏状态：待执行锁屏的取消代号 + 冷却时间戳
pub struct AutoLockState {
    /// 待执行锁屏任务的代号：leave 时自增，approach / 关闭开关时再自增即取消
    pending_gen: AtomicU64,
    /// 是否有挂起中的延迟锁屏（用于日志与取消判断）
    pending: AtomicBool,
    /// 最近一次触发锁屏的时间（冷却用）
    last_lock_ms: Mutex<i64>,
}

impl Default for AutoLockState {
    fn default() -> Self {
        Self {
            pending_gen: AtomicU64::new(0),
            pending: AtomicBool::new(false),
            last_lock_ms: Mutex::new(0),
        }
    }
}

impl AutoLockState {
    /// 处理一条 csi/notify 消息（由消息处理线程调用）
    pub fn handle_event(&self, app: &AppHandle, raw: &str) {
        let Some(msg) = parse_csi_event(raw) else {
            logger::log(app, WARN, format!("自动锁屏：无法解析的 CSI 消息：{}", logger::preview(raw)));
            return;
        };
        match msg.event.as_str() {
            "leave" => self.schedule_lock(app, &msg.id),
            "approach" => self.cancel_pending(app, &msg.id),
            _ => {}
        }
    }

    /// 收到 leave：延迟锁屏（期间检测到有人回来则取消）
    fn schedule_lock(&self, app: &AppHandle, device: &str) {
        let cfg = app.state::<crate::mqtt::SyncState>().config.lock().unwrap().clone();
        if !cfg.auto_lock {
            return;
        }
        let now = now_ms();
        if in_cooldown(*self.last_lock_ms.lock().unwrap(), now) {
            logger::log(app, INFO, "自动锁屏：距上次锁屏不足 60 秒，忽略本次离开事件");
            return;
        }
        let delay = cfg.auto_lock_delay_secs.clamp(1, 300);
        // 自增代号：取消此前挂起的锁屏任务
        let task_id = self.pending_gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.pending.store(true, Ordering::SeqCst);
        logger::log(
            app,
            INFO,
            format!("自动锁屏：设备 {device} 检测到人离开，{delay} 秒后锁屏（期间检测到有人则取消）"),
        );

        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(delay));
            let state = app.state::<AutoLockState>();
            if state.pending_gen.load(Ordering::SeqCst) != task_id {
                return; // 已被取消（approach / 新一轮调度 / 关闭开关）
            }
            state.pending.store(false, Ordering::SeqCst);
            match lock_screen() {
                Ok(method) => {
                    *state.last_lock_ms.lock().unwrap() = now_ms();
                    logger::log(&app, INFO, format!("自动锁屏：已锁定屏幕（{method}）"));
                }
                Err(err) => logger::log(&app, ERROR, format!("自动锁屏：锁屏失败：{err}")),
            }
        });
    }

    /// 收到 approach：取消挂起中的延迟锁屏
    fn cancel_pending(&self, app: &AppHandle, device: &str) {
        if self.pending.swap(false, Ordering::SeqCst) {
            self.pending_gen.fetch_add(1, Ordering::SeqCst);
            logger::log(app, INFO, format!("自动锁屏：设备 {device} 检测到有人回来，已取消待执行的锁屏"));
        }
    }

    /// 关闭开关时取消挂起中的延迟锁屏
    pub fn cancel_all(&self, app: &AppHandle) {
        if self.pending.swap(false, Ordering::SeqCst) {
            self.pending_gen.fetch_add(1, Ordering::SeqCst);
            logger::log(app, INFO, "自动锁屏：功能已关闭，取消待执行的锁屏");
        }
    }
}

/// 解析 CSI 消息（纯逻辑，供测试）
pub fn parse_csi_event(raw: &str) -> Option<CsiMessage> {
    serde_json::from_str::<CsiMessage>(raw).ok()
}

/// 冷却窗口判断（纯逻辑，供测试）
pub fn in_cooldown(last_lock_ms: i64, now: i64) -> bool {
    now - last_lock_ms < LOCK_COOLDOWN_MS
}

/// 锁定屏幕，返回实际使用的锁定方式（供日志）
fn lock_screen() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        // 1) 系统「锁定屏幕」快捷键 Ctrl+Cmd+Q（需要辅助功能权限，失败则降级）
        let keystroke = std::process::Command::new("osascript")
            .args([
                "-e",
                r#"tell application "System Events" to keystroke "q" using {control down, command down}"#,
            ])
            .output()
            .map_err(|e| format!("osascript 启动失败：{e}"))?;
        if keystroke.status.success() {
            return Ok("系统快捷键 Ctrl+Cmd+Q".into());
        }
        // 2) 降级：立即息屏；系统设置「唤醒时需要密码」时等效锁屏（无需任何权限）
        let sleep_display = std::process::Command::new("pmset")
            .arg("displaysleepnow")
            .output()
            .map_err(|e| format!("pmset 启动失败：{e}"))?;
        if sleep_display.status.success() {
            return Ok("pmset displaysleepnow（等效锁屏，建议开启「唤醒时需要密码」）".into());
        }
        let mut err = String::from_utf8_lossy(&keystroke.stderr).trim().to_string();
        if err.is_empty() {
            err = String::from_utf8_lossy(&sleep_display.stderr).trim().to_string();
        }
        Err(format!("两种锁定方式均失败：{err}"))
    }
    #[cfg(target_os = "windows")]
    {
        let out = std::process::Command::new("rundll32.exe")
            .args(["user32.dll,LockWorkStation"])
            .output()
            .map_err(|e| format!("rundll32 启动失败：{e}"))?;
        if out.status.success() {
            Ok("LockWorkStation".into())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前平台不支持自动锁屏".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_csi_message() {
        let raw = r#"{"id":"441BF681C1F4","cmd":"csi","event":"leave","slope":-0.03,"amp":19.9,"rs":0.019,"frames":181}"#;
        let msg = parse_csi_event(raw).unwrap();
        assert_eq!(msg.id, "441BF681C1F4");
        assert_eq!(msg.event, "leave");
    }

    #[test]
    fn ignores_unparsable_message() {
        assert!(parse_csi_event("not json").is_none());
    }

    #[test]
    fn cools_down_within_window() {
        assert!(in_cooldown(1_000, 1_000 + 59_000));
        assert!(!in_cooldown(1_000, 1_000 + LOCK_COOLDOWN_MS));
    }

    #[test]
    fn cancel_generation_invalidates_pending_task() {
        let state = AutoLockState::default();
        let task_id = state.pending_gen.fetch_add(1, Ordering::SeqCst) + 1;
        state.pending.store(true, Ordering::SeqCst);
        // approach 取消
        assert!(state.pending.swap(false, Ordering::SeqCst));
        state.pending_gen.fetch_add(1, Ordering::SeqCst);
        // 定时任务醒来发现代号不一致，放弃锁屏
        assert_ne!(state.pending_gen.load(Ordering::SeqCst), task_id);
    }
}
