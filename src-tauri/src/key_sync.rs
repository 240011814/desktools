//! 按键同步：一台设备按下配置的按键组合后，经 MQTT（`keysync/<group号>`）通知同组
//! 设备注入映射后的目标组合（例：Windows 按 Ctrl+↓ → macOS 按 F9）。
//!
//! 设计要点：
//! - 只同步「规则里配置的组合」，不做全键盘流转发；总开关（`keySyncEnabled`）默认关闭。
//! - 规则模型：`KeySyncRule { id, enabled, source, target, platform, note }`，
//!   `source/target` 是「修饰键 + 命名主键」的组合，platform 指明该映射适用的接收平台。
//!   发送端命中规则后广播 `source + target + platform`（`ruleId` 仅用于日志）。
//!   接收端做两级匹配：
//!   1) 本机启用规则里存在同源组合且其平台含本机 → 注入**本机规则**的目标
//!      （每台设备可映射到不同目标键；平台不符记 WARN 忽略，不回退远端映射）；
//!   2) 本机没有同源规则且消息携带合法 target → 按**发送端映射**直接注入目标组合
//!      （单向配置即生效；仅当消息 platform 含本机时执行，缺省 / 非法 platform 忽略）。
//! - 组合必须精确匹配：修饰键多按 / 少按都不命中；修饰键自身不并入主键；同一
//!   (主键, 修饰键) 只有一条规则命中（同主键多个组合按修饰键数降序取最具体者）。
//! - 发送端：全局键盘钩子（Windows `WH_KEYBOARD_LL` / macOS listen-only `CGEventTap`）
//!   在后台与失焦时仍生效；命中后同一组合 300ms 内去重；注入的按键（本机或他人
//!   SendInput / CGEventPost 产生）不再次广播，防多设备回环。
//! - 接收端：忽略自身 `deviceId`；MQTT 断线只记 INFO 不补发（按键事件讲时效，不做重放）。
//! - macOS 权限以 `CGPreflightListenEventAccess / CGPreflightPostEventAccess` 预检为准，
//!   不调用 CGRequest 弹安全面板；无权限时总开关回退关闭并在 UI 明确提示。

use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::config::MqttConfig;
use crate::history::now_ms;
use crate::logger::{self, ERROR, INFO, WARN};
use crate::mqtt::SyncState;

/// 按键同步 topic 前缀：keysync/<group号>
pub const TOPIC_PREFIX: &str = "keysync";

/// 发送 / 注入防重窗（毫秒）：同一组合 300ms 内只处理一次（防长按残漏与回环）
pub const DEDUPE_MS: i64 = 300;

/// MQTT 未连接时的提示限频（毫秒），避免按住组合时刷日志
const OFFLINE_NOTICE_MS: i64 = 5_000;

/* ---------------- 组合模型 / 键名归一（纯逻辑） ---------------- */

/// 修饰键位掩码（与 KeyCombo 的布尔字段一一对应）
pub const MOD_CTRL: u8 = 1;
pub const MOD_ALT: u8 = 2;
pub const MOD_SHIFT: u8 = 4;
pub const MOD_META: u8 = 8;

/// 目标平台取值：Windows / macOS / 全部
pub const PLATFORM_WINDOWS: &str = "windows";
pub const PLATFORM_MACOS: &str = "macos";
pub const PLATFORM_ALL: &str = "all";

/// 键组合：修饰键 + 命名主键（主键必填）。
/// 键名一律用规范化 camelCase 存储（`pageUp` 等），跨平台按物理键位对应
/// （Windows VK 码 / macOS 虚拟键码都是物理键），与输入法布局无关。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct KeyCombo {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    /// Super / Win / Cmd
    #[serde(default)]
    pub meta: bool,
    /// 规范化命名主键（字母 / 数字 / f1~f12 / up / down / left / right / pageUp / pageDown / home / end）
    #[serde(default)]
    pub key: String,
}

/// 修饰键种类（解析修饰键名用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModKind {
    Ctrl,
    Alt,
    Shift,
    Meta,
}

impl KeyCombo {
    /// 修饰键掩码
    pub fn mask(&self) -> u8 {
        let mut mask = 0;
        if self.ctrl {
            mask |= MOD_CTRL;
        }
        if self.alt {
            mask |= MOD_ALT;
        }
        if self.shift {
            mask |= MOD_SHIFT;
        }
        if self.meta {
            mask |= MOD_META;
        }
        mask
    }

    /// 规范化字符串（消息里传输的「源组合」）：修饰键固定顺序 + 主键，如 `ctrl+alt+down`
    pub fn canonical(&self) -> String {
        canonical_from(&self.key, self.mask())
    }
}

/// 由「主键 + 修饰键掩码」构造规范化组合串（钩子命中路径与规则路径共用，保证键一致）
pub fn canonical_from(key: &str, mask: u8) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if mask & MOD_CTRL != 0 {
        parts.push("ctrl");
    }
    if mask & MOD_ALT != 0 {
        parts.push("alt");
    }
    if mask & MOD_SHIFT != 0 {
        parts.push("shift");
    }
    if mask & MOD_META != 0 {
        parts.push("super");
    }
    parts.push(key);
    parts.join("+")
}

/// 按平台显示组合（Windows 用 Win、macOS 用 Cmd/Option、其余用 Super）
pub fn display_combo(combo: &KeyCombo, os: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if combo.ctrl {
        parts.push("Ctrl".into());
    }
    if combo.alt {
        parts.push(
            if os == PLATFORM_MACOS {
                "Option"
            } else {
                "Alt"
            }
            .into(),
        );
    }
    if combo.shift {
        parts.push("Shift".into());
    }
    if combo.meta {
        parts.push(
            match os {
                PLATFORM_MACOS => "Cmd",
                PLATFORM_WINDOWS => "Win",
                _ => "Super",
            }
            .into(),
        );
    }
    parts.push(display_key(&combo.key));
    parts.join("+")
}

/// 主键显示名（方向键用箭头符号，字母 / F 系列转大写）
pub fn display_key(key: &str) -> String {
    match key {
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "pageUp" => "PageUp".into(),
        "pageDown" => "PageDown".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "enter" => "Enter".into(),
        other => {
            let bytes = other.as_bytes();
            if bytes.len() == 1 && bytes[0].is_ascii_lowercase()
                || bytes.len() >= 2
                    && bytes[0] == b'f'
                    && bytes[1..].iter().all(|b| b.is_ascii_digit())
            {
                return other.to_ascii_uppercase();
            }
            other.to_string()
        }
    }
}

/// 主键名归一化：仅支持字母 / 数字 / F1~F12 / 方向 / PageUp / PageDown / Home / End / Enter。
/// 接受常见别名（ArrowUp、Page Up、PgUp、↓、Return、NumpadEnter 等），返回 camelCase 规范名；
/// 其余按键（Space / 标点等）不支持，返回 None。
pub fn normalize_key_name(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // 箭头 / 回车写法（录制输入 / 手动输入容错）
    match raw {
        "↑" => return Some("up".into()),
        "↓" => return Some("down".into()),
        "←" => return Some("left".into()),
        "→" => return Some("right".into()),
        "⏎" | "↵" => return Some("enter".into()),
        _ => {}
    }
    let folded = raw.replace(' ', "").to_ascii_lowercase();
    // 单字符：字母 / 数字
    let mut chars = folded.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            return Some(c.to_string());
        }
        return None; // 单字符的非字母数字（标点等）不支持
    }
    // F1 ~ F12
    if let Some(num) = folded.strip_prefix('f') {
        if let Ok(n) = num.parse::<u8>()
            && (1..=12).contains(&n)
        {
            return Some(format!("f{n}"));
        }
        return None;
    }
    // 方向 / 翻页 / Home / End / Enter（ArrowUp / PageUp / PgUp / Page Up 均可）
    let stripped = folded.strip_prefix("arrow").unwrap_or(&folded);
    match stripped {
        "up" => Some("up".into()),
        "down" => Some("down".into()),
        "left" => Some("left".into()),
        "right" => Some("right".into()),
        "pageup" | "pgup" => Some("pageUp".into()),
        "pagedown" | "pgdown" | "pgdwn" | "pgdn" => Some("pageDown".into()),
        "home" => Some("home".into()),
        "end" => Some("end".into()),
        "enter" | "return" | "numpadenter" => Some("enter".into()),
        _ => None,
    }
}

/// 修饰键名归一化（Ctrl / Alt / Shift / Super·Win·Cmd）
fn normalize_modifier(raw: &str) -> Option<ModKind> {
    let folded = raw.replace(' ', "").to_ascii_lowercase();
    match folded.as_str() {
        "ctrl" | "control" | "⌃" => Some(ModKind::Ctrl),
        "alt" | "option" | "opt" | "⌥" => Some(ModKind::Alt),
        "shift" | "⇧" => Some(ModKind::Shift),
        "super" | "meta" | "win" | "windows" | "cmd" | "command" | "⌘" => Some(ModKind::Meta),
        _ => None,
    }
}

/// 解析组合文本（手动编辑 / 录制回填统一入口）：`Ctrl+Shift+↓` / `alt+f9` / `Cmd+Down`
pub fn parse_combo(input: &str) -> Result<KeyCombo, String> {
    let parts: Vec<&str> = input
        .trim()
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("按键组合为空".into());
    }
    let (main_raw, mod_parts) = parts.split_last().unwrap();
    let mut combo = KeyCombo::default();
    for part in mod_parts {
        match normalize_modifier(part) {
            Some(ModKind::Ctrl) => combo.ctrl = true,
            Some(ModKind::Alt) => combo.alt = true,
            Some(ModKind::Shift) => combo.shift = true,
            Some(ModKind::Meta) => combo.meta = true,
            None => {
                return Err(format!(
                    "无法识别的修饰键「{part}」（支持 Ctrl / Alt / Shift / Win·Cmd，用 + 分隔）"
                ));
            }
        }
    }
    match normalize_key_name(main_raw) {
        Some(key) => combo.key = key,
        None => {
            return Err(format!(
                "无法识别的主键「{main_raw}」（支持字母 / 数字 / F1~F12 / 方向键 / PageUp / PageDown / Home / End / Enter）"
            ));
        }
    }
    Ok(combo)
}

/// 校验（结构化）组合：主键必须存在且已是规范化写法
pub fn combo_error(combo: &KeyCombo) -> Result<(), String> {
    if combo.key.is_empty() {
        return Err(
            "组合缺少主键（需为字母 / 数字 / F1~F12 / 方向键 / PageUp / PageDown / Home / End / Enter）"
                .into(),
        );
    }
    match normalize_key_name(&combo.key) {
        Some(canonical) if canonical == combo.key => Ok(()),
        Some(canonical) => Err(format!(
            "主键写法不规范，应为「{}」",
            display_key(&canonical)
        )),
        None => Err(format!(
            "主键「{}」不受支持（需为字母 / 数字 / F1~F12 / 方向键 / PageUp / PageDown / Home / End / Enter）",
            combo.key
        )),
    }
}

/* ---------------- 规则 ---------------- */

/// 按键同步规则：源组合（触发）→ 目标组合（注入）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeySyncRule {
    /// 规则 id（前端生成 uuid，用于日志与标识）
    pub id: String,
    /// 是否启用（禁用的规则不参与匹配，也允许保留未填写 / 非法的组合）
    #[serde(default)]
    pub enabled: bool,
    /// 源组合：任一同组设备按下后触发（本机钩子同样监听）
    #[serde(default)]
    pub source: KeyCombo,
    /// 目标组合：本机命中的规则注入这个组合
    #[serde(default)]
    pub target: KeyCombo,
    /// 目标平台：windows / macos / all（本机平台不含其中时忽略并记 WARN）
    #[serde(default = "default_rule_platform")]
    pub platform: String,
    /// 备注（可选）
    #[serde(default)]
    pub note: String,
}

fn default_rule_platform() -> String {
    PLATFORM_ALL.into()
}

/// 校验一批规则：平台字段必须合法；启用的规则组合必须完整合法（禁用的可不填）
pub fn validate_rules(rules: &[KeySyncRule]) -> Result<(), String> {
    for (index, rule) in rules.iter().enumerate() {
        let label = rule_label(rule, index);
        if !matches!(
            rule.platform.as_str(),
            PLATFORM_WINDOWS | PLATFORM_MACOS | PLATFORM_ALL
        ) {
            return Err(format!(
                "规则 {label}：目标平台需为「全部 / 仅 Windows / 仅 macOS」"
            ));
        }
        if !rule.enabled {
            continue;
        }
        combo_error(&rule.source).map_err(|e| format!("规则 {label}：源组合无效：{e}"))?;
        combo_error(&rule.target).map_err(|e| format!("规则 {label}：目标组合无效：{e}"))?;
    }
    Ok(())
}

/// 错误信息里的规则定位：优先备注，其次序号
fn rule_label(rule: &KeySyncRule, index: usize) -> String {
    let note = rule.note.trim();
    if note.is_empty() {
        format!("第 {} 条", index + 1)
    } else {
        format!("「{note}」")
    }
}

/// 平台串是否为合法取值（windows / macos / all）
pub fn is_valid_platform(platform: &str) -> bool {
    matches!(platform, PLATFORM_WINDOWS | PLATFORM_MACOS | PLATFORM_ALL)
}

/// 平台取值是否包含本机
pub fn platform_applies_str(platform: &str, os: &str) -> bool {
    match platform {
        PLATFORM_WINDOWS => os == PLATFORM_WINDOWS,
        PLATFORM_MACOS => os == PLATFORM_MACOS,
        _ => true,
    }
}

/// 规则的目标平台是否包含本机
pub fn platform_applies(rule: &KeySyncRule, os: &str) -> bool {
    platform_applies_str(&rule.platform, os)
}

/// 本机平台标识（与规则 platform 字段对应）
pub fn os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        PLATFORM_WINDOWS
    } else if cfg!(target_os = "macos") {
        PLATFORM_MACOS
    } else {
        "other"
    }
}

/* ---------------- 活跃规则索引（纯逻辑） ---------------- */

/// 启用规则的按下匹配索引：主键 → （修饰键掩码, 规则下标）。
/// 同一 (主键, 修饰键) 只有一条规则命中；同主键多个掩码按修饰键数降序排列，
/// 修饰键必须与按下状态完全一致才算命中（多按 / 少按都不触发）。
#[derive(Debug, Default, Clone)]
pub struct ActiveRules {
    rules: Vec<KeySyncRule>,
    by_key: HashMap<String, Vec<(u8, usize)>>,
}

impl ActiveRules {
    /// 从全部规则构建（仅保留启用且源组合合法的规则）
    pub fn build(rules: &[KeySyncRule]) -> Self {
        let active: Vec<KeySyncRule> = rules
            .iter()
            .filter(|r| r.enabled && combo_error(&r.source).is_ok())
            .cloned()
            .collect();
        let mut by_key: HashMap<String, Vec<(u8, usize)>> = HashMap::new();
        for (index, rule) in active.iter().enumerate() {
            by_key
                .entry(rule.source.key.clone())
                .or_default()
                .push((rule.source.mask(), index));
        }
        for bucket in by_key.values_mut() {
            // 修饰键数多的组合排前（最具体者优先）；重复 (键, 掩码) 保持定义顺序，先定义者胜出
            bucket.sort_by_key(|(mask, index)| (std::cmp::Reverse(mask.count_ones()), *index));
        }
        Self {
            rules: active,
            by_key,
        }
    }

    /// 按「主键 + 修饰键掩码」精确查找命中的规则
    pub fn lookup(&self, key: &str, mask: u8) -> Option<&KeySyncRule> {
        let bucket = self.by_key.get(key)?;
        let &(_, index) = bucket.iter().find(|(m, _)| *m == mask)?;
        self.rules.get(index)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/* ---------------- 共享状态（发送去重 / 注入抑制 / 掉线提示限频） ---------------- */

/// 按键同步运行状态：managed 状态，钩子回调线程与 MQTT 消息线程共用
pub struct KeySyncState {
    /// 最近已发布的源组合 → 时间：300ms 内的同键重复（长按 / 多设备同键）不再发布
    published: Mutex<HashMap<String, i64>>,
    /// 本机注入过的组合 → 时间：300ms 内钩子再次看到该组合时不发布（防回环）
    injected: Mutex<HashMap<String, i64>>,
    /// 最近一次「MQTT 未连接」提示时间（限频）
    offline_notice: Mutex<i64>,
    /// 是否已提示过「辅助功能未授权」（注入权限预检误报时不刷屏）
    post_warned: AtomicBool,
}

impl Default for KeySyncState {
    fn default() -> Self {
        Self {
            published: Mutex::new(HashMap::new()),
            injected: Mutex::new(HashMap::new()),
            offline_notice: Mutex::new(0),
            post_warned: AtomicBool::new(false),
        }
    }
}

/// 仅保留窗口内的条目，防止长时间运行把哈希表撑大
fn prune_window(map: &mut HashMap<String, i64>, now: i64) {
    map.retain(|_, ts| now - *ts <= DEDUPE_MS);
}

impl KeySyncState {
    /// 钩子命中后能否发布：300ms 内已发布过 / 本机刚注入过该组合 → false
    pub fn may_publish(&self, combo: &str) -> bool {
        self.may_publish_at(combo, now_ms())
    }

    fn may_publish_at(&self, combo: &str, now: i64) -> bool {
        if self.recently_injected_at(combo, now) {
            return false;
        }
        let mut published = self.published.lock().unwrap();
        prune_window(&mut published, now);
        !published.iter().any(|(text, _)| text == combo)
    }

    /// 记录一次发布（发布动作实际发起后调用）
    pub fn record_published(&self, combo: &str) {
        self.record_published_at(combo, now_ms());
    }

    fn record_published_at(&self, combo: &str, now: i64) {
        let mut published = self.published.lock().unwrap();
        prune_window(&mut published, now);
        published.insert(combo.to_string(), now);
    }

    /// 本机注入目标组合：标记 300ms 抑制（在注入前调用，防注入被钩子再次广播）
    pub fn mark_injected(&self, combo: &str) {
        self.mark_injected_at(combo, now_ms());
    }

    fn mark_injected_at(&self, combo: &str, now: i64) {
        let mut injected = self.injected.lock().unwrap();
        prune_window(&mut injected, now);
        injected.insert(combo.to_string(), now);
    }

    fn recently_injected_at(&self, combo: &str, now: i64) -> bool {
        let mut injected = self.injected.lock().unwrap();
        prune_window(&mut injected, now);
        injected.iter().any(|(text, _)| text == combo)
    }

    /// 「MQTT 未连接」提示限频：返回 true 表示本次允许提示
    fn take_offline_notice(&self) -> bool {
        let now = now_ms();
        let mut last = self.offline_notice.lock().unwrap();
        if now - *last < OFFLINE_NOTICE_MS {
            return false;
        }
        *last = now;
        true
    }

    /// 是否应记录一次「辅助功能（注入）权限未授予」提示：每个应用生命周期仅一次
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn take_post_permission_warning(&self) -> bool {
        !self.post_warned.swap(true, Ordering::Relaxed)
    }
}

/* ---------------- MQTT 消息 ---------------- */

/// 构造按键同步 topic（group 首尾空白容错）
pub fn topic(group: &str) -> String {
    format!("{TOPIC_PREFIX}/{}", group.trim())
}

/// MQTT 按键同步消息：
/// `{"deviceId":"...","ruleId":"...","source":"ctrl+down","target":{...},"platform":"macos","ts":...}`
/// `target / platform` 描述发送端规则的映射（接收端无同源本地规则时按它注入，单向生效）；
/// 旧版本消息缺这两个字段：`target` 缺省为空组合 → 仅本机规则可命中，行为向后兼容。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySyncMessage {
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub rule_id: String,
    /// 规范化的源组合串
    #[serde(default)]
    pub source: String,
    /// 发送端规则的目标组合（结构化）
    #[serde(default)]
    pub target: KeyCombo,
    /// 发送端规则的目标平台（windows / macos / all；非法 / 缺省时按空处理）
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub ts: i64,
}

/// 解析后的远端按键同步消息（供接收匹配）
#[derive(Debug, Clone)]
pub struct RemoteKeySync {
    /// 远端按下的源组合
    pub source: KeyCombo,
    /// 消息携带的发送端目标组合（缺省 / 非法为 None）
    pub target: Option<KeyCombo>,
    /// 发送端规则的目标平台（缺省 / 非法为空串 → 不做远端直注）
    pub platform: String,
}

/// 解析远端消息（纯逻辑，供测试）：
/// - 本机自己发布的 → `Ok(None)`（防回环）
/// - JSON 解析失败 / 缺 deviceId / 源组合无效 → `Err`（调用方记 WARN）
/// - `target / platform` 非法时宽容降级（None / 空串），不影响本机规则匹配
pub fn parse_remote_message(
    raw: &str,
    own_device_id: &str,
) -> Result<Option<RemoteKeySync>, String> {
    let msg: KeySyncMessage =
        serde_json::from_str(raw).map_err(|e| format!("JSON 解析失败：{e}"))?;
    if msg.device_id.trim().is_empty() {
        return Err("缺少 deviceId".into());
    }
    if msg.device_id == own_device_id {
        return Ok(None);
    }
    let source =
        parse_combo(&msg.source).map_err(|e| format!("源组合「{}」无效：{e}", msg.source))?;
    let target = if msg.target.key.is_empty() {
        None
    } else if combo_error(&msg.target).is_ok() {
        Some(msg.target)
    } else {
        None
    };
    let platform = if is_valid_platform(&msg.platform) {
        msg.platform
    } else {
        String::new()
    };
    Ok(Some(RemoteKeySync {
        source,
        target,
        platform,
    }))
}

/// 远端消息在本机的处理动作（纯逻辑，供测试与日志分级）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAction {
    /// 本机同源规则命中且平台适用：注入**本机规则**的目标组合（本地映射优先）
    Local(KeySyncRule),
    /// 本机无同源规则：按消息携带的发送端映射注入目标组合（单向配置即生效）
    Remote(KeyCombo),
    /// 本机有同源规则但其目标平台不含本机：明确拒绝（不回退远端映射）
    PlatformMismatch { platform: String },
    /// 本机无同源规则且消息未携带可注入目标（旧消息 / 平台不符）：忽略
    NoLocalRule,
}

/// 决定远端消息在本机的注入路径（纯逻辑，供测试）：
/// 结构化规则匹配（source 精确 + platform 适用）→ 本地优先；
/// 否则消息 target 合法且 platform 含本机 → 远端映射；否则忽略。
pub fn resolve_remote_action(
    remote: &RemoteKeySync,
    rules: &[KeySyncRule],
    os: &str,
) -> RemoteAction {
    let active = ActiveRules::build(rules);
    if let Some(rule) = active.lookup(&remote.source.key, remote.source.mask()) {
        if platform_applies(rule, os) {
            return RemoteAction::Local(rule.clone());
        }
        return RemoteAction::PlatformMismatch {
            platform: rule.platform.clone(),
        };
    }
    if let Some(target) = &remote.target
        && is_valid_platform(&remote.platform)
        && platform_applies_str(&remote.platform, os)
    {
        return RemoteAction::Remote(target.clone());
    }
    RemoteAction::NoLocalRule
}

/// 处理一条远端按键同步消息（由 MQTT 消息线程调用）：
/// 本机有同源规则 → 注入本机目标；无本机规则 → 按发送端映射注入（单向）。
pub fn handle_remote_message(app: &AppHandle, raw: &str) {
    let cfg = app.state::<SyncState>().config.lock().unwrap().clone();
    if !cfg.key_sync_enabled {
        return; // 防御：总开关关闭时本就未订阅该 topic
    }
    let remote = match parse_remote_message(raw, &cfg.device_id) {
        Ok(Some(remote)) => remote,
        Ok(None) => return,
        Err(err) => {
            logger::log(
                app,
                WARN,
                format!(
                    "按键同步：忽略无法识别的消息（{err}）：{}",
                    logger::preview(raw)
                ),
            );
            return;
        }
    };

    let os = os_name();
    let device = remote_device_of(raw).unwrap_or_default();
    let action = resolve_remote_action(&remote, &cfg.key_sync_rules, os);
    match action {
        RemoteAction::Local(rule) => {
            inject_remote_combo(
                app,
                os,
                &device,
                &remote.source,
                &rule.target,
                &format!("按本机规则 {} 注入", rule_display_id(&rule)),
            );
        }
        RemoteAction::Remote(target) => {
            inject_remote_combo(
                app,
                os,
                &device,
                &remote.source,
                &target,
                "本机未配置同源规则，按发送端映射注入",
            );
        }
        RemoteAction::PlatformMismatch { platform } => logger::log(
            app,
            WARN,
            format!(
                "按键同步：本机同源规则的目标平台为 {}，不适用于本机，忽略远端 {}",
                platform,
                display_combo(&remote.source, os)
            ),
        ),
        RemoteAction::NoLocalRule => logger::log(
            app,
            INFO,
            format!(
                "按键同步：远端 {}，本机未配置对应规则，消息也未携带可注入的映射，忽略",
                display_combo(&remote.source, os)
            ),
        ),
    }
}

/// 命中后的注入路径（本地规则 / 远端映射共用）：先标记抑制再注入（防回环），并记录日志
fn inject_remote_combo(
    app: &AppHandle,
    os: &str,
    device: &str,
    source: &KeyCombo,
    target: &KeyCombo,
    action_desc: &str,
) {
    let state = app.state::<KeySyncState>();
    // 先标记抑制再注入：注入产生的本机按键事件不被钩子再次广播（防回环）
    state.mark_injected(&target.canonical());
    // 注入权限（辅助功能）预检仅用于提示：预检可能误报，注入照常尝试
    #[cfg(target_os = "macos")]
    if macos_impl::post_access_ok() == Some(false) && state.take_post_permission_warning() {
        // 首次缺权限时主动弹一次系统授权面板（已拒绝过的不会重复弹）
        let main_app = app.clone();
        let _ = main_app.run_on_main_thread(macos_impl::request_post_access);
        logger::log(
            app,
            WARN,
            format!(
                "按键同步：{}（本次仍会尝试注入）",
                macos_impl::post_permission_hint()
            ),
        );
    }
    let media_note = {
        #[cfg(target_os = "macos")]
        {
            macos_impl::media_target(target)
                .map(|label| format!("（macOS 媒体键：{label}）"))
                .unwrap_or_default()
        }
        #[cfg(not(target_os = "macos"))]
        {
            String::new()
        }
    };
    match inject_combo(target) {
        Ok(()) => logger::log(
            app,
            INFO,
            format!(
                "按键同步：收到设备 {device} 的 {}，{action_desc} {}{media_note}",
                display_combo(source, os),
                display_combo(target, os)
            ),
        ),
        Err(err) => logger::log(
            app,
            ERROR,
            format!("按键同步：注入 {} 失败：{err}", display_combo(target, os)),
        ),
    }
}

/// 从原始消息中取发送方设备 id（仅用于日志，失败返回空串）
fn remote_device_of(raw: &str) -> Option<String> {
    serde_json::from_str::<KeySyncMessage>(raw)
        .ok()
        .map(|msg| msg.device_id)
}

/* ---------------- 发送链路（钩子命中 → 发布） ---------------- */

/// 钩子命中一条规则：300ms 去重后发布到 keysync/<group>
pub fn publish_press(app: &AppHandle, rule: &KeySyncRule) {
    let state = app.state::<KeySyncState>();
    let combo = rule.source.canonical();
    if !state.may_publish(&combo) {
        return;
    }
    let (device_id, group) = {
        let sync = app.state::<SyncState>();
        let cfg = sync.config.lock().unwrap();
        (cfg.device_id.clone(), cfg.group.trim().to_string())
    };
    if group.is_empty() {
        logger::log(app, INFO, "按键同步：未配置 Group 号，本机按键未同步");
        return;
    }
    let Some(client) = app.state::<SyncState>().client() else {
        // MQTT 断线：只记 INFO 限频提示，不补发
        if state.take_offline_notice() {
            logger::log(
                app,
                INFO,
                format!(
                    "按键同步：MQTT 未连接，本机 {} 未同步（不补发）",
                    display_combo(&rule.source, os_name())
                ),
            );
        }
        return;
    };
    let payload = serde_json::json!({
        "deviceId": device_id,
        "ruleId": rule.id,
        "source": combo,
        "target": rule.target,
        "platform": rule.platform,
        "ts": now_ms(),
    })
    .to_string();
    let topic = format!("{TOPIC_PREFIX}/{group}");
    tauri::async_runtime::spawn(async move {
        let _ = client
            .publish(topic, rumqttc::QoS::AtLeastOnce, false, payload)
            .await;
    });
    logger::log(
        app,
        INFO,
        format!(
            "按键同步：本机按下 {}，已通知同组设备（规则 {}）",
            display_combo(&rule.source, os_name()),
            rule_display_id(rule)
        ),
    );
    state.record_published(&combo);
}

/// 日志里的规则标识：优先备注，其次 id 截短
fn rule_display_id(rule: &KeySyncRule) -> String {
    let note = rule.note.trim();
    if note.is_empty() {
        rule.id.chars().take(8).collect()
    } else {
        note.to_string()
    }
}

/* ---------------- 引擎（全局钩子 / 注入）管理 ---------------- */

/// 引擎启动结果：Started = 已安装监听；Idle = 总开关关闭或无启用规则，未安装监听
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineStart {
    Started,
    Idle,
}

/// 引擎启动失败：Permission（macOS 缺权限等，可自动回退关闭）/ Failed（其他错误）。
/// Permission 由 macOS 权限预检构造；Windows 构建下永远不构造（dead-code 特召回宽），
/// 不能用 #[expect(dead_code)]——macOS 下该 expect 不生效反而会报错。
#[derive(Debug, Clone)]
pub enum EngineError {
    #[allow(dead_code)] // 仅 macOS 构造；Windows 构建下从未构造
    Permission(String),
    Failed(String),
}

/// 引擎回调线程使用的 AppHandle（键盘钩子回调没有用户数据参数，静态取）
static ENGINE_APP: OnceLock<AppHandle> = OnceLock::new();
/// 当前启用的规则索引（Arc 快照：回调线程只读，配置变更时整体替换）
static ENGINE_RULES: RwLock<Option<Arc<ActiveRules>>> = RwLock::new(None);

/// 钩子回调线程查规则（很短暂：只 clone 出命中规则）
fn engine_rule_for(key: &str, mask: u8) -> Option<KeySyncRule> {
    let slot = ENGINE_RULES.read().unwrap().clone()?;
    slot.lookup(key, mask).cloned()
}

/// 应用启动时按持久化开关启动引擎；失败（含 macOS 缺权限）回退关闭并 WARN。
/// 返回可能被修正过的配置（调用方继续用其完成启动时序）。
pub fn startup_enable(app: &AppHandle, mut cfg: MqttConfig) -> MqttConfig {
    if !cfg.key_sync_enabled {
        return cfg;
    }
    let enabled_count = cfg.key_sync_rules.iter().filter(|r| r.enabled).count();
    match start_engine(app, &cfg) {
        Ok(EngineStart::Started) => {
            logger::log(
                app,
                INFO,
                format!("按键同步已启动（{enabled_count} 条启用规则）"),
            );
            cfg
        }
        Ok(EngineStart::Idle) => {
            logger::log(app, INFO, "按键同步已开启（暂无启用规则，未安装键盘监听）");
            cfg
        }
        Err(EngineError::Permission(message)) => {
            logger::log(app, WARN, format!("按键同步：{message}"));
            key_sync_failed_fallback(app, &mut cfg, "权限不足");
            cfg
        }
        Err(EngineError::Failed(message)) => {
            logger::log(app, WARN, format!("按键同步：引擎启动失败：{message}"));
            key_sync_failed_fallback(app, &mut cfg, "启动失败");
            cfg
        }
    }
}

/// 启动失败 / 缺权限：总开关立即回退关闭（规则保留），配置落盘并同步内存状态
fn key_sync_failed_fallback(app: &AppHandle, cfg: &mut MqttConfig, reason: &str) {
    cfg.key_sync_enabled = false;
    let path = crate::config::config_path(app);
    if let Err(err) = cfg.save(&path) {
        logger::log(
            app,
            ERROR,
            format!("按键同步：{reason}后保存回退状态失败：{err}"),
        );
    }
    if let Some(sync) = app.try_state::<SyncState>() {
        *sync.config.lock().unwrap() = cfg.clone();
    }
    logger::log(
        app,
        WARN,
        "按键同步：总开关已自动回退为关闭（规则已保留，处理完权限后重新打开开关即可生效）",
    );
}

/// 启停引擎：先停旧引擎（幂等），再按新配置重建。
/// 规则索引先于平台引擎安装，保证钩子回调一开始就能命中规则。
pub fn start_engine(app: &AppHandle, cfg: &MqttConfig) -> Result<EngineStart, EngineError> {
    stop_engine();
    let active = ActiveRules::build(&cfg.key_sync_rules);
    if !cfg.key_sync_enabled || active.is_empty() {
        return Ok(EngineStart::Idle);
    }
    let _ = ENGINE_APP.set(app.clone());
    *ENGINE_RULES.write().unwrap() = Some(Arc::new(active));
    let result = platform_start(app);
    if result.is_err() {
        *ENGINE_RULES.write().unwrap() = None;
    }
    result.map(|()| EngineStart::Started)
}

/// 停止引擎（幂等）：清空规则索引并停掉平台钩子
pub fn stop_engine() {
    *ENGINE_RULES.write().unwrap() = None;
    platform_stop();
}

#[cfg(target_os = "windows")]
fn platform_start(app: &AppHandle) -> Result<(), EngineError> {
    windows_start(app)
}

#[cfg(target_os = "macos")]
fn platform_start(app: &AppHandle) -> Result<(), EngineError> {
    macos_start(app)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_start(_app: &AppHandle) -> Result<(), EngineError> {
    Err(EngineError::Failed("当前平台不支持按键同步".into()))
}

#[cfg(target_os = "windows")]
fn platform_stop() {
    windows_stop();
}

#[cfg(target_os = "macos")]
fn platform_stop() {
    macos_stop();
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_stop() {}

/// 注入目标组合（按本机平台实现）
pub fn inject_combo(combo: &KeyCombo) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        windows_inject_combo(combo)
    }
    #[cfg(target_os = "macos")]
    {
        macos_inject_combo(combo)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Err("当前平台不支持按键注入".into())
    }
}

/// macOS 键鼠权限状态（系统设置页提示用；非 macOS supported = false）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySyncPermissions {
    pub supported: bool,
    /// 输入监控（监听本机按键）；None = 当前系统无预检 API
    pub listen: Option<bool>,
    /// 辅助功能（注入模拟按键）；None = 当前系统无预检 API
    pub post: Option<bool>,
}

/// 查询当前进程的按键同步权限（macOS 专用；预检可能误报，仅用于 UI 提示）
pub fn key_sync_permissions() -> KeySyncPermissions {
    #[cfg(target_os = "macos")]
    {
        macos_impl::permissions()
    }
    #[cfg(not(target_os = "macos"))]
    {
        KeySyncPermissions {
            supported: false,
            listen: None,
            post: None,
        }
    }
}

/* ---------------- 启动自检：macOS「模拟按键」权限自修复 ---------------- */

/// 启动自检对调用方的期望
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupHeal {
    /// 已授权（或已请求授权），正常继续启动
    Ok,
    /// 本进程应立即退出，由自修复流程重新签名后重新启动
    Relaunching,
}

/// 与 tauri.conf.json `bundle.macOS.signingIdentity` 一致的稳定签名身份
#[cfg(target_os = "macos")]
const STABLE_SIGNING_IDENTITY: &str = "desktools-dev";

/// 启动时自动检查 macOS 辅助功能（模拟按键）权限：
/// - 已授权：直接跳过，不打扰用户。
/// - 未授权且签名异常（如被 GitHub release 的 ad-hoc 更新包覆盖）：
///   本机 keychain 若有稳定证书，就自动重签重启，重启后 TCC 记录会重新匹配，无需再授权。
/// - 未授权且签名正常 / 无法重签：主动弹出系统授权面板。
#[cfg(target_os = "macos")]
pub fn startup_self_heal(app: &AppHandle) -> StartupHeal {
    // 仅在「明确返回 false」时动作；预检 API 缺失（None）或误报时保持现状
    if macos_impl::post_access_ok() != Some(false) {
        return StartupHeal::Ok;
    }

    let Some(bundle) = installed_app_bundle() else {
        // 开发构建 / 非标准安装位置：请求授权即可
        macos_impl::request_post_access();
        return StartupHeal::Ok;
    };

    if is_signed_with_stable_identity(&bundle) {
        // 签名正常但仍未授权：首次安装或开关被动手动调整，主动请求授权
        macos_impl::request_post_access();
        return StartupHeal::Ok;
    }

    if !keychain_has_stable_identity() {
        logger::log(
            app,
            WARN,
            "按键同步权限未生效，且本机未导入稳定签名证书（desktools-dev）：跳过自动重签，已弹出系统授权面板",
        );
        macos_impl::request_post_access();
        return StartupHeal::Ok;
    }

    logger::log(
        app,
        WARN,
        "按键同步权限未生效，且安装包签名异常：自动以 desktools-dev 重新签名并重启应用（重启后若之前授权过该身份，无需再确认）",
    );
    spawn_repair_and_relaunch(&bundle);
    StartupHeal::Relaunching
}

/// 仅对 /Applications 下的 .app 安装形式识别 bundle 路径；开发/调试构建返回 None
#[cfg(target_os = "macos")]
fn installed_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    detect_app_bundle_from_exe(&exe)
}

/// 给定可执行路径，返回所属 .app bundle（dev/debug 等目标路径返回 None）
#[cfg(target_os = "macos")]
fn detect_app_bundle_from_exe(exe: &Path) -> Option<PathBuf> {
    // /Applications/<name>.app/Contents/MacOS/<name>
    if !exe.starts_with("/Applications/") {
        return None;
    }
    let macos = exe.parent()?;
    if macos.file_name().and_then(|name| name.to_str()) != Some("MacOS") {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name().and_then(|name| name.to_str()) != Some("Contents") {
        return None;
    }
    let bundle = contents.parent()?;
    if bundle
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("app"))
        != Some(true)
    {
        return None;
    }
    Some(bundle.to_path_buf())
}

/// 当前安装包是否由稳定身份（desktools-dev）签名
#[cfg(target_os = "macos")]
fn is_signed_with_stable_identity(bundle: &Path) -> bool {
    Command::new("codesign")
        .arg("-dv")
        .arg("--verbose=4")
        .arg(bundle)
        .output()
        .map(|out| {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            signed_authority_matches(&text)
        })
        .unwrap_or(false)
}

/// 判断 `codesign -dv` 输出中是否包含稳定身份的 Authority 行
#[cfg(target_os = "macos")]
fn signed_authority_matches(codesign_output: &str) -> bool {
    codesign_output.lines().any(|line| {
        line.trim()
            .starts_with(&format!("Authority={STABLE_SIGNING_IDENTITY}"))
    })
}

/// 本机 keychain 是否具备可用的稳定签名身份
#[cfg(target_os = "macos")]
fn keychain_has_stable_identity() -> bool {
    Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .map(|out| {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            identity_list_contains_stable(&text)
        })
        .unwrap_or(false)
}

/// 判断 `security find-identity -v -p codesigning` 输出中是否包含稳定身份
#[cfg(target_os = "macos")]
fn identity_list_contains_stable(identity_list: &str) -> bool {
    identity_list.contains(STABLE_SIGNING_IDENTITY)
}

/// 以稳定身份重签安装包并重新启动：抢占进程退出后执行的分离脚本
#[cfg(target_os = "macos")]
fn spawn_repair_and_relaunch(bundle: &Path) {
    let bundle_arg = shell_quoted(bundle);
    let log_file = shell_quoted(&std::env::temp_dir().join("desktools-selfheal.log"));
    let script = format!(
        "sleep 1.5 && codesign --force --deep --sign {STABLE_SIGNING_IDENTITY} {bundle_arg} >> {log_file} 2>&1 && open {bundle_arg}",
    );
    // 用分离子进程执行：主进程退出后脚本仍继续跑，完成重签与重启
    let _ = Command::new("sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// 简单 shell 引号包裹（路径不含单引号，正常情况足够）
#[cfg(target_os = "macos")]
fn shell_quoted(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/* ---------------- Windows：WH_KEYBOARD_LL 钩子 + SendInput 注入 ---------------- */

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{GetLastError, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VK_CONTROL, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR,
        VK_RETURN, VK_RIGHT, VK_RWIN, VK_SHIFT, VK_UP,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, KF_REPEAT, LLKHF_INJECTED, MSG,
        PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
        WM_QUIT, WM_SYSKEYDOWN,
    };

    /// 运行中的钩子引擎：线程 ID（用于 WM_QUIT 停止）+ 线程句柄
    struct WinEngine {
        thread_id: u32,
        join: Option<std::thread::JoinHandle<()>>,
    }

    static WIN_ENGINE: RwLock<Option<WinEngine>> = RwLock::new(None);
    /// 钩子线程安装完成后的线程 ID（父线程收到信号后读取）
    static WIN_THREAD_ID: Mutex<u32> = Mutex::new(0);

    const AZ: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    const FN: [&str; 12] = [
        "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12",
    ];

    /// 主键名 → Windows VK 码（物理键位）
    fn key_vk(name: &str) -> Option<u16> {
        Some(match name {
            "up" => VK_UP,
            "down" => VK_DOWN,
            "left" => VK_LEFT,
            "right" => VK_RIGHT,
            "pageUp" => VK_PRIOR,
            "pageDown" => VK_NEXT,
            "home" => VK_HOME,
            "end" => VK_END,
            "enter" => VK_RETURN,
            _ => {
                let bytes = name.as_bytes();
                if bytes.len() == 1 {
                    let c = bytes[0];
                    if c.is_ascii_lowercase() {
                        return Some(u16::from(c - b'a') + 0x41);
                    }
                    if c.is_ascii_digit() {
                        return Some(u16::from(c));
                    }
                    return None;
                }
                if bytes.len() >= 2 && bytes[0] == b'f' {
                    return std::str::from_utf8(&bytes[1..])
                        .ok()
                        .and_then(|num| num.parse::<u8>().ok())
                        .filter(|n| (1..=12).contains(n))
                        .map(|n| u16::from(0x70 + n - 1));
                }
                return None;
            }
        })
    }

    /// VK 码 → 主键名（钩子回调用）
    fn key_name_for_vk(vk: u16) -> Option<&'static str> {
        if (0x41..=0x5A).contains(&vk) {
            return Some(AZ[(vk - 0x41) as usize]);
        }
        if (0x30..=0x39).contains(&vk) {
            return Some(DIGITS[(vk - 0x30) as usize]);
        }
        if (0x70..=0x7B).contains(&vk) {
            return Some(FN[(vk - 0x70) as usize]);
        }
        match vk {
            VK_UP => Some("up"),
            VK_DOWN => Some("down"),
            VK_LEFT => Some("left"),
            VK_RIGHT => Some("right"),
            VK_PRIOR => Some("pageUp"),
            VK_NEXT => Some("pageDown"),
            VK_HOME => Some("home"),
            VK_END => Some("end"),
            VK_RETURN => Some("enter"),
            _ => None,
        }
    }

    /// 当前按下的修饰键集合（GetAsyncKeyState 高位即按下）
    fn mod_state() -> u8 {
        let mut mask = 0u8;
        if unsafe { GetAsyncKeyState(i32::from(VK_CONTROL)) } < 0 {
            mask |= MOD_CTRL;
        }
        if unsafe { GetAsyncKeyState(i32::from(VK_MENU)) } < 0 {
            mask |= MOD_ALT;
        }
        if unsafe { GetAsyncKeyState(i32::from(VK_SHIFT)) } < 0 {
            mask |= MOD_SHIFT;
        }
        if unsafe {
            GetAsyncKeyState(i32::from(VK_LWIN)) < 0 || GetAsyncKeyState(i32::from(VK_RWIN)) < 0
        } {
            mask |= MOD_META;
        }
        mask
    }

    /// 低级键盘钩子回调：主键 keydown 时按（主键, 修饰键）精确匹配规则。
    /// 跳过注入事件（LLKHF_INJECTED，含本机与远端触发的注入，防回环）与长按重复（KF_REPEAT）。
    unsafe extern "system" fn key_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            if code >= 0 {
                let info = &*(lparam as *const KBDLLHOOKSTRUCT);
                let message = wparam as u32;
                let down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
                if down
                    && info.flags & LLKHF_INJECTED == 0
                    && info.flags & KF_REPEAT == 0
                    && let Some(name) = key_name_for_vk(info.vkCode as u16)
                {
                    let mask = mod_state();
                    if let (Some(rule), Some(app)) = (engine_rule_for(name, mask), ENGINE_APP.get())
                    {
                        publish_press(app, &rule);
                    }
                }
            }
            CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
        }
    }

    /// 安装 WH_KEYBOARD_LL 钩子并运行消息泵（专用线程，独立生命周期）
    pub(super) fn start(_app: &AppHandle) -> Result<(), EngineError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let join = std::thread::Builder::new()
            .name("key-sync-hook".into())
            .spawn(move || unsafe {
                let module = GetModuleHandleW(std::ptr::null());
                let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook_proc), module, 0);
                if hook.is_null() {
                    let _ = ready_tx.send(Err(format!(
                        "安装键盘钩子失败（错误码 {}）",
                        GetLastError()
                    )));
                    return;
                }
                *WIN_THREAD_ID.lock().unwrap() = GetCurrentThreadId();
                let _ = ready_tx.send(Ok(()));
                // 消息泵：钩子回调依赖本线程的消息循环保持活跃；WM_QUIT 到达后退出并卸载
                // （外层 unsafe 已覆盖，edition 2024 下再写内层 unsafe 会报 unused_unsafe）
                let mut message: MSG = std::mem::zeroed();
                while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {}
                UnhookWindowsHookEx(hook);
            });
        let Ok(join) = join else {
            return Err(EngineError::Failed("无法创建键盘监听线程".into()));
        };
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => {
                let thread_id = *WIN_THREAD_ID.lock().unwrap();
                *WIN_ENGINE.write().unwrap() = Some(WinEngine {
                    thread_id,
                    join: Some(join),
                });
                Ok(())
            }
            Ok(Err(message)) => {
                let _ = join.join();
                Err(EngineError::Failed(message))
            }
            Err(_) => Err(EngineError::Failed("键盘监听线程安装超时".into())),
        }
    }

    /// 停止：向钩子线程投递 WM_QUIT，消息泵退出后钩子自行卸载
    pub(super) fn stop() {
        let engine = WIN_ENGINE.write().unwrap().take();
        let Some(engine) = engine else { return };
        unsafe { PostThreadMessageW(engine.thread_id, WM_QUIT, 0, 0) };
        if let Some(join) = engine.join {
            let _ = join.join();
        }
    }

    /// SendInput 注入目标组合：修饰键按下 → 主键按下抬起 → 修饰键抬起（逆序）
    pub(super) fn inject(combo: &KeyCombo) -> Result<(), String> {
        let main_vk =
            key_vk(&combo.key).ok_or_else(|| format!("目标主键「{}」不受支持", combo.key))?;
        let mut modifiers: Vec<u16> = Vec::with_capacity(4);
        if combo.ctrl {
            modifiers.push(VK_CONTROL);
        }
        if combo.alt {
            modifiers.push(VK_MENU);
        }
        if combo.shift {
            modifiers.push(VK_SHIFT);
        }
        if combo.meta {
            modifiers.push(VK_LWIN);
        }
        let key = |vk: u16, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let mut inputs: Vec<INPUT> = Vec::with_capacity(2 * modifiers.len() + 2);
        for vk in &modifiers {
            inputs.push(key(*vk, false));
        }
        inputs.push(key(main_vk, false));
        inputs.push(key(main_vk, true));
        for vk in modifiers.iter().rev() {
            inputs.push(key(*vk, true));
        }
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent != inputs.len() as u32 {
            return Err(format!("SendInput 只注入 {sent}/{} 个事件", inputs.len()));
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn windows_start(app: &AppHandle) -> Result<(), EngineError> {
    windows_impl::start(app)
}

#[cfg(target_os = "windows")]
fn windows_stop() {
    windows_impl::stop();
}

#[cfg(target_os = "windows")]
fn windows_inject_combo(combo: &KeyCombo) -> Result<(), String> {
    windows_impl::inject(combo)
}

/* ---------------- macOS：CGEventTap（listen-only）+ CGEventPost 注入 ---------------- */

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::*;
    use std::ffi::{CString, c_void};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crate::auto_lock::{dlsym_checked, open_framework};

    /* ---- CoreGraphics / CoreFoundation 常量（CGEventTypes.h / CGEventSource.h） ---- */
    /// kCGEventKeyDown
    const CG_EVENT_KEY_DOWN: u32 = 10;
    /// kCGEventTapDisabledByTimeout（系统超时禁用 tap，需重新启用）
    const CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFF;
    /// kCGHIDEventTap
    const CG_EVENT_TAP_HID: u32 = 0;
    /// kCGSessionEventTap
    const CG_EVENT_TAP_SESSION: u32 = 1;
    /// kCGHeadInsertEventTap
    const CG_EVENT_TAP_HEAD: u32 = 0;
    /// kCGEventTapOptionListenOnly（只监听、不拦截）
    const CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
    /// kCGKeyboardEventAutorepeat
    const CG_FIELD_AUTOREPEAT: i64 = 8;
    /// kCGKeyboardEventKeycode
    const CG_FIELD_KEYCODE: i64 = 9;
    /// kCGEventSourceUserData（自注入事件的标记字段）
    const CG_FIELD_SOURCE_USER_DATA: i64 = 42;
    /// 自注入标记值（字段命中该值即视为本模块注入）
    const CG_SELF_INJECT_MARKER: i64 = 42;
    /// kCGEventFlagMaskShift（1 << 17）
    const CG_FLAG_SHIFT: u64 = 0x0002_0000;
    /// kCGEventFlagMaskControl（1 << 18）
    const CG_FLAG_CONTROL: u64 = 0x0004_0000;
    /// kCGEventFlagMaskAlternate（1 << 19）
    const CG_FLAG_ALTERNATE: u64 = 0x0008_0000;
    /// kCGEventFlagMaskCommand（1 << 20）
    const CG_FLAG_COMMAND: u64 = 0x0010_0000;

    /* ---- 媒体键（IOKit/hidsystem/ev_keymap.h 的 NX_KEYTYPE_*） ---- */
    /// NX_KEYTYPE_SOUND_UP（音量 +）
    const NX_KEYTYPE_SOUND_UP: u32 = 0;
    /// NX_KEYTYPE_SOUND_DOWN（音量 -）
    const NX_KEYTYPE_SOUND_DOWN: u32 = 1;
    /// NX_KEYTYPE_MUTE（静音）
    const NX_KEYTYPE_MUTE: u32 = 7;
    /// NX_KEYTYPE_PLAY（播放 / 暂停）
    const NX_KEYTYPE_PLAY: u32 = 16;
    /// NX_KEYTYPE_NEXT（下一首）
    const NX_KEYTYPE_NEXT: u32 = 17;
    /// NX_KEYTYPE_PREVIOUS（上一首）
    const NX_KEYTYPE_PREVIOUS: u32 = 18;
    /// NSEventTypeSystemDefined
    const NS_EVENT_TYPE_SYSTEM_DEFINED: u64 = 14;
    /// NX_SUBTYPE_AUX_CONTROL_BUTTONS
    const NX_SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;

    type TapCallback = unsafe extern "C" fn(
        proxy: *mut c_void,
        event_type: u32,
        event: *mut c_void,
        user_info: *mut c_void,
    ) -> *mut c_void;
    type TapCreateFn = unsafe extern "C" fn(
        tap: u32,
        place: u32,
        options: u32,
        mask: u64,
        callback: Option<TapCallback>,
        user_info: *mut c_void,
    ) -> *mut c_void;
    type TapEnableFn = unsafe extern "C" fn(tap: *mut c_void, enable: bool);
    type SourceCreateFn = unsafe extern "C" fn(
        allocator: *mut c_void,
        port: *mut c_void,
        order: isize,
    ) -> *mut c_void;
    type AddSourceFn =
        unsafe extern "C" fn(run_loop: *mut c_void, source: *mut c_void, mode: *mut c_void);
    type RunFn = unsafe extern "C" fn();
    type StopFn = unsafe extern "C" fn(run_loop: *mut c_void);
    type CurrentFn = unsafe extern "C" fn() -> *mut c_void;
    type CreateKeyboardEventFn =
        unsafe extern "C" fn(source: *mut c_void, key: u16, down: bool) -> *mut c_void;
    type PostFn = unsafe extern "C" fn(tap: u32, event: *mut c_void);
    type SetFlagsFn = unsafe extern "C" fn(event: *mut c_void, flags: u64);
    type GetFlagsFn = unsafe extern "C" fn(event: *mut c_void) -> u64;
    type GetFieldFn = unsafe extern "C" fn(event: *mut c_void, field: i64) -> i64;
    type SetFieldFn = unsafe extern "C" fn(event: *mut c_void, field: i64, value: i64);
    type ReleaseFn = unsafe extern "C" fn(reference: *mut c_void);
    type PreflightFn = unsafe extern "C" fn() -> bool;

    /* ---- Objective-C / AppKit：构造 NX_SYSDEFINED 媒体键事件 ---- */
    type GetClassFn = unsafe extern "C" fn(name: *const std::ffi::c_char) -> *mut c_void;
    type SelRegisterFn = unsafe extern "C" fn(name: *const std::ffi::c_char) -> *mut c_void;
    type AutoreleasePoolPushFn = unsafe extern "C" fn() -> *mut c_void;
    type AutoreleasePoolPopFn = unsafe extern "C" fn(pool: *mut c_void);
    type OtherEventFn = unsafe extern "C" fn(
        class: *mut c_void,
        selector: *mut c_void,
        event_type: u64,
        x: f64,
        y: f64,
        flags: u64,
        timestamp: f64,
        window_number: i64,
        context: *mut c_void,
        subtype: i16,
        data1: i64,
        data2: i64,
    ) -> *mut c_void;
    type NsEventToCgFn =
        unsafe extern "C" fn(event: *mut c_void, selector: *mut c_void) -> *mut c_void;

    /// dlsym 出的函数指针集合（data 符号以 usize 存地址，保证 Send/Sync）
    struct MacApi {
        tap_create: TapCreateFn,
        tap_enable: TapEnableFn,
        run_loop_source: SourceCreateFn,
        run_loop_add_source: AddSourceFn,
        run_loop_run: RunFn,
        run_loop_stop: StopFn,
        run_loop_current: CurrentFn,
        event_create: CreateKeyboardEventFn,
        event_post: PostFn,
        event_set_flags: SetFlagsFn,
        event_get_flags: GetFlagsFn,
        event_get_field: GetFieldFn,
        event_set_field: SetFieldFn,
        cf_release: ReleaseFn,
        /// kCFAllocatorDefault 的值
        allocator: *mut c_void,
        /// kCFRunLoopCommonModes 的值
        common_modes: *mut c_void,
        /// CGPreflightListenEventAccess（10.15+，缺失时跳过预检）
        preflight_listen: Option<PreflightFn>,
        /// CGPreflightPostEventAccess（10.15+，缺失时跳过预检）
        preflight_post: Option<PreflightFn>,
        /// CGRequestPostEventAccess（10.15+，缺失时不主动弹权限面板）
        request_post: Option<PreflightFn>,
    }

    // 纯函数指针 + 从 CF 全局常量读出的不可变值，跨线程共享只读安全
    unsafe impl Send for MacApi {}
    unsafe impl Sync for MacApi {}

    impl MacApi {
        /// dlopen 句柄上的符号集合。dlopen 句柄刻意不关闭（函数指针全程存活，
        /// 与 lock_sync 的长驻 dlopen 同理）。
        fn from_handles(cg: *mut c_void, cf: *mut c_void) -> Result<Self, String> {
            macro_rules! sym {
                ($handle:expr, $name:literal, $ty:ty) => {{
                    let ptr = dlsym_checked($handle, $name)
                        .ok_or_else(|| format!("缺少符号 {}", $name))?;
                    unsafe { std::mem::transmute::<*mut c_void, $ty>(ptr) }
                }};
            }
            macro_rules! data_sym {
                ($handle:expr, $name:literal) => {{
                    // CF 全局常量本身是指针变量：dlsym 拿到变量地址，解引用取值
                    let var = dlsym_checked($handle, $name)
                        .ok_or_else(|| format!("缺少符号 {}", $name))?;
                    unsafe { *(var as *const *mut c_void) }
                }};
            }
            Ok(Self {
                tap_create: sym!(cg, "CGEventTapCreate", TapCreateFn),
                tap_enable: sym!(cg, "CGEventTapEnable", TapEnableFn),
                run_loop_source: sym!(cf, "CFMachPortCreateRunLoopSource", SourceCreateFn),
                run_loop_add_source: sym!(cf, "CFRunLoopAddSource", AddSourceFn),
                run_loop_run: sym!(cf, "CFRunLoopRun", RunFn),
                run_loop_stop: sym!(cf, "CFRunLoopStop", StopFn),
                run_loop_current: sym!(cf, "CFRunLoopGetCurrent", CurrentFn),
                event_create: sym!(cg, "CGEventCreateKeyboardEvent", CreateKeyboardEventFn),
                event_post: sym!(cg, "CGEventPost", PostFn),
                event_set_flags: sym!(cg, "CGEventSetFlags", SetFlagsFn),
                event_get_flags: sym!(cg, "CGEventGetFlags", GetFlagsFn),
                event_get_field: sym!(cg, "CGEventGetIntegerValueField", GetFieldFn),
                event_set_field: sym!(cg, "CGEventSetIntegerValueField", SetFieldFn),
                cf_release: sym!(cf, "CFRelease", ReleaseFn),
                allocator: data_sym!(cf, "kCFAllocatorDefault"),
                common_modes: data_sym!(cf, "kCFRunLoopCommonModes"),
                preflight_listen: dlsym_checked(cg, "CGPreflightListenEventAccess")
                    .map(|ptr| unsafe { std::mem::transmute::<*mut c_void, PreflightFn>(ptr) }),
                preflight_post: dlsym_checked(cg, "CGPreflightPostEventAccess")
                    .map(|ptr| unsafe { std::mem::transmute::<*mut c_void, PreflightFn>(ptr) }),
                request_post: dlsym_checked(cg, "CGRequestPostEventAccess")
                    .map(|ptr| unsafe { std::mem::transmute::<*mut c_void, PreflightFn>(ptr) }),
            })
        }
    }

    static MAC_API: OnceLock<MacApi> = OnceLock::new();

    /// 加载 CoreGraphics / CoreFoundation 符号（首次成功后缓存；失败返回 Err）
    fn api() -> Result<&'static MacApi, String> {
        if let Some(api) = MAC_API.get() {
            return Ok(api);
        }
        let cg = open_framework("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
            .ok_or("dlopen CoreGraphics 失败")?;
        let cf =
            open_framework("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
                .ok_or("dlopen CoreFoundation 失败")?;
        let api = MacApi::from_handles(cg, cf)?;
        let _ = MAC_API.set(api);
        Ok(MAC_API.get().unwrap())
    }

    /// 监听权限提示（输入监控）：安装事件 tap 需要
    fn permission_hint() -> String {
        "macOS 未授予「监听键盘事件」权限：请在 系统设置 → 隐私与安全性 → 输入监控 中勾选 desktools；若已勾选仍不生效，完全退出应用后重新打开，再开启本开关".into()
    }

    /// 注入权限提示（辅助功能）：CGEventPost 投递按键需要；预检可能误报，不阻断注入
    pub(super) fn post_permission_hint() -> String {
        "macOS 未授予「模拟按键」权限（辅助功能）：已尝试弹出系统授权面板；若未出现，请到 系统设置 → 隐私与安全性 → 辅助功能 勾选 desktools（之前拒绝过需手动开启）并重启应用；本次注入仍会尝试但可能被系统丢弃".into()
    }

    /// 预检系统是否允许监听键盘事件（以预检为准，不弹安全面板）。
    /// 预检 API 为 10.15+ 符号：更早系统缺失时忽略预检，交给实际安装结果兜底。
    /// 注意：这里只检查监听权限；注入权限（辅助功能）在接收消息时按需提示，避免
    /// 「只想接收、不发按键」的设备被误判为未授权。
    fn preflight() -> Result<(), String> {
        let api = api()?;
        if let Some(check) = api.preflight_listen
            && unsafe { !check() }
        {
            return Err(permission_hint());
        }
        Ok(())
    }

    /// 注入权限预检（辅助功能，10.15+）：仅供提示，不阻断注入——
    /// CGPreflightPostEventAccess 在部分环境会误报 false，而 CGEventPost 实际可投递
    pub(super) fn post_access_ok() -> Option<bool> {
        let api = api().ok()?;
        api.preflight_post.map(|check| unsafe { check() })
    }

    /// 查询监听 / 注入两类系统权限，供设置页展示可操作的提示。
    pub(super) fn permissions() -> KeySyncPermissions {
        let Ok(api) = api() else {
            return KeySyncPermissions {
                supported: true,
                listen: None,
                post: None,
            };
        };
        KeySyncPermissions {
            supported: true,
            listen: api.preflight_listen.map(|check| unsafe { check() }),
            post: api.preflight_post.map(|check| unsafe { check() }),
        }
    }

    /// 主动请求「模拟按键」权限（辅助功能）：系统会弹出授权面板并把应用加入列表。
    /// 仅在预检失败且首次收到远端按键时调用；已拒绝过的应用不会再次弹窗。
    pub(super) fn request_post_access() {
        if let Ok(api) = api()
            && let Some(request) = api.request_post
        {
            unsafe {
                request();
            }
        }
    }

    /* ---- macOS 顶排 F7~F12：按物理键语义注入媒体键 ---- */

    /// Objective-C 运行时 / AppKit 函数集合（构造 NX_SYSDEFINED 媒体键事件）
    struct MediaApi {
        ns_event_class: *mut c_void,
        sel_other_event: *mut c_void,
        sel_cg_event: *mut c_void,
        other_event: OtherEventFn,
        cg_event: NsEventToCgFn,
        pool_push: AutoreleasePoolPushFn,
        pool_pop: AutoreleasePoolPopFn,
    }

    // 仅函数指针与 ObjC 类/选择器地址，跨线程只读使用安全
    unsafe impl Send for MediaApi {}
    unsafe impl Sync for MediaApi {}

    fn media_api() -> Result<&'static MediaApi, String> {
        static MEDIA_API: OnceLock<Result<MediaApi, String>> = OnceLock::new();
        MEDIA_API
            .get_or_init(|| {
                let _appkit = open_framework("/System/Library/Frameworks/AppKit.framework/AppKit")
                    .ok_or("dlopen AppKit 失败")?;
                let objc = open_framework("/usr/lib/libobjc.A.dylib")
                    .ok_or("dlopen libobjc 失败")?;
                let get_class: GetClassFn = objc_sym(objc, "objc_getClass")?;
                let sel_register: SelRegisterFn = objc_sym(objc, "sel_registerName")?;
                let msg_send = dlsym_checked(objc, "objc_msgSend")
                    .ok_or_else(|| "缺少符号 objc_msgSend".to_string())?;
                let pool_push: AutoreleasePoolPushFn = objc_sym(objc, "objc_autoreleasePoolPush")?;
                let pool_pop: AutoreleasePoolPopFn = objc_sym(objc, "objc_autoreleasePoolPop")?;
                let class_name = CString::new("NSEvent").unwrap();
                let ns_event_class = unsafe { get_class(class_name.as_ptr()) };
                if ns_event_class.is_null() {
                    return Err("未找到 NSEvent 类".into());
                }
                let other_sel = CString::new(
                    "otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:",
                )
                .unwrap();
                let cg_sel = CString::new("CGEvent").unwrap();
                Ok(MediaApi {
                    ns_event_class,
                    sel_other_event: unsafe { sel_register(other_sel.as_ptr()) },
                    sel_cg_event: unsafe { sel_register(cg_sel.as_ptr()) },
                    other_event: unsafe { std::mem::transmute::<*mut c_void, OtherEventFn>(msg_send) },
                    cg_event: unsafe { std::mem::transmute::<*mut c_void, NsEventToCgFn>(msg_send) },
                    pool_push,
                    pool_pop,
                })
            })
            .as_ref()
            .map_err(|err| err.clone())
    }

    fn objc_sym<T>(handle: *mut c_void, name: &str) -> Result<T, String> {
        let ptr = dlsym_checked(handle, name).ok_or_else(|| format!("缺少符号 {name}"))?;
        Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) })
    }

    /// 顶排 F7~F12 的媒体功能（与物理键一致；F1~F6 涉及亮度 / 调度中心等，暂不转换）
    pub(super) fn media_aux_key(key: &str) -> Option<(u32, &'static str)> {
        Some(match key {
            "f7" => (NX_KEYTYPE_PREVIOUS, "上一首"),
            "f8" => (NX_KEYTYPE_PLAY, "播放/暂停"),
            "f9" => (NX_KEYTYPE_NEXT, "下一首"),
            "f10" => (NX_KEYTYPE_MUTE, "静音"),
            "f11" => (NX_KEYTYPE_SOUND_DOWN, "音量减"),
            "f12" => (NX_KEYTYPE_SOUND_UP, "音量加"),
            _ => return None,
        })
    }

    /// 系统是否勾选「将 F1、F2 等键用作标准功能键」。
    /// 未设置 / 0 = 顶排默认媒体键（注入时模拟媒体键）；1 = 标准 F 键。
    /// 30 秒缓存：避免每次按键都起 `defaults` 进程，又能让用户改设置后较快生效。
    fn fn_keys_standard() -> bool {
        static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
        let now = Instant::now();
        if let Some((at, value)) = *CACHE.lock().unwrap()
            && now.duration_since(at) < Duration::from_secs(30)
        {
            return value;
        }
        let value = std::process::Command::new("/usr/bin/defaults")
            .args(["read", "-g", "com.apple.keyboard.fnState"])
            .output()
            .ok()
            .is_some_and(|out| {
                out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "1"
            });
        *CACHE.lock().unwrap() = Some((now, value));
        value
    }

    /// 该目标组合是否会按媒体键注入（None = 普通按键；Some(说明) 用于日志）
    pub(super) fn media_target(combo: &KeyCombo) -> Option<&'static str> {
        if combo.mask() != 0 || fn_keys_standard() {
            return None;
        }
        media_aux_key(&combo.key).map(|(_, label)| label)
    }

    /// 投递一对 NX_SYSDEFINED 媒体键事件（data1 = 键类型 + 按下/抬起状态）。
    /// CGEventPost 无法直接构造该类型，这里经 NSEvent 转换后投递到 HID tap。
    fn post_media_key(api: &MacApi, aux: u32) -> Result<(), String> {
        let media = media_api()?;
        unsafe {
            let pool = (media.pool_push)();
            for down in [true, false] {
                // 状态位同时写入 modifierFlags 与 data1（0xA00 = 按下，0xB00 = 抬起），
                // 这是系统媒体键事件的真实形态；只写 data1 时部分系统版本会忽略事件。
                let state: i64 = if down { 0xA00 } else { 0xB00 };
                let data1 = (i64::from(aux) << 16) | state;
                let ns_event = (media.other_event)(
                    media.ns_event_class,
                    media.sel_other_event,
                    NS_EVENT_TYPE_SYSTEM_DEFINED,
                    0.0,
                    0.0,
                    state as u64,
                    0.0,
                    0,
                    std::ptr::null_mut(),
                    NX_SUBTYPE_AUX_CONTROL_BUTTONS,
                    data1,
                    -1,
                );
                if ns_event.is_null() {
                    (media.pool_pop)(pool);
                    return Err("创建媒体键事件失败".into());
                }
                let event = (media.cg_event)(ns_event, media.sel_cg_event);
                if event.is_null() {
                    (media.pool_pop)(pool);
                    return Err("创建媒体键 CGEvent 失败".into());
                }
                (api.event_set_field)(event, CG_FIELD_SOURCE_USER_DATA, CG_SELF_INJECT_MARKER);
                (api.event_post)(CG_EVENT_TAP_HID, event);
            }
            (media.pool_pop)(pool);
        }
        Ok(())
    }

    /// macOS 虚拟键码（kVK_*，物理键位）
    pub(super) fn keycode_for_key(key: &str) -> Option<u16> {
        Some(match key {
            "a" => 0x00,
            "s" => 0x01,
            "d" => 0x02,
            "f" => 0x03,
            "h" => 0x04,
            "g" => 0x05,
            "z" => 0x06,
            "x" => 0x07,
            "c" => 0x08,
            "v" => 0x09,
            "b" => 0x0B,
            "q" => 0x0C,
            "w" => 0x0D,
            "e" => 0x0E,
            "r" => 0x0F,
            "y" => 0x10,
            "t" => 0x11,
            "i" => 0x22,
            "o" => 0x1F,
            "p" => 0x23,
            "l" => 0x25,
            "j" => 0x26,
            "k" => 0x28,
            "n" => 0x2D,
            "m" => 0x2E,
            "u" => 0x20,
            "1" => 0x12,
            "2" => 0x13,
            "3" => 0x14,
            "4" => 0x15,
            "5" => 0x17,
            "6" => 0x16,
            "7" => 0x1A,
            "8" => 0x1C,
            "9" => 0x19,
            "0" => 0x1D,
            "f1" => 0x7A,
            "f2" => 0x78,
            "f3" => 0x63,
            "f4" => 0x76,
            "f5" => 0x60,
            "f6" => 0x61,
            "f7" => 0x62,
            "f8" => 0x64,
            "f9" => 0x65,
            "f10" => 0x6D,
            "f11" => 0x67,
            "f12" => 0x6F,
            "up" => 0x7E,
            "down" => 0x7D,
            "left" => 0x7B,
            "right" => 0x7C,
            "pageUp" => 0x74,
            "pageDown" => 0x79,
            "home" => 0x73,
            "end" => 0x77,
            "enter" => 0x24,
            _ => return None,
        })
    }

    /// 键码 → 主键名（tap 回调用）
    fn key_name_for_keycode(code: u16) -> Option<&'static str> {
        Some(match code {
            0x00 => "a",
            0x01 => "s",
            0x02 => "d",
            0x03 => "f",
            0x04 => "h",
            0x05 => "g",
            0x06 => "z",
            0x07 => "x",
            0x08 => "c",
            0x09 => "v",
            0x0B => "b",
            0x0C => "q",
            0x0D => "w",
            0x0E => "e",
            0x0F => "r",
            0x10 => "y",
            0x11 => "t",
            0x1F => "o",
            0x20 => "u",
            0x22 => "i",
            0x23 => "p",
            0x25 => "l",
            0x26 => "j",
            0x28 => "k",
            0x2D => "n",
            0x2E => "m",
            0x12 => "1",
            0x13 => "2",
            0x14 => "3",
            0x15 => "4",
            0x16 => "6",
            0x17 => "5",
            0x19 => "9",
            0x1A => "7",
            0x1C => "8",
            0x1D => "0",
            0x60 => "f5",
            0x61 => "f6",
            0x62 => "f7",
            0x63 => "f3",
            0x64 => "f8",
            0x65 => "f9",
            0x67 => "f11",
            0x6D => "f10",
            0x6F => "f12",
            0x76 => "f4",
            0x78 => "f2",
            0x7A => "f1",
            0x73 => "home",
            0x74 => "pageUp",
            0x77 => "end",
            0x79 => "pageDown",
            0x24 => "enter",
            0x4C => "enter",
            0x7B => "left",
            0x7C => "right",
            0x7D => "down",
            0x7E => "up",
            _ => return None,
        })
    }

    /// 本机修饰键掩码 → CGEventFlags
    fn mask_to_flags(mask: u8) -> u64 {
        let mut flags = 0u64;
        if mask & MOD_CTRL != 0 {
            flags |= CG_FLAG_CONTROL;
        }
        if mask & MOD_ALT != 0 {
            flags |= CG_FLAG_ALTERNATE;
        }
        if mask & MOD_SHIFT != 0 {
            flags |= CG_FLAG_SHIFT;
        }
        if mask & MOD_META != 0 {
            flags |= CG_FLAG_COMMAND;
        }
        flags
    }

    /// CGEventFlags → 本机修饰键掩码（只取四个修饰键位）
    fn flags_to_mask(flags: u64) -> u8 {
        let mut mask = 0u8;
        if flags & CG_FLAG_CONTROL != 0 {
            mask |= MOD_CTRL;
        }
        if flags & CG_FLAG_ALTERNATE != 0 {
            mask |= MOD_ALT;
        }
        if flags & CG_FLAG_SHIFT != 0 {
            mask |= MOD_SHIFT;
        }
        if flags & CG_FLAG_COMMAND != 0 {
            mask |= MOD_META;
        }
        mask
    }

    /// tap 回调：kCGEventKeyDown 时按（主键, 修饰键）精确匹配规则。
    /// 跳过长按自动重复（kCGKeyboardEventAutorepeat）与自注入事件
    /// （kCGEventSourceUserData == 标记值，防回环）。
    unsafe extern "C" fn tap_callback(
        _proxy: *mut c_void,
        event_type: u32,
        event: *mut c_void,
        _user_info: *mut c_void,
    ) -> *mut c_void {
        unsafe {
            // 系统可能因超时禁用 tap：尽力重新启用（防御性处理）
            if event_type == CG_EVENT_TAP_DISABLED_BY_TIMEOUT {
                if let (Some(api), Some(engine)) =
                    (MAC_API.get(), MAC_ENGINE.read().unwrap().as_ref())
                {
                    (api.tap_enable)(engine.tap as *mut c_void, true);
                }
                return event;
            }
            if event_type != CG_EVENT_KEY_DOWN {
                return event;
            }
            let Some(api) = MAC_API.get() else {
                return event;
            };
            if (api.event_get_field)(event, CG_FIELD_AUTOREPEAT) != 0 {
                return event; // 长按不重复
            }
            if (api.event_get_field)(event, CG_FIELD_SOURCE_USER_DATA) == CG_SELF_INJECT_MARKER {
                return event; // 自注入事件不触发广播
            }
            let code = (api.event_get_field)(event, CG_FIELD_KEYCODE) as u16;
            let Some(name) = key_name_for_keycode(code) else {
                return event;
            };
            let mask = flags_to_mask((api.event_get_flags)(event));
            if let (Some(rule), Some(app)) = (engine_rule_for(name, mask), ENGINE_APP.get()) {
                publish_press(app, &rule);
            }
            event
        }
    }

    /// 运行中的 tap 引擎：CFMachPortRef（tap）、CFRunLoopRef（可停止）
    struct MacEngine {
        tap: usize,
        run_loop: usize,
        join: Option<std::thread::JoinHandle<()>>,
    }

    static MAC_ENGINE: RwLock<Option<MacEngine>> = RwLock::new(None);

    /// 安装 listen-only tap 会话并进入 CFRunLoop（专用线程，独立生命周期）
    pub(super) fn start(_app: &AppHandle) -> Result<(), EngineError> {
        preflight().map_err(EngineError::Permission)?;
        let api = api().map_err(EngineError::Failed)?;

        let (ready_tx, ready_rx) = mpsc::channel::<Result<(usize, usize), (bool, String)>>();
        let join = std::thread::Builder::new()
            .name("key-sync-tap".into())
            .spawn(move || unsafe {
                let tap = (api.tap_create)(
                    CG_EVENT_TAP_SESSION,
                    CG_EVENT_TAP_HEAD,
                    CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                    1u64 << CG_EVENT_KEY_DOWN,
                    Some(tap_callback),
                    std::ptr::null_mut(),
                );
                if tap.is_null() {
                    // 会话级 tap 拿不到权限是最常见原因，按权限类错误处理
                    let _ = ready_tx.send(Err((true, permission_hint())));
                    return;
                }
                let source = (api.run_loop_source)(api.allocator, tap, 0);
                if source.is_null() {
                    (api.cf_release)(tap);
                    let _ = ready_tx.send(Err((false, "创建 RunLoop Source 失败".into())));
                    return;
                }
                let run_loop = (api.run_loop_current)();
                (api.run_loop_add_source)(run_loop, source, api.common_modes);
                (api.tap_enable)(tap, true);
                let _ = ready_tx.send(Ok((tap as usize, run_loop as usize)));
                // 阻塞运行；stop() 使用 CFRunLoopStop 结束后在这里收尾
                (api.run_loop_run)();
                (api.cf_release)(source);
                (api.cf_release)(tap);
            });
        let Ok(join) = join else {
            return Err(EngineError::Failed("无法创建键盘监听线程".into()));
        };
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok((tap, run_loop))) => {
                *MAC_ENGINE.write().unwrap() = Some(MacEngine {
                    tap,
                    run_loop,
                    join: Some(join),
                });
                Ok(())
            }
            Ok(Err((permission_like, message))) => {
                let _ = join.join();
                if permission_like {
                    Err(EngineError::Permission(message))
                } else {
                    Err(EngineError::Failed(message))
                }
            }
            Err(_) => Err(EngineError::Failed("键盘监听线程安装超时".into())),
        }
    }

    /// 停止：禁用 tap 并中断 tap 线程的 run loop，线程随后释放资源退出
    pub(super) fn stop() {
        let engine = MAC_ENGINE.write().unwrap().take();
        let Some(engine) = engine else { return };
        if let Some(api) = MAC_API.get() {
            unsafe {
                (api.tap_enable)(engine.tap as *mut c_void, false);
                (api.run_loop_stop)(engine.run_loop as *mut c_void);
            }
        }
        if let Some(join) = engine.join {
            let _ = join.join();
        }
    }

    /// 注入目标组合：构造一对带修饰键 flag 与自注入标记的按下/抬起事件并投递到
    /// kCGHIDEventTap（AppleScript keystroke 同源做法；kCGEventSourceUserData 标记
    /// 让 tap 回调跳过，防回环）。
    pub(super) fn inject(combo: &KeyCombo) -> Result<(), String> {
        let api = api()?;
        // macOS 顶排 F7~F12 默认是媒体键：未按修饰键且系统未切到「标准功能键」时，
        // 模拟物理媒体键（NX_SYSDEFINED）。否则 Music / QQ 音乐等只监听媒体键的
        // 应用收不到普通 F 键——表现为「日志显示已注入，但音乐毫无反应」。
        if combo.mask() == 0
            && !fn_keys_standard()
            && let Some((aux, _)) = media_aux_key(&combo.key)
        {
            return post_media_key(api, aux);
        }
        let keycode = keycode_for_key(&combo.key)
            .ok_or_else(|| format!("目标主键「{}」不受支持", combo.key))?;
        // 不在这里用 CGPreflightPostEventAccess 阻断注入：该预检在部分环境会误报
        // （权限已授予仍返回 false），而实际 CGEventPost 可能正常投递。
        // 权限缺失时由调用方记录一次 WARN 提示，注入始终尽力执行。
        let flags = mask_to_flags(combo.mask());
        unsafe {
            let down = (api.event_create)(std::ptr::null_mut(), keycode, true);
            if down.is_null() {
                return Err("创建按键事件失败".into());
            }
            let up = (api.event_create)(std::ptr::null_mut(), keycode, false);
            if up.is_null() {
                (api.cf_release)(down);
                return Err("创建按键事件失败".into());
            }
            for event in [down, up] {
                (api.event_set_field)(event, CG_FIELD_SOURCE_USER_DATA, CG_SELF_INJECT_MARKER);
                (api.event_set_flags)(event, flags);
                (api.event_post)(CG_EVENT_TAP_HID, event);
                (api.cf_release)(event);
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn macos_start(app: &AppHandle) -> Result<(), EngineError> {
    macos_impl::start(app)
}

#[cfg(target_os = "macos")]
fn macos_stop() {
    macos_impl::stop();
}

#[cfg(target_os = "macos")]
fn macos_inject_combo(combo: &KeyCombo) -> Result<(), String> {
    macos_impl::inject(combo)
}

#[cfg(all(test, target_os = "macos"))]
mod self_heal_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detects_installed_app_bundle() {
        let exe = Path::new("/Applications/desktools.app/Contents/MacOS/desktools");
        assert_eq!(
            detect_app_bundle_from_exe(exe).as_deref(),
            Some(Path::new("/Applications/desktools.app"))
        );
    }

    #[test]
    fn ignores_non_installed_locations() {
        // dev / debug 构建不在 /Applications 下
        assert_eq!(
            detect_app_bundle_from_exe(Path::new(
                "/Users/hyy/project/desktools/src-tauri/target/release/desktools"
            )),
            None
        );
        // .app 目录缺失
        assert_eq!(
            detect_app_bundle_from_exe(Path::new("/Applications/foo/Contents/MacOS/foo")),
            None
        );
        // 不是从*.app 里的可执行
        assert_eq!(
            detect_app_bundle_from_exe(Path::new(
                "/Applications/other.app/Contents/Resources/helper"
            )),
            None
        );
    }

    #[test]
    fn stable_identity_matches_authority_line() {
        assert!(signed_authority_matches("Authority=desktools-dev"));
        assert!(!signed_authority_matches("Signature=adhoc"));
        assert!(!signed_authority_matches("Authority=other-dev"));
    }

    #[test]
    fn identity_list_matches_stable_identity() {
        assert!(identity_list_contains_stable(
            "1) 89EA8DD1763109B974A2B31118D6FA9DAB0EDD32 \"desktools-dev\"\n"
        ));
        assert!(!identity_list_contains_stable(
            "  2) EFCDE... \"some-other-identity\"\n"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(text: &str) -> KeyCombo {
        parse_combo(text).unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn rule(
        id: &str,
        enabled: bool,
        source: &str,
        target: &str,
        platform: &str,
        note: &str,
    ) -> KeySyncRule {
        KeySyncRule {
            id: id.into(),
            enabled,
            source: combo(source),
            target: combo(target),
            platform: platform.into(),
            note: note.into(),
        }
    }

    #[test]
    fn topic_uses_trimmed_group() {
        assert_eq!(topic(" my-group "), "keysync/my-group");
    }

    #[test]
    fn parse_combo_variants() {
        assert_eq!(parse_combo("Ctrl+down").unwrap().key, "down");
        assert!(parse_combo("Ctrl+down").unwrap().ctrl);
        assert!(!parse_combo("Ctrl+down").unwrap().shift);
        assert!(parse_combo("Ctrl+Shift+Alt+PageUp").unwrap().shift);
        assert!(parse_combo("Super+F9").unwrap().meta);
        assert!(parse_combo("Cmd+F1").unwrap().meta);
        assert_eq!(parse_combo("f9").unwrap().key, "f9");
        assert_eq!(parse_combo("A").unwrap().key, "a");
        assert_eq!(parse_combo("arrowdown").unwrap().key, "down");
        assert_eq!(parse_combo("Page Up").unwrap().key, "pageUp");
        assert_eq!(parse_combo("PgDn").unwrap().key, "pageDown");
        assert_eq!(parse_combo("Enter").unwrap().key, "enter");
        assert_eq!(parse_combo("Return").unwrap().key, "enter");
        assert_eq!(parse_combo("numpadenter").unwrap().key, "enter");
        assert_eq!(parse_combo("Ctrl+Enter").unwrap().canonical(), "ctrl+enter");
        assert!(parse_combo("ctrl+win+down").is_ok()); // Super 的别名 windows
    }

    #[test]
    fn parse_combo_rejects_invalid() {
        assert!(parse_combo("").is_err());
        assert!(parse_combo("   ").is_err());
        assert!(parse_combo("ctrl").is_err()); // 只有修饰键，缺主键
        assert!(parse_combo("ctrl++").is_err());
        assert!(parse_combo("f13").is_err());
        assert!(parse_combo("Space").is_err());
        assert!(parse_combo("Alt+ArrowLeft&&").is_err());
    }

    #[test]
    fn canonical_and_display() {
        let ctrl_shift_down = combo("Ctrl+Shift+Down");
        assert_eq!(ctrl_shift_down.canonical(), "ctrl+shift+down");
        assert_eq!(display_combo(&ctrl_shift_down, "windows"), "Ctrl+Shift+↓");
        assert_eq!(display_combo(&ctrl_shift_down, "macos"), "Ctrl+Shift+↓");
        let mac_combo = combo("Cmd+F9");
        assert_eq!(mac_combo.canonical(), "super+f9");
        assert_eq!(display_combo(&mac_combo, "macos"), "Cmd+F9");
        assert_eq!(display_combo(&mac_combo, "windows"), "Win+F9");
        assert_eq!(display_combo(&combo("enter"), "macos"), "Enter");
        assert_eq!(
            display_combo(&parse_combo("alt+f1").unwrap(), "macos"),
            "Option+F1"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_media_keys_map_top_row_f7_to_f12() {
        use super::macos_impl::media_aux_key;
        assert_eq!(media_aux_key("f7").unwrap().1, "上一首");
        assert_eq!(media_aux_key("f8").unwrap().1, "播放/暂停");
        assert_eq!(media_aux_key("f9").unwrap().1, "下一首");
        assert_eq!(media_aux_key("f10").unwrap().1, "静音");
        assert_eq!(media_aux_key("f11").unwrap().1, "音量减");
        assert_eq!(media_aux_key("f12").unwrap().1, "音量加");
        assert!(media_aux_key("f6").is_none());
        assert!(media_aux_key("a").is_none());
    }

    #[test]
    fn canonical_from_matches_combo_canonical() {
        // 钩子命中路径（主键+掩码）与规则路径必须产出同一个串
        assert_eq!(canonical_from("down", MOD_CTRL), "ctrl+down");
        let combo = combo("Ctrl+Down");
        assert_eq!(combo.canonical(), canonical_from("down", combo.mask()));
    }

    #[test]
    fn rules_specificity_lookup() {
        let rules = vec![
            rule("r1", true, "Ctrl+a", "F5", PLATFORM_ALL, "ctrl-only"),
            rule("r2", true, "Ctrl+Alt+a", "F6", PLATFORM_ALL, "ctrl-alt"),
        ];
        let active = ActiveRules::build(&rules);
        assert_eq!(active.lookup("a", MOD_CTRL).unwrap().id, "r1");
        assert_eq!(active.lookup("a", MOD_CTRL | MOD_ALT).unwrap().id, "r2");
        // 修饰键必须精确匹配：多按 / 少按都不命中
        assert!(active.lookup("a", MOD_CTRL | MOD_SHIFT).is_none());
        assert!(active.lookup("b", MOD_CTRL).is_none());
    }

    #[test]
    fn duplicate_exact_rules_first_defined_wins() {
        let rules = vec![
            rule("first", true, "f9", "F12", PLATFORM_ALL, "dup-1"),
            rule("second", true, "f9", "F11", PLATFORM_ALL, "dup-2"),
        ];
        let active = ActiveRules::build(&rules);
        assert_eq!(active.lookup("f9", 0).unwrap().id, "first");
    }

    #[test]
    fn disabled_rules_never_match() {
        let rules = vec![rule("off", false, "Ctrl+a", "F5", PLATFORM_ALL, "")];
        let active = ActiveRules::build(&rules);
        assert!(active.lookup("a", MOD_CTRL).is_none());
    }

    #[test]
    fn invalid_source_rules_never_match() {
        // 手改配置里出现的非法源组合不应让引擎崩溃或匹配
        let mut bad = rule("bad", true, "Ctrl+a", "F5", PLATFORM_ALL, "");
        bad.source.key = "space!!".into();
        let active = ActiveRules::build(&[bad]);
        assert!(active.lookup("a", MOD_CTRL).is_none());
    }

    #[test]
    fn platform_applies_filters_receiving() {
        let all = rule("all", true, "Ctrl+down", "F9", PLATFORM_ALL, "");
        let win_only = rule("win", true, "Ctrl+down", "F9", PLATFORM_WINDOWS, "");
        let mac_only = rule("mac", true, "Ctrl+down", "F9", PLATFORM_MACOS, "");
        assert!(platform_applies(&all, PLATFORM_WINDOWS));
        assert!(platform_applies(&all, PLATFORM_MACOS));
        assert!(!platform_applies(&win_only, PLATFORM_MACOS));
        assert!(platform_applies(&mac_only, PLATFORM_MACOS));
    }

    #[test]
    fn validate_rules_enforces_enabled_and_platform() {
        let valid = vec![rule("r", true, "Ctrl+down", "F9", PLATFORM_ALL, "")];
        assert!(validate_rules(&valid).is_ok());

        // 平台字段非法
        let mut bad_platform = valid.clone();
        bad_platform[0].platform = "linux".into();
        assert!(validate_rules(&bad_platform).is_err());

        // 启用规则缺主键
        let mut empty = valid.clone();
        empty[0].source = KeyCombo::default();
        assert!(validate_rules(&empty).is_err());

        // 启用规则目标键非法
        let mut bad_target = valid.clone();
        bad_target[0].target.key = "space!!".into();
        assert!(validate_rules(&bad_target).is_err());

        // 禁用规则允许保留未填写 / 非法的组合
        let mut disabled = valid.clone();
        disabled[0].enabled = false;
        disabled[0].source = KeyCombo::default();
        disabled[0].target.key = "whatever!!".into();
        assert!(validate_rules(&disabled).is_ok());
    }

    #[test]
    fn rule_serde_uses_camel_case() {
        let rule = rule("r1", true, "Ctrl+down", "F9", PLATFORM_ALL, "示例");
        let json = serde_json::to_string(&rule).unwrap();
        assert!(json.contains("\"source\":{\"ctrl\":true"));
        assert!(json.contains("\"key\":\"down\""));
        assert!(json.contains("\"platform\":\"all\""));
        let back: KeySyncRule = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rule);
    }

    #[test]
    fn message_serde_uses_camel_case() {
        // 新版本消息：携带 target（结构化）与 platform；字段名 camelCase
        let full = r#"{"deviceId":"d1","ruleId":"r1","source":"ctrl+down","target":{},"platform":"macos","ts":123}"#;
        let msg: KeySyncMessage = serde_json::from_str(full).unwrap();
        assert_eq!(msg.device_id, "d1");
        assert_eq!(msg.rule_id, "r1");
        assert_eq!(msg.source, "ctrl+down");
        assert_eq!(msg.platform, "macos");

        let remote = parse_remote_message(full, "d2").unwrap().unwrap();
        assert_eq!(remote.source.key, "down");
        assert_eq!(remote.source.mask(), MOD_CTRL);
        // 旧版本消息缺 target / platform：宽容降级（仅本机规则可命中）
        let legacy = r#"{"deviceId":"d1","ruleId":"r1","source":"ctrl+down","ts":123}"#;
        let remote = parse_remote_message(legacy, "d2").unwrap().unwrap();
        assert_eq!(remote.source.key, "down");
        assert!(remote.target.is_none());
        assert!(remote.platform.is_empty());
    }

    #[test]
    fn remote_message_ignores_own_device() {
        let json = r#"{"deviceId":"d1","source":"ctrl+down","ts":1}"#;
        assert!(parse_remote_message(json, "d1").unwrap().is_none());
    }

    #[test]
    fn remote_message_rejects_malformed() {
        assert!(parse_remote_message("not json", "me").is_err());
        assert!(parse_remote_message(r#"{"source":"ctrl+down"}"#, "me").is_err());
        assert!(parse_remote_message(r#"{"deviceId":"d","source":"ctrl++"}"#, "me").is_err());
        // target 非法不整条报错：宽容降级为 None（仍可走本机规则）
        let bad_target = parse_remote_message(
            r#"{"deviceId":"d","source":"ctrl+down","target":{"key":"Space"}}"#,
            "me",
        )
        .unwrap()
        .unwrap();
        assert!(bad_target.target.is_none());
    }

    #[test]
    fn resolve_remote_local_rule_wins() {
        // 本机同源规则命中且平台适用 → 本地映射优先（不用远端 target）
        let rules = vec![rule("local", true, "f9", "F10", PLATFORM_ALL, "本机")];
        let remote = RemoteKeySync {
            source: combo("f9"),
            target: Some(combo("F12")),
            platform: PLATFORM_MACOS.into(),
        };
        assert_eq!(
            resolve_remote_action(&remote, &rules, "macos"),
            RemoteAction::Local(rules[0].clone())
        );
    }

    #[test]
    fn resolve_remote_fallback_injects_remote_target() {
        // 本机无同源规则 + 消息 platform 含本机 → 按发送端映射注入（单向生效）
        let remote = RemoteKeySync {
            source: combo("ctrl+down"),
            target: Some(combo("F9")),
            platform: PLATFORM_MACOS.into(),
        };
        assert_eq!(
            resolve_remote_action(&remote, &[], "macos"),
            RemoteAction::Remote(combo("F9"))
        );
        // 平台不符（仅 macOS 的映射，Windows 设备不直注）
        assert_eq!(
            resolve_remote_action(&remote, &[], "windows"),
            RemoteAction::NoLocalRule
        );
        // platform 缺省 / 非法 → 不直注
        assert_eq!(
            resolve_remote_action(
                &RemoteKeySync {
                    source: combo("ctrl+down"),
                    target: Some(combo("F9")),
                    platform: String::new(),
                },
                &[],
                "macos"
            ),
            RemoteAction::NoLocalRule
        );
        // 无 remote target（旧消息）→ 忽略
        assert_eq!(
            resolve_remote_action(
                &RemoteKeySync {
                    source: combo("ctrl+down"),
                    target: None,
                    platform: PLATFORM_ALL.into(),
                },
                &[],
                "macos"
            ),
            RemoteAction::NoLocalRule
        );
    }

    #[test]
    fn resolve_remote_local_platform_mismatch_never_falls_back() {
        // 本机虽有同源规则但平台不含本机：明确拒绝，不回退远端映射
        let rules = vec![rule("win-only", true, "f9", "F10", PLATFORM_WINDOWS, "x")];
        let remote = RemoteKeySync {
            source: combo("f9"),
            target: Some(combo("F12")),
            platform: PLATFORM_ALL.into(),
        };
        assert_eq!(
            resolve_remote_action(&remote, &rules, "macos"),
            RemoteAction::PlatformMismatch {
                platform: PLATFORM_WINDOWS.into()
            }
        );
    }

    #[test]
    fn publish_dedupe_within_window() {
        let state = KeySyncState::default();
        assert!(state.may_publish_at("ctrl+down", 1_000));
        state.record_published_at("ctrl+down", 1_000);
        // 同键 300ms 内去重（含边界）
        assert!(!state.may_publish_at("ctrl+down", 1_000 + DEDUPE_MS - 1));
        assert!(!state.may_publish_at("ctrl+down", 1_000 + DEDUPE_MS));
        // 过窗后放行
        assert!(state.may_publish_at("ctrl+down", 1_000 + DEDUPE_MS + 1));
        // 其他组合不受影响
        assert!(state.may_publish_at("ctrl+up", 1_001));
    }

    #[test]
    fn injection_suppresses_publish_within_window() {
        let state = KeySyncState::default();
        state.mark_injected_at("f9", 0);
        assert!(!state.may_publish_at("f9", 200));
        assert!(!state.may_publish_at("f9", 299));
        assert!(!state.may_publish_at("f9", 300));
        // 超过 300ms 抑制窗后不再拦截
        assert!(state.may_publish_at("f9", 301));
        // 其他组合不受影响
        assert!(state.may_publish_at("f10", 200));
    }

    #[test]
    fn offline_notice_is_throttled() {
        let state = KeySyncState::default();
        assert_eq!(*state.offline_notice.lock().unwrap(), 0);
        assert!(state.take_offline_notice());
        assert!(!state.take_offline_notice());
    }

    #[test]
    fn display_key_coverage() {
        assert_eq!(display_key("a"), "A");
        assert_eq!(display_key("f9"), "F9");
        assert_eq!(display_key("up"), "↑");
        assert_eq!(display_key("pageUp"), "PageUp");
        assert_eq!(display_key("home"), "Home");
        assert_eq!(display_key("9"), "9");
    }

    /// 引擎未启动 / 已停止时 stop 幂等，不 panic、不挂起
    #[test]
    fn engine_stop_is_idempotent_without_start() {
        stop_engine();
        stop_engine();
    }
}
