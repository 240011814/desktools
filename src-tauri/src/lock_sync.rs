//! 锁屏同步：本机锁屏 / 解锁事件经 MQTT（`locksync/<group号>`）与同组设备联动。
//!
//! - 收发分离：`lockSyncPublish`（广播我的锁屏 / 解锁状态）与 `lockSyncFollow`（跟随他人）
//!   是两个独立开关——工作机等「正在被使用的设备」可以只广播、不跟随，或者都不启用。
//! - 本机状态：轮询检测（约 1 秒；macOS `CGSessionCopyCurrentDictionary`，Windows `OpenInputDesktop`），
//!   仅在「未锁 → 已锁」/「已锁 → 未锁」跃迁时处理；启动 / 开关刚开启时以当前状态为基准，不补发旧事件。
//! - 真锁屏判定（广播端）：仅「锁定瞬间显示器仍点亮」的立即型锁定（SAC / Ctrl+Cmd+Q，
//!   立即要求密码）广播 lock；与显示器熄灭同刻的息屏型锁定（阶段一，密码延迟未到期）
//!   不广播，不让本机空闲息屏带动远端设备真锁屏；显示器状态探测失败 fail-open，
//!   按真锁屏广播（宁多广播、勿漏真锁）。**合盖例外**：本机盖子合着时息屏型锁定照常广播
//!   （盖子合上 = 人离开的确定信号，不是空闲息屏）。
//! - 配套对称：只有此前广播过 lock 的锁定会话，其后的「已锁 → 未锁」才广播 unlock；
//!   息屏型阶段的免密恢复不广播，避免无来源的解锁事件把远端屏幕点亮。
//! - 合盖标记：所有 locksync 消息携带 `lid` 字段 = 发送端发布时盖子是否合着；
//!   接收端按此豁免活跃守卫（合盖 = 发送侧的人已离开）。旧版本消息无该字段
//!   按 false（守卫照常），字段对旧版本接收端无副作用（serde 静默忽略未知字段）。
//! - 远端事件：`lock` → 复用 `auto_lock` 的锁屏链路锁定本机，但受**活跃守卫**约束
//!   （本机最近 `lockSyncIdleGuardSecs` 秒内有人键鼠输入则跳过；0 = 关闭守卫；探测失败
//!   fail-open：照常锁屏并记 WARN。**合盖豁免（两路）**：远端事件标记发送端合盖，
//!   或本机盖子合上 = 人不在的确定信号，均忽略守卫直接锁定）；`unlock` → 仅唤醒显示器到
//!   锁屏界面（远程无法、也不应绕过密码真正解锁）。
//! - 合盖信号探测（广播 / 跟随用；**Windows 不参与盖子探测**，本机盖子按未合处理，只消费远端消息里的合盖标记）：
//!   - macOS：每秒轮询 `IOPMrootDomain` 的 `AppleClamshellState`（dlopen IOKit，零权限）
//! - 合盖立即锁屏：检测到「开 → 合」跃迁且本机未锁 → 立即锁屏
//!   （合盖 ≠ 睡眠：外接显示器 clamshell 模式合盖照常运行，缺省不锁屏；对外广播由
//!   既有锁定跃迁链路接管，盖子合着时绕过息屏型判定直接广播）。
//! - 合盖补发：合盖时本机已锁（如息屏型从未广播过的会话、或手动锁定后延迟合盖）→
//!   补发一条 lock（盖子此刻合着，`lid` 必为 true），接收端凭合盖标记豁免活跃守卫
//!   跟随锁定；已锁接收端幂等；补发登记为已广播，其后解锁照常广播 unlock。
//! - 防回环：消息带 `deviceId`（自身消息忽略）；应用远端锁屏后进入发布抑制窗口
//!   （`REMOTE_SUPPRESS_MS`），本机随之发生的同状态变化不再发布，
//!   避免 A 锁 → B 锁 → B 又发布 → A 再锁 的回环。

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

/// MQTT 锁屏同步消息：`{"type":"lock"|"unlock","deviceId":"...","ts":...,"lid":false}`
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
    /// 发送端发布时盖子是否合着（接收端据此豁免活跃守卫：合盖 = 发送侧的人已离开）。
    /// 旧版本消息无该字段 → 默认 false（守卫照常）；旧版本接收端忽略未知字段
    #[serde(default)]
    pub lid: bool,
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

/// 解析出的远端事件
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteEvent {
    /// lock / unlock
    pub event: LockEvent,
    /// 发送端发布时盖子是否合着（豁免活跃守卫的依据；旧版本消息按 false）
    pub sender_lid_closed: bool,
}

/// 解析远端消息（纯逻辑，供测试）：
/// - 本机自己发布的（deviceId 相同）→ `Ok(None)`，忽略防回环
/// - JSON 解析失败 / 缺少 deviceId → `Err`（调用方记 WARN）
/// - 未知事件类型 → `Ok(None)`（向前兼容，静默忽略）
pub fn parse_remote_message(raw: &str, own_device_id: &str) -> Result<Option<RemoteEvent>, String> {
    let msg: LockSyncMessage =
        serde_json::from_str(raw).map_err(|e| format!("JSON 解析失败：{e}"))?;
    if msg.device_id.trim().is_empty() {
        return Err("缺少 deviceId".into());
    }
    if msg.device_id == own_device_id {
        return Ok(None);
    }
    let Some(event) = msg.event_kind() else {
        return Ok(None); // 未知事件类型：向前兼容，静默忽略
    };
    Ok(Some(RemoteEvent {
        event,
        sender_lid_closed: msg.lid,
    }))
}

/// 锁屏同步状态：远端事件应用后的发布抑制标记 + 广播端「lock 是否已发布」共享标记
pub struct LockSyncState {
    /// `Some((截止时间, 目标锁屏状态))`：窗口内本机发生的「同状态变化」判定为被动变化，不再发布
    suppress: Mutex<Option<(i64, bool)>>,
    /// 本机当前已锁会话的 lock 是否对外广播过：决定其 unlock 是否跟随广播（解锁对称），
    /// 也登记合盖补发。锁屏状态 watcher 与盖子 watcher 两个线程都要读写，故放共享状态
    lock_announced: Mutex<bool>,
}

impl Default for LockSyncState {
    fn default() -> Self {
        Self {
            suppress: Mutex::new(None),
            lock_announced: Mutex::new(false),
        }
    }
}

impl LockSyncState {
    /// 当前已锁会话的 lock 是否已对外广播（锁定跃迁发布或合盖补发时置位，解锁跃迁清除）
    fn lock_announced(&self) -> bool {
        *self.lock_announced.lock().unwrap()
    }

    /// 登记 / 清除「lock 已广播」标记（原锁定跃迁 watcher 内局部 announced 的共享化）
    fn set_lock_announced(&self, announced: bool) {
        *self.lock_announced.lock().unwrap() = announced;
    }

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
        if !cfg.lock_sync_follow {
            return; // 「跟随」开关关闭：不处理（防御；关闭时本就未订阅该 topic）
        }
        let remote = match parse_remote_message(raw, &cfg.device_id) {
            Ok(Some(remote)) => remote,
            Ok(None) => return,
            Err(err) => {
                logger::log(
                    app,
                    WARN,
                    format!(
                        "锁屏同步：忽略无法识别的消息（{err}）：{}",
                        logger::preview(raw)
                    ),
                );
                return;
            }
        };
        if remote.sender_lid_closed {
            logger::log(
                app,
                INFO,
                "锁屏同步：远端事件来自已合盖设备（视为人已离开）",
            );
        }
        match remote.event {
            LockEvent::Lock => {
                if is_screen_locked().unwrap_or(false) {
                    logger::log(app, INFO, "锁屏同步：收到远端锁屏事件，本机已处于锁屏状态");
                    return;
                }
                // 活跃守卫：有人正在本机操作时不用被远端事件锁掉，跳过（记 INFO 便于排查）。
                // 阈值 0 = 关闭守卫；探测失败 fail-open：照常锁屏、失败详情记 WARN。
                // 合盖豁免（两路，见 follow_lock_intercepts）：远端事件标记发送端合盖，
                // 或本机盖子已合上 = 人不在的确定信号，忽略之前设置的活跃守卫时间直接锁定
                //（本机盖子探测失败按「未合」处理，守卫照常生效）。
                let threshold = cfg.lock_sync_idle_guard_secs;
                let idle = input_idle_secs();
                let lid_closed = matches!(is_lid_closed(), Ok(true));
                if follow_lock_intercepts(
                    guard_blocks(&idle, threshold),
                    lid_closed,
                    remote.sender_lid_closed,
                ) {
                    // 这里 idle 必为 Ok（guard_blocks 只在 Ok 分支返回 true）
                    let idle_secs = idle.as_ref().unwrap_or(&0);
                    logger::log(
                        app,
                        INFO,
                        format!(
                            "锁屏同步：收到远端锁屏事件，已跳过——本机 {} 秒内有人操作（活跃守卫阈值 {} 秒）",
                            idle_secs, threshold
                        ),
                    );
                    return;
                }
                if let Err(err) = &idle {
                    logger::log(
                        app,
                        WARN,
                        format!("锁屏同步：活跃守卫探测失败（{err}），本次照常锁屏"),
                    );
                }
                if lid_closed {
                    logger::log(
                        app,
                        INFO,
                        "锁屏同步：收到远端锁屏事件，本机盖子已合上，忽略活跃守卫并锁定",
                    );
                }
                // 先标记抑制再执行锁屏：监听线程随后观察到的「已锁」跃迁不再对外发布
                // （已在锁定 / 守卫跳过的分支不会发生本机状态变化，无须标记）
                self.mark_remote_locked();
                logger::log(app, INFO, "锁屏同步：收到远端锁屏事件，正在锁定本机");
                match auto_lock::lock_screen(app) {
                    Ok(method) => {
                        logger::log(app, INFO, format!("锁屏同步：已锁定屏幕（{method}）"))
                    }
                    Err(err) => logger::log(app, ERROR, format!("锁屏同步：锁屏失败：{err}")),
                }
            }
            LockEvent::Unlock => {
                // 唤醒屏幕不会改变锁屏状态（仍停在锁屏界面），无需抑制发布
                logger::log(app, INFO, "锁屏同步：收到远端解锁事件，正在唤醒屏幕");
                match wake_display() {
                    Ok(detail) => {
                        logger::log(app, INFO, format!("锁屏同步：已唤醒屏幕（{detail}）"))
                    }
                    Err(err) => logger::log(app, WARN, format!("锁屏同步：唤醒屏幕失败：{err}")),
                }
            }
        }
    }
}

/// 活跃守卫判定（纯逻辑，供测试）：
/// - `idle`: `Ok(空闲秒数)` 为探测成功；探测失败 / 阈值为 0 都不拦截（fail-open：
///   锁屏联动优先，探测失败只记日志）——守卫的语义是「防误伤」而不是「门禁」
/// - 返回 true = 应跳过本次远端锁屏；false = 照常锁定
pub fn guard_blocks(idle: &Result<u64, String>, threshold_secs: u64) -> bool {
    if threshold_secs == 0 {
        return false; // 守卫关闭：总是跟随
    }
    match idle {
        Ok(idle_secs) => *idle_secs < threshold_secs,
        Err(_) => false, // 探测失败：偏向继续锁
    }
}

/// 跟随锁屏拦截判定（活跃守卫 + 合盖豁免，纯逻辑，供测试）：
/// - 守卫命中且两侧盖子都未合 → 拦截（有人可能正在本机操作）
/// - 本机盖子已合 → 永不拦截（盖子合上 = 人不在的确定信号，忽略之前设置的活跃守卫时间）
/// - 远端事件标记发送端盖子已合 → 永不拦截（发送侧的人已离开；本机阈值内的输入
///   只是离开前最后一次操作，不足以代表「正在使用」）
pub fn follow_lock_intercepts(
    guard_intercepts: bool,
    receiver_lid_closed: bool,
    sender_lid_closed: bool,
) -> bool {
    guard_intercepts && !receiver_lid_closed && !sender_lid_closed
}

/// 本机最近一次键鼠输入距今的空闲秒数（活跃守卫的输入源；探测失败返回 Err）。
/// 探测不出任何权限要求，属于只读的输入状态查询。
pub fn input_idle_secs() -> Result<u64, String> {
    #[cfg(target_os = "macos")]
    {
        macos_idle_secs()
    }
    #[cfg(target_os = "windows")]
    {
        windows_idle_secs()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前平台不支持输入空闲检测".into())
    }
}

/// macOS：`CGEventSourceSecondsSinceLastEventType(CombinedSession, AnyInput)`。
/// 只读取输入事件流的聚合空闲时长、不安装事件钩子，预期不需要辅助功能 /
/// Input Monitoring 权限（真机验证；异常时返回 Err，调用方按 fail-open 处理）
#[cfg(target_os = "macos")]
fn macos_idle_secs() -> Result<u64, String> {
    use std::ffi::c_void;

    /// CGEventSourceStateID::kCGEventSourceStateCombinedSessionState
    const COMBINED_SESSION: i32 = 0;
    /// kCGAnyInputEventType = ~0（任意输入事件类型）
    const ANY_INPUT: u32 = u32::MAX;

    type SinceLast = unsafe extern "C" fn(i32, u32) -> f64;

    /// dlopen 的框架句柄，Drop 时 dlclose（与 is_screen_locked 相同的模式，不泄漏引用计数）
    struct Framework(*mut c_void);
    impl Drop for Framework {
        fn drop(&mut self) {
            unsafe { libc::dlclose(self.0) };
        }
    }

    let cg = Framework(
        auto_lock::open_framework("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
            .ok_or("dlopen CoreGraphics 失败")?,
    );

    let since_last: SinceLast = unsafe {
        std::mem::transmute(
            auto_lock::dlsym_checked(cg.0, "CGEventSourceSecondsSinceLastEventType")
                .ok_or("缺少 CGEventSourceSecondsSinceLastEventType")?,
        )
    };

    let secs = unsafe { since_last(COMBINED_SESSION, ANY_INPUT) };
    if !secs.is_finite() {
        return Err(format!(
            "CGEventSourceSecondsSinceLastEventType 返回 {secs}"
        ));
    }
    Ok(if secs < 0.0 { 0 } else { secs as u64 })
}

/// Windows：`GetLastInputInfo`（无需任何权限）取最后一次输入的 tick，
/// 与当前 tick 做 49.7 天回绕安全的差值
#[cfg(target_os = "windows")]
fn windows_idle_secs() -> Result<u64, String> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    let ok = unsafe { GetLastInputInfo(&mut info) };
    if ok == 0 {
        return Err("GetLastInputInfo 失败".into());
    }
    let idle_ms = unsafe { GetTickCount() }.wrapping_sub(info.dwTime);
    Ok(u64::from(idle_ms / 1000))
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
        auto_lock::open_framework(
            "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation",
        )
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
        let get_type_id: GetTypeId = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFGetTypeID").ok_or("缺少 CFGetTypeID")?,
        );
        let bool_type_id: GetTypeIdNoArg = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFBooleanGetTypeID")
                .ok_or("缺少 CFBooleanGetTypeID")?,
        );
        let bool_get: BoolGetValue = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFBooleanGetValue").ok_or("缺少 CFBooleanGetValue")?,
        );
        let release: Release = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFRelease").ok_or("缺少 CFRelease")?,
        );

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
        let locked =
            !value.is_null() && get_type_id(value) == bool_type_id() && bool_get(value) != 0;
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

/* ---------------- 合盖状态探测（macOS；Windows 不做盖子探测） ---------------- */

/// 本机盖子是否处于合上状态（true = 已合盖）：
/// - macOS：dlopen IOKit 读 `IOPMrootDomain` 的 `AppleClamshellState`
///   （键存在 = 笔记本；CFBoolean true = 已合上。合盖 ≠ 睡眠，外接显示器
///   clamshell 模式合盖后系统照常运行，这正是需要该探测的原因）
/// - 其他平台（含 Windows）：不支持（盖子视为未合，守卫照常生效；Windows 机器
///   不参与盖子信号，只在收到远端消息携带发送端合盖标记时豁免守卫）
///
/// 零权限只读查询；桌面 Mac（iMac / Mac mini）无该键 → Err，调用方按未合盖处理。
pub fn is_lid_closed() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        macos_is_lid_closed()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("当前平台不支持盖子状态检测".into())
    }
}

/// macOS：IOKit 名称匹配 `IOPMrootDomain` 后读 `AppleClamshellState` 属性（CFBoolean）。
/// 注意 `IOServiceGetMatchingService` 会**消耗**传入的匹配字典（不另做 CFRelease）；
/// service / 键字符串 / 属性值各自 +1，用完逐个释放。与 macos_is_screen_locked 相同的
/// dlopen/dlsym 模式（每秒轮询期间不能泄漏引用计数）
#[cfg(target_os = "macos")]
fn macos_is_lid_closed() -> Result<bool, String> {
    use std::ffi::{c_char, c_void};

    type IOServiceNameMatchingFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    type IOServiceGetMatchingServiceFn = unsafe extern "C" fn(u32, *const c_void) -> u32;
    type IORegistryEntryCreateCFPropertyFn =
        unsafe extern "C" fn(u32, *const c_void, *const c_void, u32) -> *const c_void;
    type IOObjectReleaseFn = unsafe extern "C" fn(u32) -> i32;
    type StringCreate = unsafe extern "C" fn(*const c_void, *const c_char, u32) -> *const c_void;
    type GetTypeId = unsafe extern "C" fn(*const c_void) -> usize;
    type GetTypeIdNoArg = unsafe extern "C" fn() -> usize;
    type BoolGetValue = unsafe extern "C" fn(*const c_void) -> u8;
    type Release = unsafe extern "C" fn(*const c_void);

    /// kCFStringEncodingUTF8
    const UTF8: u32 = 0x0800_0100;

    /// dlopen 的框架句柄，Drop 时 dlclose（每秒轮询期间不能泄漏引用计数）
    struct Framework(*mut c_void);
    impl Drop for Framework {
        fn drop(&mut self) {
            unsafe { libc::dlclose(self.0) };
        }
    }

    let iokit = Framework(
        auto_lock::open_framework("/System/Library/Frameworks/IOKit.framework/IOKit")
            .ok_or("dlopen IOKit 失败")?,
    );
    let cf = Framework(
        auto_lock::open_framework(
            "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation",
        )
        .ok_or("dlopen CoreFoundation 失败")?,
    );

    unsafe {
        let name_matching: IOServiceNameMatchingFn = std::mem::transmute(
            auto_lock::dlsym_checked(iokit.0, "IOServiceNameMatching")
                .ok_or("缺少 IOServiceNameMatching")?,
        );
        let get_matching_service: IOServiceGetMatchingServiceFn = std::mem::transmute(
            auto_lock::dlsym_checked(iokit.0, "IOServiceGetMatchingService")
                .ok_or("缺少 IOServiceGetMatchingService")?,
        );
        let create_property: IORegistryEntryCreateCFPropertyFn = std::mem::transmute(
            auto_lock::dlsym_checked(iokit.0, "IORegistryEntryCreateCFProperty")
                .ok_or("缺少 IORegistryEntryCreateCFProperty")?,
        );
        let object_release: IOObjectReleaseFn = std::mem::transmute(
            auto_lock::dlsym_checked(iokit.0, "IOObjectRelease").ok_or("缺少 IOObjectRelease")?,
        );
        let string_create: StringCreate = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFStringCreateWithCString")
                .ok_or("缺少 CFStringCreateWithCString")?,
        );
        let get_type_id: GetTypeId = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFGetTypeID").ok_or("缺少 CFGetTypeID")?,
        );
        let bool_type_id: GetTypeIdNoArg = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFBooleanGetTypeID")
                .ok_or("缺少 CFBooleanGetTypeID")?,
        );
        let bool_get: BoolGetValue = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFBooleanGetValue").ok_or("缺少 CFBooleanGetValue")?,
        );
        let release: Release = std::mem::transmute(
            auto_lock::dlsym_checked(cf.0, "CFRelease").ok_or("缺少 CFRelease")?,
        );

        let dict = name_matching(c"IOPMrootDomain".as_ptr());
        if dict.is_null() {
            return Err("IOServiceNameMatching 失败".into());
        }
        // kIOMainPortDefault 为 0；匹配字典按文档被该调用消耗，无须再 CFRelease
        let service = get_matching_service(0, dict);
        if service == 0 {
            return Err("未找到 IOPMrootDomain".into());
        }
        let key = string_create(std::ptr::null(), c"AppleClamshellState".as_ptr(), UTF8);
        if key.is_null() {
            let _ = object_release(service);
            return Err("构造 AppleClamshellState 键失败".into());
        }
        let value = create_property(service, key, std::ptr::null(), 0);
        let result = if value.is_null() {
            // 键不存在 = 桌面 Mac（iMac / Mac mini），没有盖子
            Err("IOPMrootDomain 缺少 AppleClamshellState 键（非笔记本？）".into())
        } else if get_type_id(value) == bool_type_id() {
            // CFBoolean true = 已合盖
            Ok(bool_get(value) != 0)
        } else {
            Err("AppleClamshellState 值类型异常".into())
        };
        if !value.is_null() {
            release(value);
        }
        release(key);
        let _ = object_release(service);
        result
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
    let posted =
        unsafe { PostMessageW(HWND_BROADCAST, WM_SYSCOMMAND, SC_MONITORPOWER as usize, -1) };
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
            // 发送端盖子是否合着：接收端据此豁免活跃守卫（合盖 = 人已离开的确定信号）。
            // 探测失败（桌面 Mac / 台式机）按未合处理，不影响任何既有语义
            "lid": matches!(is_lid_closed(), Ok(true)),
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

/* ---------------- 真锁屏判定（广播端） ---------------- */

/// 本机显示器是否处于点亮状态（区分「显示器亮着的立即型真锁屏」与「显示器同步熄灭的
/// 息屏型阶段一锁定」）：
/// - macOS：`CGDisplayIsActive(kCGNullDirectDisplay)`（与 is_screen_locked 相同的
///   dlopen 模式，只读查询、无需任何权限）
/// - Windows：锁定语义本就是真锁屏（`LockWorkStation`），显示器状态无关，恒报点亮
pub fn display_is_active() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        macos_display_is_active()
    }
    #[cfg(target_os = "windows")]
    {
        Ok(true)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前平台不支持显示器状态检测".into())
    }
}

/// macOS：`CGDisplayIsActive(0)`，0 = kCGNullDirectDisplay，任一显示器亮即点亮。
/// 与 macos_idle_secs / macos_is_screen_locked 相同的 dlopen/dlsym 模式
#[cfg(target_os = "macos")]
fn macos_display_is_active() -> Result<bool, String> {
    use std::ffi::c_void;

    /// CGDisplayIsActive 返回 CoreGraphics 的 boolean_t（mach 下为 int）
    type IsActive = unsafe extern "C" fn(u32) -> i32;

    /// dlopen 的框架句柄，Drop 时 dlclose（与同文件其他探测相同的模式，不泄漏引用计数）
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

    let is_active: IsActive = unsafe {
        std::mem::transmute(
            auto_lock::dlsym_checked(cg.0, "CGDisplayIsActive").ok_or("缺少 CGDisplayIsActive")?,
        )
    };

    let active = unsafe { is_active(0) };
    Ok(active != 0)
}

/// 判定一次「未锁 → 已锁」跃迁是否属于真锁屏（纯逻辑，供测试）：
/// - `Ok(true)`：显示器还亮着的立即型锁定（SAC / Ctrl+Cmd+Q / 立即要求密码）→ 广播
/// - `Ok(false)`：显示器同步熄灭的息屏型锁定（阶段一，密码延迟未到期）→ 不广播
/// - `Err`：探测失败 fail-open，按真锁屏广播（宁多广播、勿漏真锁）
pub fn is_active_display_lock(display_active: &Result<bool, String>) -> bool {
    match display_active {
        Ok(active) => *active,
        Err(_) => true,
    }
}

/// 「未锁 → 已锁」跃迁是否广播 lock（真锁屏判定 + 合盖豁免，纯逻辑，供测试）：
/// - 显示器点亮的立即型锁定 → 广播
/// - 盖子已合 → 息屏型也广播（合盖 = 人离开的确定信号，「合盖 + 锁定」就是要带动
///   全组锁定的明确意图；盖子在合着的 clamshell 会话里息屏也不代表空闲）
/// - 其余息屏型锁定 → 不广播（空闲息屏不应联动远端真锁屏）
/// - 显示器状态探测失败 fail-open → 广播
pub fn should_broadcast_lock(display_active: &Result<bool, String>, lid_closed: bool) -> bool {
    lid_closed || is_active_display_lock(display_active)
}

/// 监听本机锁屏状态（轮询），状态跃迁时按真锁屏判定发布到 locksync/<group>：
/// - 启动 / 开关刚开启：先建立基准状态，不发布
/// - 应用远端事件引起的被动变化：由抑制窗口拦截，不发布
/// - 合盖触发的锁屏 / 盖子合着的锁定跃迁：广播 lock（绕过息屏型判定，见 should_broadcast_lock）
/// - 合盖时本机已锁：补发一次带合盖标记的 lock（「人离开」信号兜底，见 spawn_lid_watcher）
/// - 仅「锁定瞬间显示器仍点亮」的立即型真锁屏发布 lock，息屏型锁定不发布；
///   显示器状态探测失败 fail-open 视为真锁屏发布
/// - 只有此前广播过 lock 的会话随后才广播 unlock（息屏型阶段免密恢复不广播）
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
                let (publish, group, client) = {
                    let cfg = sync.config.lock().unwrap();
                    (cfg.lock_sync_publish, cfg.group.clone(), sync.client())
                };
                let lock_state = app.state::<LockSyncState>();
                if !publish || group.trim().is_empty() {
                    // 「广播」关闭 / 未配置：清除基准，下次开启时重新建立，不补发历史状态
                    baseline = None;
                    lock_state.set_lock_announced(false);
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
                            logger::log(
                                &app,
                                WARN,
                                format!("锁屏同步：检测本机锁屏状态失败：{err}"),
                            );
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
                        // 无论锁定/解锁跃迁都消费抑制标记（保持一次性语义，
                        // 防止远端锁屏窗口内被快速解锁后标记滞留在下一次跃迁）
                        let suppressed = lock_state.should_suppress(locked);
                        if locked {
                            // 广播端盖子状态：合着 → 息屏型锁定也广播（见 should_broadcast_lock）
                            let lid_closed = matches!(is_lid_closed(), Ok(true));
                            if suppressed {
                                lock_state.set_lock_announced(false);
                                logger::log(
                                    &app,
                                    INFO,
                                    "锁屏同步：本机状态变化由远端事件触发，跳过发布",
                                );
                            } else if !should_broadcast_lock(&display_is_active(), lid_closed) {
                                // 锁定与显示器熄灭同刻：息屏型阶段一（密码延迟未到期），不广播
                                lock_state.set_lock_announced(false);
                                logger::log(
                                    &app,
                                    INFO,
                                    "锁屏同步：息屏型锁定（显示器同步熄灭），不广播",
                                );
                            } else if let Some(client) = &client {
                                lock_state.set_lock_announced(true);
                                publish_state(&app, client, locked);
                            } else {
                                lock_state.set_lock_announced(false);
                                logger::log(
                                    &app,
                                    INFO,
                                    "锁屏同步：MQTT 未连接，本次状态变化未发布",
                                );
                            }
                        } else if let Some(client) = &client {
                            if lock_state.lock_announced() {
                                lock_state.set_lock_announced(false);
                                publish_state(&app, client, locked);
                            } else {
                                logger::log(
                                    &app,
                                    INFO,
                                    "锁屏同步：本机解锁（此前未广播锁定），跳过发布",
                                );
                            }
                        } else {
                            lock_state.set_lock_announced(false);
                            logger::log(&app, INFO, "锁屏同步：MQTT 未连接，本次状态变化未发布");
                        }
                    }
                }
            }
        });
}

/// 监听本机盖子状态（macOS），检测到「开 → 合」跃迁且本机未锁 → 立即锁屏：
/// - 合盖 ≠ 睡眠：外接显示器开启 clamshell 模式时合盖后系统照常运行、缺省不锁屏，
///   这里补上确定性锁屏（对外广播由 spawn_watcher 的锁定跃迁链路接管，
///   盖子合着时绕过息屏型判定直接广播）
/// - 状态源：macOS 每秒读 AppleClamshellState；Windows 不做盖子探测
///   （长期 Err，走去重后静默，仅消费远端消息里的发送端合盖标记）
/// - 门禁：广播 / 跟随任一开启才处理；两者都关时不处理、重置基准
/// - 已处于锁屏状态时合盖不重复锁（例如手动锁定后才合盖），但补发一条带合盖标记的
///   lock：锁定跃迁早已过去，未广播过 lock 的息屏型会话、以及手动锁定消息（lid=false）
///   曾被接收端活跃守卫跳过的设备，都靠这条补发跟锁；已锁接收端幂等，只记一行日志
/// - 仅跃迁触发：合盖后锁定失败不自动重试（盖子仍合着时每秒追踪重锁会与
///   外接显示器下用户的主动解锁打架），失败记得 ERROR 便于排查
#[cfg(desktop)]
pub fn spawn_lid_watcher(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("lock-sync-lid-watcher".into())
        .spawn(move || {
            // Some(已合盖)；None = 基准未建立（启动 / 开关刚开启 / 平台不支持）
            let mut baseline_closed: Option<bool> = None;
            let mut last_error: Option<String> = None;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS));

                let enabled = {
                    let sync = app.state::<crate::mqtt::SyncState>();
                    let cfg = sync.config.lock().unwrap();
                    cfg.lock_sync_publish || cfg.lock_sync_follow
                };
                if !enabled {
                    baseline_closed = None;
                    last_error = None;
                    continue;
                }

                let probe = is_lid_closed();
                let Some(closed) = (match probe {
                    Ok(closed) => {
                        if last_error.is_some() {
                            last_error = None;
                            logger::log(&app, INFO, "锁屏同步：盖子状态探测已恢复");
                        }
                        Some(closed)
                    }
                    Err(err) => {
                        // 仅原因变化时记 WARN，避免每秒刷日志；
                        // 台式机 / 非 Windows+macOS 平台的长期 Err 属预期，走去重后静默
                        if last_error.as_deref() != Some(err.as_str()) {
                            logger::log(&app, WARN, format!("锁屏同步：盖子状态探测失败：{err}"));
                            last_error = Some(err);
                        }
                        None
                    }
                }) else {
                    continue;
                };

                match baseline_closed {
                    None => baseline_closed = Some(closed),
                    Some(prev) if prev == closed => {}
                    Some(_) => {
                        baseline_closed = Some(closed);
                        if closed {
                            // 开 → 合：先查本机锁屏状态，未锁才执行锁屏
                            if is_screen_locked().unwrap_or(false) {
                                logger::log(&app, INFO, "锁屏同步：检测到合盖，本机已处于锁屏状态");
                                // 合盖 = 人离开的确定信号：本机不重复锁屏，但要把合盖补发给全组——
                                // 锁定跃迁早已过去，若当前会话的 lock 从未广播（息屏型不广播）、
                                // 或广播的消息 lid=false 而被接收端活跃守卫跳过（如手动锁屏后
                                // 延迟合盖），这条 lid=true 的 lock 是它们跟随锁定的机会
                                // （接收端凭合盖标记豁免守卫；已锁定的接收端幂等，只记日志）
                                let (publish, group, client) = {
                                    let sync = app.state::<crate::mqtt::SyncState>();
                                    let cfg = sync.config.lock().unwrap();
                                    (cfg.lock_sync_publish, cfg.group.clone(), sync.client())
                                };
                                if publish && !group.trim().is_empty() {
                                    match client {
                                        Some(client) => {
                                            logger::log(
                                                &app,
                                                INFO,
                                                "锁屏同步：合盖时本机已锁，补发带合盖标记的 lock",
                                            );
                                            app.state::<LockSyncState>().set_lock_announced(true);
                                            publish_state(&app, &client, true);
                                        }
                                        None => logger::log(
                                            &app,
                                            INFO,
                                            "锁屏同步：MQTT 未连接，本次合盖未补发",
                                        ),
                                    }
                                }
                                continue;
                            }
                            // 无须主动标记广播：锁定跃迁由监听线程发布时按盖子状态
                            // 豁息屏型判定（should_broadcast_lock），语义一致
                            logger::log(&app, INFO, "锁屏同步：检测到合盖，立即锁屏");
                            match auto_lock::lock_screen(&app) {
                                Ok(method) => logger::log(
                                    &app,
                                    INFO,
                                    format!("锁屏同步：合盖锁屏完成（{method}）"),
                                ),
                                Err(err) => logger::log(
                                    &app,
                                    ERROR,
                                    format!("锁屏同步：合盖锁屏失败：{err}"),
                                ),
                            }
                        }
                        // 合 → 开：无须处理
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
            lid: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"lock\""));
        assert!(json.contains("\"deviceId\":\"dev-1\""));
        assert!(json.contains("\"lid\":true"));
        let back: LockSyncMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event_kind(), Some(LockEvent::Lock));
        assert!(back.lid);
    }

    #[test]
    fn message_without_lid_defaults_false() {
        // 旧版本消息无 lid 字段：解析成功且按未合盖处理（守卫照常，向后兼容）
        let back: LockSyncMessage =
            serde_json::from_str(r#"{"type":"lock","deviceId":"other","ts":1}"#).unwrap();
        assert!(!back.lid);
        // 旧版本接收端忽略未知 lid 字段：仅消息里多一颗 key 也能正常读回
        let back: LockSyncMessage =
            serde_json::from_str(r#"{"type":"unlock","deviceId":"x","ts":1,"lid":true}"#).unwrap();
        assert_eq!(back.event_kind(), Some(LockEvent::Unlock));
    }

    #[test]
    fn parses_remote_lock_and_unlock() {
        let lock = r#"{"type":"lock","deviceId":"other","ts":1,"lid":true}"#;
        assert_eq!(
            parse_remote_message(lock, "me").unwrap(),
            Some(RemoteEvent {
                event: LockEvent::Lock,
                sender_lid_closed: true
            })
        );
        let unlock = r#"{"type":"unlock","deviceId":"other","ts":2}"#;
        assert_eq!(
            parse_remote_message(unlock, "me").unwrap(),
            Some(RemoteEvent {
                event: LockEvent::Unlock,
                sender_lid_closed: false
            })
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

    #[test]
    fn guard_blocks_recent_activity_only() {
        let probe: Result<u64, String> = Ok(10);
        // 10 秒前刚用过：拦截
        assert!(guard_blocks(&probe, 300));
        // 阈值边界：300 秒空闲恰好在阈值上，不算「有人操作」
        let at_threshold: Result<u64, String> = Ok(300);
        assert!(!guard_blocks(&at_threshold, 300));
        let idle_long: Result<u64, String> = Ok(60_000);
        assert!(!guard_blocks(&idle_long, 300));
    }

    #[test]
    fn guard_disabled_at_zero_threshold_and_fails_open() {
        let probe: Result<u64, String> = Ok(0); // 刚刚还在输入
        // 阈值 0 = 守卫关闭：总是跟随
        assert!(!guard_blocks(&probe, 0));
        // 探测失败 fail-open：不拦截
        let broken: Result<u64, String> = Err("权限被拒绝".into());
        assert!(!guard_blocks(&broken, 300));
    }

    #[test]
    fn lid_close_bypasses_active_guard() {
        // 本机盖子合上 = 无人使用：守卫命中也放行（忽略之前设置的活跃守卫时间）
        assert!(!follow_lock_intercepts(true, true, false));
        // 远端事件标记发送端盖子已合（发送侧的人已离开）：同样忽略守卫
        assert!(!follow_lock_intercepts(true, false, true));
        assert!(!follow_lock_intercepts(true, true, true));
        // 两侧盖子都没合：守卫保持原判定
        assert!(follow_lock_intercepts(true, false, false));
        // 守卫未命中：永不拦截
        assert!(!follow_lock_intercepts(false, false, false));
    }

    #[test]
    fn broadcast_lock_skips_display_check_when_lid_closed() {
        // 显示器亮着的立即型锁定：广播
        let on: Result<bool, String> = Ok(true);
        assert!(should_broadcast_lock(&on, false));
        // 息屏型（阶段一）：盖子没合不广播；盖子合着照常广播（合盖 = 人离开的确定信号）
        let off: Result<bool, String> = Ok(false);
        assert!(!should_broadcast_lock(&off, false));
        assert!(should_broadcast_lock(&off, true));
        // 探测失败 fail-open：按真锁屏广播
        let broken: Result<bool, String> = Err("dlopen CoreGraphics 失败".into());
        assert!(should_broadcast_lock(&broken, false));
        assert!(should_broadcast_lock(&broken, true));
    }

    /// 真实探测一次本机输入空闲时长：保证 dlopen/dlsym/FFI 链路不 panic（不校验具体值）
    #[test]
    fn real_idle_probe_does_not_panic() {
        let _ = input_idle_secs();
    }

    /// 真实探测一次本机锁屏状态：保证 dlopen/dlsym 链路不 panic（不校验具体值）
    #[test]
    fn real_detection_does_not_panic() {
        let _ = is_screen_locked();
    }

    /// 真实探测一次本机盖子状态：保证 IOKit 名称匹配 + 属性读取链路不 panic（不校验合未合）
    #[test]
    fn real_lid_probe_does_not_panic() {
        let _ = is_lid_closed();
    }

    #[test]
    fn classifies_real_lock_by_display_state() {
        // SAC / Ctrl+Cmd+Q / 立即密码：锁定瞬间显示器还亮 → 真锁屏，广播
        let on: Result<bool, String> = Ok(true);
        assert!(is_active_display_lock(&on));
        // 息屏型（阶段一）：锁定与显示器熄灭同刻 → 不广播
        let off: Result<bool, String> = Ok(false);
        assert!(!is_active_display_lock(&off));
        // 探测失败 fail-open：按真锁屏广播
        let broken: Result<bool, String> = Err("dlopen CoreGraphics 失败".into());
        assert!(is_active_display_lock(&broken));
    }

    /// 真实探测一次本机显示器状态：保证 CGDisplayIsActive 的 dlopen/dlsym 链路不 panic
    #[test]
    fn real_display_probe_does_not_panic() {
        let _ = display_is_active();
    }
}
