use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
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
            match lock_screen(&app) {
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

/// 锁定屏幕，返回实际使用的锁定方式（供日志）。lock_sync 远端锁屏事件同样复用此链路。
/// 链路：系统锁屏接口（真锁屏，无需权限）→ Ctrl+Cmd+Q（需辅助功能）→ pmset 息屏（需「唤醒时需要密码」），
/// 逐级降级，每一级的失败原因都记 WARN（便于排查"为什么没锁住"）。
pub(crate) fn lock_screen(app: &AppHandle) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let mut errors = Vec::new();
        match sac_lock_screen() {
            Ok(()) => return Ok("系统锁屏接口 SACLockScreenImmediate".into()),
            Err(err) => {
                logger::log(
                    app,
                    WARN,
                    format!("自动锁屏：系统锁屏接口不可用（{err}），尝试 Ctrl+Cmd+Q 快捷键"),
                );
                errors.push(err);
            }
        }
        // Ctrl+Cmd+Q 系统「锁定屏幕」快捷键（需要辅助功能权限）
        match run_and_check("osascript", &[
            "-e",
            r#"tell application "System Events" to keystroke "q" using {control down, command down}"#,
        ]) {
            Ok(()) => return Ok("系统快捷键 Ctrl+Cmd+Q".into()),
            Err(err) => {
                logger::log(
                    app,
                    WARN,
                    format!("自动锁屏：Ctrl+Cmd+Q 快捷键失败（{err}），降级为息屏"),
                );
                errors.push(format!("Ctrl+Cmd+Q：{err}"));
            }
        }
        // 最后降级：立即息屏；系统设置「唤醒时需要密码」时等效锁屏（无需任何权限）
        match run_and_check("pmset", &["displaysleepnow"]) {
            Ok(()) => {
                return Ok("pmset displaysleepnow 息屏（等效锁屏，建议开启「唤醒时需要密码」）".into())
            }
            Err(err) => {
                logger::log(app, WARN, format!("自动锁屏：息屏降级失败（{err}）"));
                errors.push(format!("pmset 息屏：{err}"));
            }
        }
        Err(format!("三种锁定方式均失败：{}", errors.join("；")))
    }
    #[cfg(target_os = "windows")]
    {
        let _ = app;
        match run_and_check("rundll32.exe", &["user32.dll,LockWorkStation"]) {
            Ok(()) => Ok("LockWorkStation".into()),
            Err(err) => Err(format!("LockWorkStation 失败：{err}")),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        Err("当前平台不支持自动锁屏".into())
    }
}

/// 执行命令并要求退出成功，失败时带上 stderr（供日志诊断；lock_sync 唤醒屏幕复用）
pub(crate) fn run_and_check(program: &str, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program} 启动失败：{e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let mut err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if err.is_empty() {
            err = format!("退出码 {}", out.status);
        }
        Err(err)
    }
}

/// macOS 系统锁屏接口（LoginUIKit 私有框架）：真正的锁屏界面、必须输密码，且无需任何 TCC 权限。
/// 私有接口可能随系统更新变化，因此调用失败时由上层降级到快捷键 / 息屏。
#[cfg(target_os = "macos")]
fn sac_lock_screen() -> Result<(), String> {
    let (handle, lock_screen) = sac_load()?;
    unsafe { lock_screen() };
    unsafe { libc::dlclose(handle) };
    Ok(())
}

/// 探测 SACLockScreenImmediate 是否可用：dlopen + dlsym，只检查不调用（不会锁屏）
#[cfg(target_os = "macos")]
fn sac_probe() -> Result<(), String> {
    let (handle, _) = sac_load()?;
    unsafe { libc::dlclose(handle) };
    Ok(())
}

/// 打开 LoginUIKit 并取出 SACLockScreenImmediate（调用方负责调用与 dlclose）
#[cfg(target_os = "macos")]
fn sac_load() -> Result<(*mut libc::c_void, unsafe extern "C" fn()), String> {
    unsafe {
        let path = std::ffi::CString::new(
            "/System/Library/PrivateFrameworks/LoginUIKit.framework/LoginUIKit",
        )
        .map_err(|e| e.to_string())?;
        let handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if handle.is_null() {
            return Err("dlopen LoginUIKit 失败".into());
        }
        let Some(func) = dlsym_checked(handle, "SACLockScreenImmediate") else {
            libc::dlclose(handle);
            return Err("dlsym SACLockScreenImmediate 失败".into());
        };
        Ok((handle, std::mem::transmute(func)))
    }
}

/* ---------------- 锁屏能力自检（macOS） ---------------- */

/// 自动化权限：desktools 控制「系统事件」是否被允许
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(target_os = "macos")]
pub enum AutomationPermission {
    /// 已授权
    Granted,
    /// 已被用户在系统设置中拒绝
    Denied,
    /// 尚未询问（首次触发时会弹窗）
    NotAsked,
    /// 无法确定
    Unknown,
}

/// 「进入睡眠/唤醒后需要密码」设置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(target_os = "macos")]
pub enum ScreenLockSetting {
    /// 立即要求密码
    Immediate,
    /// N 秒后才要求密码
    AfterSeconds(u64),
    /// 关闭：唤醒不需要密码
    Off,
    /// 无法读取
    Unknown,
}

/// 自检的原始探测结果（纯数据，供 build_report 单元测试）
#[derive(Debug, Clone)]
#[cfg(target_os = "macos")]
pub struct SelfCheckInput {
    pub sac_available: bool,
    pub sac_error: Option<String>,
    pub accessibility: bool,
    pub automation: AutomationPermission,
    pub screen_lock: ScreenLockSetting,
}

/// 锁屏能力自检报告（启动日志与设置页共用）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockReport {
    /// 平台：macos / windows / other
    pub platform: String,
    /// 结论：当前会使用的锁屏方式
    pub summary: String,
    /// 修复建议（空串 = 无需处理）
    pub advice: String,
    /// 是否有「锁不住 / 只能降级」的风险
    pub degraded: bool,
    /// 逐项探测明细
    pub details: Vec<String>,
}

/// 执行锁屏能力自检：非破坏性（不锁屏、不弹窗、不修改任何设置）
pub fn self_check() -> LockReport {
    #[cfg(target_os = "macos")]
    {
        let sac_error = sac_probe().err();
        build_report(&SelfCheckInput {
            sac_available: sac_error.is_none(),
            sac_error,
            accessibility: accessibility_trusted(),
            automation: automation_permission(),
            screen_lock: screen_lock_setting(),
        })
    }
    #[cfg(target_os = "windows")]
    {
        LockReport {
            platform: "windows".into(),
            summary: "当前将使用：LockWorkStation（无需额外权限）".into(),
            advice: String::new(),
            degraded: false,
            details: vec!["锁定方式：LockWorkStation".into()],
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        LockReport {
            platform: "other".into(),
            summary: "当前平台不支持自动锁屏".into(),
            advice: String::new(),
            degraded: true,
            details: Vec::new(),
        }
    }
}

/// 辅助功能权限（静默检测：AXIsProcessTrusted，不弹窗）
#[cfg(target_os = "macos")]
fn accessibility_trusted() -> bool {
    let Some(handle) =
        open_framework("/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices")
    else {
        return false;
    };
    let trusted = unsafe {
        match dlsym_checked(handle, "AXIsProcessTrusted") {
            Some(func) => {
                let is_trusted: unsafe extern "C" fn() -> u8 = std::mem::transmute(func);
                is_trusted() != 0
            }
            None => false,
        }
    };
    unsafe { libc::dlclose(handle) };
    trusted
}

/// 自动化权限（能否控制 System Events）：静默检测（askUserIfNeeded=false，不弹窗）
#[cfg(target_os = "macos")]
fn automation_permission() -> AutomationPermission {
    use std::ffi::c_void;

    /// 'bund'：地址描述符类型 = 应用 Bundle ID
    const TYPE_APPLICATION_BUNDLE_ID: u32 = 0x6275_6E64;
    /// '****'：事件类别 / 事件 ID 通配
    const TYPE_WILD_CARD: u32 = 0x2A2A_2A2A;
    /// errAEEventNotPermitted：已被拒绝
    const ERR_EVENT_NOT_PERMITTED: i32 = -1743;
    /// errAEEventWouldRequireUserConsent：尚未询问
    const ERR_EVENT_WOULD_REQUIRE_CONSENT: i32 = -1744;
    /// 探测目标（快捷键降级路径依赖它合成按键）
    const TARGET_BUNDLE_ID: &str = "com.apple.systemevents";

    #[repr(C)]
    #[allow(dead_code)] // 字段由 AECreateDesc 通过 FFI 写入，Rust 侧不直接读取
    struct Aedesc {
        descriptor_type: u32,
        data_handle: *mut c_void,
    }

    type CreateDesc = unsafe extern "C" fn(u32, *const c_void, usize, *mut Aedesc) -> i32;
    type DisposeDesc = unsafe extern "C" fn(*mut Aedesc) -> i32;
    type DeterminePermission = unsafe extern "C" fn(*const Aedesc, u32, u32, u8) -> i32;

    let Some(handle) =
        open_framework("/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices")
    else {
        return AutomationPermission::Unknown;
    };
    let result = unsafe {
        let (Some(create), Some(determine)) = (
            dlsym_checked(handle, "AECreateDesc"),
            dlsym_checked(handle, "AEDeterminePermissionToAutomateTarget"),
        ) else {
            libc::dlclose(handle);
            return AutomationPermission::Unknown;
        };
        let create: CreateDesc = std::mem::transmute(create);
        let determine: DeterminePermission = std::mem::transmute(determine);
        let mut desc = Aedesc {
            descriptor_type: TYPE_APPLICATION_BUNDLE_ID,
            data_handle: std::ptr::null_mut(),
        };
        let created = create(
            TYPE_APPLICATION_BUNDLE_ID,
            TARGET_BUNDLE_ID.as_ptr() as *const c_void,
            TARGET_BUNDLE_ID.len(),
            &mut desc,
        );
        let permission = if created == 0 {
            let status = determine(&desc, TYPE_WILD_CARD, TYPE_WILD_CARD, 0);
            if let Some(dispose) = dlsym_checked(handle, "AEDisposeDesc") {
                let dispose: DisposeDesc = std::mem::transmute(dispose);
                dispose(&mut desc);
            }
            match status {
                0 => AutomationPermission::Granted,
                ERR_EVENT_NOT_PERMITTED => AutomationPermission::Denied,
                ERR_EVENT_WOULD_REQUIRE_CONSENT => AutomationPermission::NotAsked,
                _ => AutomationPermission::Unknown,
            }
        } else {
            AutomationPermission::Unknown
        };
        libc::dlclose(handle);
        permission
    };
    result
}

/// 读取「进入睡眠后需要密码」设置（sysadminctl，静默，不需要管理员权限）
#[cfg(target_os = "macos")]
fn screen_lock_setting() -> ScreenLockSetting {
    match std::process::Command::new("/usr/sbin/sysadminctl")
        .args(["-screenLock", "status"])
        .output()
    {
        Ok(out) => {
            // sysadminctl 把结果写到 stderr，stdout/stderr 都拼接后解析
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            parse_screen_lock_output(&text)
        }
        Err(_) => ScreenLockSetting::Unknown,
    }
}

/// 解析 sysadminctl -screenLock status 的输出（纯逻辑，供测试）
#[cfg(target_os = "macos")]
pub fn parse_screen_lock_output(raw: &str) -> ScreenLockSetting {
    if raw.contains("screenLock delay is immediate") {
        return ScreenLockSetting::Immediate;
    }
    if raw.contains("screenLock is off") {
        return ScreenLockSetting::Off;
    }
    if let Some(rest) = raw.split("screenLock delay is ").nth(1) {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            if let Ok(secs) = digits.parse::<u64>() {
                return ScreenLockSetting::AfterSeconds(secs);
            }
        }
    }
    ScreenLockSetting::Unknown
}

/// 由探测结果生成自检报告（纯逻辑，供测试）
#[cfg(target_os = "macos")]
fn build_report(input: &SelfCheckInput) -> LockReport {
    let keystroke_ok =
        input.accessibility && input.automation == AutomationPermission::Granted;
    let screen_lock_text = match input.screen_lock {
        ScreenLockSetting::Immediate => "立即".to_string(),
        ScreenLockSetting::AfterSeconds(secs) => format!("{secs} 秒后"),
        ScreenLockSetting::Off => "关闭".to_string(),
        ScreenLockSetting::Unknown => "未知".to_string(),
    };

    let details = vec![
        match &input.sac_error {
            None => "系统锁屏接口（SACLockScreenImmediate）：可用".to_string(),
            Some(err) => format!("系统锁屏接口（SACLockScreenImmediate）：不可用（{err}）"),
        },
        format!(
            "辅助功能权限：{}",
            if input.accessibility { "已授权" } else { "未授权" }
        ),
        format!(
            "自动化权限（控制 System Events）：{}",
            match input.automation {
                AutomationPermission::Granted => "已授权",
                AutomationPermission::Denied => "被拒绝",
                AutomationPermission::NotAsked => "尚未询问",
                AutomationPermission::Unknown => "未知",
            }
        ),
        format!("进入睡眠后需要密码：{screen_lock_text}"),
    ];

    let mut advice = Vec::new();
    let (summary, degraded) = if input.sac_available {
        (
            "当前将使用：系统锁屏接口（真锁屏，无需任何权限）".to_string(),
            false,
        )
    } else if keystroke_ok {
        (
            "当前将使用：Ctrl+Cmd+Q 系统快捷键（系统锁屏接口不可用）".to_string(),
            false,
        )
    } else {
        let fallback_ok = matches!(
            input.screen_lock,
            ScreenLockSetting::Immediate | ScreenLockSetting::AfterSeconds(_)
        );
        if fallback_ok {
            (
                "当前将使用：息屏降级（pmset displaysleepnow），可能不是真正锁屏".to_string(),
                true,
            )
        } else {
            (
                "没有可靠的锁屏方式，息屏降级也不会要求密码".to_string(),
                true,
            )
        }
    };

    if !input.sac_available {
        if !input.accessibility {
            advice.push("在「系统设置 → 隐私与安全性 → 辅助功能」中勾选 desktools");
        }
        match input.automation {
            AutomationPermission::Granted => {}
            AutomationPermission::Denied => {
                advice.push("在「系统设置 → 隐私与安全性 → 自动化」中允许 desktools 控制「系统事件」")
            }
            AutomationPermission::NotAsked => {
                advice.push("首次触发锁屏时按系统弹窗允许 desktools 控制「系统事件」")
            }
            AutomationPermission::Unknown => {
                advice.push("无法确定自动化权限，可在「系统设置 → 隐私与安全性 → 自动化」中检查")
            }
        }
        if !keystroke_ok {
            match input.screen_lock {
                ScreenLockSetting::Immediate => {}
                ScreenLockSetting::AfterSeconds(_) => advice
                    .push("建议将「进入睡眠后需要密码」改为「立即」（当前息屏后延迟才需要密码）"),
                ScreenLockSetting::Off => {
                    advice.push("在「系统设置 → 锁定屏幕」中开启「进入睡眠后需要密码」")
                }
                ScreenLockSetting::Unknown => {
                    advice.push("无法读取「进入睡眠后需要密码」设置，请手动确认")
                }
            }
        }
    }

    LockReport {
        platform: "macos".into(),
        summary,
        advice: advice.join("；"),
        degraded,
        details,
    }
}

/// 打开系统框架（dlopen），失败返回 None（lock_sync 锁屏状态检测复用）
#[cfg(target_os = "macos")]
pub(crate) fn open_framework(path: &str) -> Option<*mut libc::c_void> {
    let path = std::ffi::CString::new(path).ok()?;
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
    if handle.is_null() { None } else { Some(handle) }
}

/// dlsym 取符号，失败返回 None（lock_sync 锁屏状态检测复用）
#[cfg(target_os = "macos")]
pub(crate) fn dlsym_checked(handle: *mut libc::c_void, symbol: &str) -> Option<*mut libc::c_void> {
    let symbol = std::ffi::CString::new(symbol).ok()?;
    let func = unsafe { libc::dlsym(handle, symbol.as_ptr()) };
    if func.is_null() { None } else { Some(func) }
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

#[cfg(all(test, target_os = "macos"))]
mod self_check_tests {
    use super::*;

    fn input(
        sac: bool,
        accessibility: bool,
        automation: AutomationPermission,
        screen_lock: ScreenLockSetting,
    ) -> SelfCheckInput {
        SelfCheckInput {
            sac_available: sac,
            sac_error: if sac { None } else { Some("dlsym 失败".into()) },
            accessibility,
            automation,
            screen_lock,
        }
    }

    #[test]
    fn parses_sysadminctl_output() {
        assert_eq!(
            parse_screen_lock_output(
                "2026-10-16 09:19:28.050 sysadminctl[21762:2618636] screenLock delay is 300 seconds"
            ),
            ScreenLockSetting::AfterSeconds(300)
        );
        assert_eq!(
            parse_screen_lock_output("screenLock delay is immediate"),
            ScreenLockSetting::Immediate
        );
        assert_eq!(parse_screen_lock_output("screenLock is off"), ScreenLockSetting::Off);
        assert_eq!(parse_screen_lock_output(""), ScreenLockSetting::Unknown);
        assert_eq!(parse_screen_lock_output("garbage"), ScreenLockSetting::Unknown);
    }

    #[test]
    fn sac_available_needs_no_permission() {
        let report = build_report(&input(
            true,
            false,
            AutomationPermission::Denied,
            ScreenLockSetting::Off,
        ));
        assert!(!report.degraded);
        assert!(report.advice.is_empty());
        assert!(report.summary.contains("系统锁屏接口"));
    }

    #[test]
    fn keystroke_available_is_real_lock() {
        let report = build_report(&input(
            false,
            true,
            AutomationPermission::Granted,
            ScreenLockSetting::Off,
        ));
        assert!(!report.degraded);
        assert!(report.summary.contains("Ctrl+Cmd+Q"));
    }

    #[test]
    fn degraded_without_permissions_or_password() {
        let report = build_report(&input(
            false,
            false,
            AutomationPermission::Denied,
            ScreenLockSetting::Off,
        ));
        assert!(report.degraded);
        assert!(report.advice.contains("辅助功能"));
        assert!(report.advice.contains("自动化"));
        assert!(report.advice.contains("需要密码"));
    }

    #[test]
    fn delayed_password_is_degraded() {
        let report = build_report(&input(
            false,
            false,
            AutomationPermission::NotAsked,
            ScreenLockSetting::AfterSeconds(300),
        ));
        assert!(report.degraded);
        assert!(report.summary.contains("息屏降级"));
        assert!(report.advice.contains("立即"));
        // 明细里带上每一项探测结果
        assert_eq!(report.details.len(), 4);
        assert!(report.details[3].contains("300 秒后"));
    }

    /// 真实执行一次自检（dlopen/dlsym + ApplicationServices + sysadminctl，全部非破坏性），
    /// 保证在本机 / CI 上探测链路不 panic、也能拿到结构完整的报告。
    #[test]
    fn real_self_check_produces_full_report() {
        let report = self_check();
        assert_eq!(report.platform, "macos");
        assert_eq!(report.details.len(), 4);
        assert!(!report.summary.is_empty());
    }
}
