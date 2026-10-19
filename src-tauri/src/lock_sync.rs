//! 锁屏同步：本机锁屏 / 解锁事件经 MQTT（`locksync/<group号>`）通知同组其他设备。
//!
//! - 本机状态：轮询检测（约 1 秒；macOS `CGSessionCopyCurrentDictionary`，Windows `OpenInputDesktop`），
//!   仅在「未锁 → 已锁」/「已锁 → 未锁」跃迁时发布；启动 / 开关刚开启时以当前状态为基准，不补发旧事件。
//! - 远端事件：`lock` → 复用 `auto_lock` 的锁屏链路锁定本机；`unlock` → 仅唤醒显示器到锁屏界面
//!   （远程无法、也不应绕过密码真正解锁）。
//! - 防回环：消息带 `deviceId`（自身消息忽略）；应用远端锁屏后进入发布抑制窗口
//!   （`REMOTE_SUPPRESS_MS`），本机随之发生的同状态变化不再发布，避免
//!   A 锁 → B 锁 → B 又发布 → A 再锁 的回环。

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::auto_lock;
use crate::history::now_ms;
use crate::logger::{self, ERROR, INFO, WARN};

/// 锁屏同步 topic 前缀：locksync/<group号>
pub const TOPIC_PREFIX: &str = "locksync";

/// 本机锁屏状态轮询间隔（仅桌面端使用）
#[cfg(desktop)]
const POLL_MS: u64 = 1_000;

/// 应用远端锁屏事件后的发布抑制窗口：
/// 覆盖「监听线程一轮轮询 + 锁屏指令执行」的耗时，防止被动锁屏被当成新事件再发出去
const REMOTE_SUPPRESS_MS: i64 = 5_000;

/// 构造同步 topic（group 首尾空白容错）
pub fn topic(group: &str) -> String {
    format!("{TOPIC_PREFIX}/{}", group.trim())
}

/// 锁屏事件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockEvent {
    /// 锁屏：其他设备跟随锁定
    Lock,
    /// 解锁：其他设备唤醒屏幕到锁屏界面
    Unlock,
}

/// MQTT 锁屏同步消息：`{"type":"lock"|"unlock","deviceId":"...","ts":...}`
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockSyncMessage {
    /// lock / unlock
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub ts: i64,
}

impl LockSyncMessage {
    /// 事件类型映射（未知类型返回 None，向前兼容）
    pub fn event_kind(&self) -> Option<LockEvent> {
        match self.kind.as_str() {
            "lock" => Some(LockEvent::Lock),
            "unlock" => Some(LockEvent::Unlock),
            _ => None,
        }
    }
}

/// 解析远端消息（纯逻辑，供测试）：
/// - 本机自己发布的（deviceId 相同）→ `Ok(None)`，忽略防回环
/// - JSON 解析失败 / 缺少 deviceId → `Err`（调用方记 WARN）
/// - 未知事件类型 → `Ok(None)`（向前兼容，静默忽略）
pub fn parse_remote_message(raw: &str, own_device_id: &str) -> Result<Option<LockEvent>, String> {
    let msg: LockSyncMessage =
        serde_json::from_str(raw).map_err(|e| format!("JSON 解析失败：{e}"))?;
    if msg.device_id.trim().is_empty() {
        return Err("缺少 deviceId".into());
    }
    if msg.device_id == own_device_id {
        return Ok(None);
    }
    Ok(msg.event_kind())
}

/// 锁屏同步状态：远端事件应用后的发布抑制标记
pub struct LockSyncState {
    /// `Some((截止时间, 目标锁屏状态))`：窗口内本机发生的「同状态变化」判定为被动变化，不再发布
    suppress: Mutex<Option<(i64, bool)>>,
}

impl Default for LockSyncState {
    fn default() -> Self {
        Self {
            suppress: Mutex::new(None),
        }
    }
}

impl LockSyncState {
    /// 应用远端锁屏事件后标记：抑制本机随之发生的「已锁」变化被再次发布
    fn mark_remote_locked(&self) {
        *self.suppress.lock().unwrap() = Some((now_ms() + REMOTE_SUPPRESS_MS, true));
    }

    /// 判定并消费抑制标记：true = 本次状态变化由远端事件引起，应该抑制发布
    fn should_suppress(&self, locked: bool) -> bool {
        self.should_suppress_at(locked, now_ms())
    }

    fn should_suppress_at(&self, locked: bool, now: i64) -> bool {
        let mut suppress = self.suppress.lock().unwrap();
        match *suppress {
            Some((until, state)) => {
                // 一次性标记：无论是否命中都消费掉（状态不匹配时更不能留到下一次跃迁）
                *suppress = None;
                state == locked && now <= until
            }
            None => false,
        }
    }

    /// 处理一条远端锁屏同步消息（由消息处理线程调用）
    pub fn handle_event(&self, app: &AppHandle, raw: &str) {
        let cfg = app
            .state::<crate::mqtt::SyncState>()
            .config
            .lock()
            .unwrap()
            .clone();
        if !cfg.lock_sync {
            return; // 开关关闭：不处理（防御；关闭后本就未订阅该 topic）
        }
        let event = match parse_remote_message(raw, &cfg.device_id) {
            Ok(Some(event)) => event,
            Ok(None) => return,
            Err(err) => {
                logger::log(
                    app,
                    WARN,
                    format!("锁屏同步：忽略无法识别的消息（{err}）：{}", logger::preview(raw)),
                );
                return;
            }
        };
        match event {
            LockEvent::Lock => {
                // 先标记抑制再执行锁屏：监听线程随后观察到的「已锁」跃迁不再对外发布
                self.mark_remote_locked();
                if is_screen_locked().unwrap_or(false) {
                    logger::log(app, INFO, "锁屏同步：收到远端锁屏事件，本机已处于锁屏状态");
                    return;
                }
                logger::log(app, INFO, "锁屏同步：收到远端锁屏事件，正在锁定本机");
                match auto_lock::lock_screen(app) {
                    Ok(method) => logger::log(app, INFO, format!("锁屏同步：已锁定屏幕（{method}）")),
                    Err(err) => logger::log(app, ERROR, format!("锁屏同步：锁屏失败：{err}")),
                }
            }
            LockEvent::Unlock => {
                // 唤醒屏幕不会改变锁屏状态（仍停在锁屏界面），无需抑制发布
                logger::log(app, INFO, "锁屏同步：收到远端解锁事件，正在唤醒屏幕");
                match wake_display() {
                    Ok(detail) => logger::log(app, INFO, format!("锁屏同步：已唤醒屏幕（{detail}）")),
                    Err(err) => logger::log(app, WARN, format!("锁屏同步：唤醒屏幕失败：{err}")),
                }
            }
        }
    }
}

/// 本机屏幕是否处于锁定状态：
/// - macOS：`CGSessionCopyCurrentDictionary` 的 `CGSSessionScreenIsLocked` 键
/// - Windows：锁屏后输入桌面被 Winlogon 安全桌面接管，`OpenInputDesktop` 以拒绝访问失败
pub fn is_screen_locked() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        macos_is_screen_locked()
    }
    #[cfg(target_os = "windows")]
    {
        windows_is_screen_locked()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前平台不支持锁屏状态检测".into())
    }
}

/// 唤醒显示器到锁屏界面（不绕过密码真正解锁）
fn wake_display() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        // caffeinate -u：声明用户活动 1 秒，系统因此点亮屏幕（仍停在锁屏界面）
        auto_lock::run_and_check("/usr/bin/caffeinate", &["-u", "-t", "1"])?;
        Ok("caffeinate 用户活动声明".into())
    }
    #[cfg(target_os = "windows")]
    {
        windows_wake_display()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前平台不支持唤醒屏幕".into())
    }
}

/// macOS：CGSessionCopyCurrentDictionary（CoreGraphics）返回当前会话信息字典，
/// 键 CGSSessionScreenIsLocked 为 kCFBooleanTrue 时表示屏幕已锁定。
/// 键不存在 / 值不是 CFBoolean 都按「未锁定」处理。
#[cfg(target_os = "macos")]
fn macos_is_screen_locked() -> Result<bool, String> {
    use std::ffi::{c_char, c_void};

    type SessionDict = unsafe extern "C" fn() -> *const c_void;
    type StringCreate = unsafe extern "C" fn(*const c_void, *const c_char, u32) -> *const c_void;
    type DictGetValue = unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_void;
    type GetTypeId = unsafe extern "C" fn(*const c_void) -> usize;
    type GetTypeIdNoArg = unsafe extern "C" fn() -> usize;
    type BoolGetValue = unsafe extern "C" fn(*const c_void) -> u8;
    type Release = unsafe extern "C" fn(*const c_void);

    /// kCFStringEncodingUTF8
    const UTF8: u32 = 0x0800_0100;

    /// dlopen 的框架句柄，Drop 时 dlclose（轮询期间不能泄漏引用计数）
    struct Framework(*mut c_void);
    impl Drop for Framework {
        fn drop(&mut self) {
            unsafe { libc::dlclose(self.0) };
        }
    }

    let cg = Framework(
        auto_lock::open_framework(
            "/System/Library/Frameworks/ApplicationServices.framework/Frameworks/CoreGraphics.framework/CoreGraphics",
        )
        .ok_or("dlopen CoreGraphics 失败")?,
    );
    let cf = Framework(
        auto_lock::open_framework("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
            .ok_or("dlopen CoreFoundation 失败")?,
    );

    unsafe {
        let session_dict: SessionDict = std::mem::transmute(
            auto_lock::dlsym_checked(cg.0, "CGSessionCopyCurrentDictionary")
                .ok_or("缺少 CGSessionCopyCurrentDictionary")?,
        );
        let string_create: StringCreate = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFStringCreateWithCString")
                .ok_or("缺少 CFStringCreateWithCString")?,
        );
        let dict_get: DictGetValue = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFDictionaryGetValue")
                .ok_or("缺少 CFDictionaryGetValue")?,
        );
        let get_type_id: GetTypeId =
            std::mem::transmute(auto_lock::dlsym_checked(cf.0, "CFGetTypeID").ok_or("缺少 CFGetTypeID")?);
        let bool_type_id: GetTypeIdNoArg = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFBooleanGetTypeID").ok_or("缺少 CFBooleanGetTypeID")?,
        );
        let bool_get: BoolGetValue = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFBooleanGetValue").ok_or("缺少 CFBooleanGetValue")?,
        );
        let release: Release =
            std::mem::transmute(auto_lock::dlsym_checked(cf.0, "CFRelease").ok_or("缺少 CFRelease")?);

        let dict = session_dict();
        if dict.is_null() {
            return Err("CGSessionCopyCurrentDictionary 返回空（无图形会话？）".into());
        }
        let key = string_create(std::ptr::null(), c"CGSSessionScreenIsLocked".as_ptr(), UTF8);
        if key.is_null() {
            release(dict);
            return Err("构造 CGSSessionScreenIsLocked 键失败".into());
        }
        let value = dict_get(dict, key);
        let locked = !value.is_null() && get_type_id(value) == bool_type_id() && bool_get(value) != 0;
        release(key);
        release(dict);
        Ok(locked)
    }
}

/// Windows：DESKTOP_SWITCHDESKTOP 权限打开输入桌面，锁屏时安全桌面（Winlogon）接管、返回拒绝访问
#[cfg(target_os = "windows")]
fn windows_is_screen_locked() -> Result<bool, String> {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, GetLastError};
    use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop};

    /// DESKTOP_SWITCHDESKTOP：请求切换到输入桌面的权限
    const DESKTOP_SWITCHDESKTOP: u32 = 0x0100;

    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_SWITCHDESKTOP) };
    if desktop.is_null() {
        let err = unsafe { GetLastError() };
        if err == ERROR_ACCESS_DENIED {
            Ok(true)
        } else {
            Err(format!("OpenInputDesktop 失败（错误码 {err}）"))
        }
    } else {
        unsafe { CloseDesktop(desktop) };
        Ok(false)
    }
}

/// Windows：广播「打开显示器」命令 + 短暂声明显示需求再释放（部分系统上单用其一无效）
#[cfg(target_os = "windows")]
fn windows_wake_display() -> Result<String, String> {
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, SetThreadExecutionState,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, PostMessageW, SC_MONITORPOWER, WM_SYSCOMMAND,
    };

    // WM_SYSCOMMAND 只取 wParam 低 16 位：SC_MONITORPOWER + lParam(-1) = 打开显示器
    let posted = unsafe { PostMessageW(HWND_BROADCAST, WM_SYSCOMMAND, SC_MONITORPOWER as usize, -1) };
    let previous = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED) };
    unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    if posted != 0 || previous != 0 {
        Ok("SC_MONITORPOWER + ES_DISPLAY_REQUIRED".into())
    } else {
        Err("PostMessage(SC_MONITORPOWER) 与 SetThreadExecutionState 均失败".into())
    }
}

/// 发布本机锁屏状态（订阅端按 deviceId 忽略自身消息）
#[cfg(desktop)]
fn publish_state(app: &AppHandle, client: &rumqttc::AsyncClient, locked: bool) {
    let (topic, payload) = {
        let sync = app.state::<crate::mqtt::SyncState>();
        let cfg = sync.config.lock().unwrap();
        let payload = serde_json::json!({
            "type": if locked { "lock" } else { "unlock" },
            "deviceId": cfg.device_id,
            "ts": now_ms(),
        })
        .to_string();
        (topic(&cfg.group), payload)
    };
    let client = client.clone();
    tauri::async_runtime::spawn(async move {
        let _ = client
            .publish(topic, rumqttc::QoS::AtLeastOnce, false, payload)
            .await;
    });
    logger::log(
        app,
        INFO,
        format!(
            "锁屏同步：本机已{}，已通知同组其他设备",
            if locked { "锁定" } else { "解锁" }
        ),
    );
}

/// 监听本机锁屏状态（轮询），状态跃迁时发布到 locksync/<group>：
/// - 启动 / 开关刚开启：先建立基准状态，不发布
/// - 应用远端事件引起的被动变化：由抑制窗口拦截，不发布
/// - MQTT 未连接：不发布（事件即时性内容不做补发，重连后也不回放旧状态）
#[cfg(desktop)]
pub fn spawn_watcher(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("lock-sync-watcher".into())
        .spawn(move || {
            let mut baseline: Option<bool> = None;
            let mut last_error: Option<String> = None;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS));

                let sync = app.state::<crate::mqtt::SyncState>();
                let (enabled, group, client) = {
                    let cfg = sync.config.lock().unwrap();
                    (cfg.lock_sync, cfg.group.clone(), sync.client())
                };
                if !enabled || group.trim().is_empty() {
                    // 关闭 / 未配置：清除基准，下次开启时重新建立，不补发历史状态
                    baseline = None;
                    continue;
                }

                let locked = match is_screen_locked() {
                    Ok(locked) => {
                        last_error = None;
                        locked
                    }
                    Err(err) => {
                        // 检测失败：仅在原因变化时记 WARN，避免每秒刷日志
                        if last_error.as_deref() != Some(err.as_str()) {
                            logger::log(&app, WARN, format!("锁屏同步：检测本机锁屏状态失败：{err}"));
                            last_error = Some(err);
                        }
                        continue;
                    }
                };

                match baseline {
                    None => baseline = Some(locked),
                    Some(prev) if prev == locked => {}
                    Some(_) => {
                        baseline = Some(locked);
                        if app.state::<LockSyncState>().should_suppress(locked) {
                            logger::log(&app, INFO, "锁屏同步：本机状态变化由远端事件触发，跳过发布");
                        } else if let Some(client) = &client {
                            publish_state(&app, client, locked);
                        } else {
                            logger::log(&app, INFO, "锁屏同步：MQTT 未连接，本次状态变化未发布");
                        }
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_uses_trimmed_group() {
        assert_eq!(topic(" my-group "), "locksync/my-group");
    }

    #[test]
    fn message_serde_uses_type_field() {
        // 钉住线上格式：发布端 json! 与接收端字段名/类型必须一致
        let msg = LockSyncMessage {
            kind: "lock".into(),
            device_id: "dev-1".into(),
            ts: 123,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"lock\""));
        assert!(json.contains("\"deviceId\":\"dev-1\""));
        let back: LockSyncMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event_kind(), Some(LockEvent::Lock));
    }

    #[test]
    fn parses_remote_lock_and_unlock() {
        let lock = r#"{"type":"lock","deviceId":"other","ts":1}"#;
        assert_eq!(parse_remote_message(lock, "me").unwrap(), Some(LockEvent::Lock));
        let unlock = r#"{"type":"unlock","deviceId":"other","ts":2}"#;
        assert_eq!(
            parse_remote_message(unlock, "me").unwrap(),
            Some(LockEvent::Unlock)
        );
    }

    #[test]
    fn ignores_own_messages() {
        let raw = r#"{"type":"lock","deviceId":"me","ts":1}"#;
        assert_eq!(parse_remote_message(raw, "me").unwrap(), None);
    }

    #[test]
    fn rejects_malformed_and_unknown() {
        // 非 JSON / 缺 deviceId 都视为不可识别
        assert!(parse_remote_message("not json", "me").is_err());
        assert!(parse_remote_message(r#"{"type":"lock","ts":1}"#, "me").is_err());
        // 未知类型向前兼容：忽略但不报错
        let raw = r#"{"type":"ping","deviceId":"other","ts":1}"#;
        assert_eq!(parse_remote_message(raw, "me").unwrap(), None);
    }

    #[test]
    fn suppression_is_one_shot_within_window() {
        let state = LockSyncState::default();
        state
            .suppress
            .lock()
            .unwrap()
            .replace((1_000 + REMOTE_SUPPRESS_MS, true));
        // 窗口边界内、同状态 → 抑制
        assert!(state.should_suppress_at(true, 1_000 + REMOTE_SUPPRESS_MS));
        // 标记是一次性的：再次判定不再抑制
        assert!(!state.should_suppress_at(true, 1_000 + REMOTE_SUPPRESS_MS));
    }

    #[test]
    fn suppression_expires_and_clears_on_mismatch() {
        let state = LockSyncState::default();
        state
            .suppress
            .lock()
            .unwrap()
            .replace((1_000 + REMOTE_SUPPRESS_MS, true));
        // 超窗 → 不抑制
        assert!(!state.should_suppress_at(true, 1_000 + REMOTE_SUPPRESS_MS + 1));

        // 状态不匹配 → 不抑制，且标记被消费（不能留到下一次跃迁）
        state
            .suppress
            .lock()
            .unwrap()
            .replace((2_000 + REMOTE_SUPPRESS_MS, true));
        assert!(!state.should_suppress_at(false, 2_000));
        assert!(!state.should_suppress_at(true, 2_000));
    }

    /// 真实探测一次本机锁屏状态：保证 dlopen/dlsym 链路不 panic（不校验具体值）
    #[test]
    fn real_detection_does_not_panic() {
        let _ = is_screen_locked();
    }
}
