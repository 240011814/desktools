/* 按键同步（纯函数，便于单元测试：scripts/keysync.test.ts）
 *
 * 与 src-tauri/src/key_sync.rs 的纯逻辑段保持语义一致：
 * - 键名归一化（camelCase 规范名）与别名集合一致；
 * - 组合串 canonical（消息里的「源组合」）与显示名一致；
 * - 规则校验规则一致（平台字段 / 启用规则的组合完整性）。
 * 主键仅支持：字母 / 数字 / F1~F12 / 方向 / pageUp / pageDown / home / end。
 * 后端保存时还会再校验一次（key_sync::validate_rules），这里的前置校验只为减少往返。
 */

export const PLATFORM_WINDOWS = "windows";
export const PLATFORM_MACOS = "macos";
export const PLATFORM_ALL = "all";

/** 修饰键掩码（与 Rust MOD_CTRL/MOD_ALT/MOD_SHIFT/MOD_META 对应） */
export const MOD_CTRL = 1;
export const MOD_ALT = 2;
export const MOD_SHIFT = 4;
export const MOD_META = 8;

export interface KeyCombo {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  /** Super / Win / Cmd */
  meta: boolean;
  /** 规范化命名主键（camelCase，见 normalizeKeyName） */
  key: string;
}

export interface KeySyncRule {
  id: string;
  enabled: boolean;
  /** 源组合：任一同组设备按下后触发 */
  source: KeyCombo;
  /** 目标组合：本机命中规则后注入 */
  target: KeyCombo;
  /** 目标平台：windows / macos / all */
  platform: string;
  /** 备注（可选，也用于错误信息里定位规则） */
  note: string;
}

export function emptyCombo(): KeyCombo {
  return { ctrl: false, alt: false, shift: false, meta: false, key: "" };
}

export function comboMask(combo: KeyCombo): number {
  let mask = 0;
  if (combo.ctrl) mask |= MOD_CTRL;
  if (combo.alt) mask |= MOD_ALT;
  if (combo.shift) mask |= MOD_SHIFT;
  if (combo.meta) mask |= MOD_META;
  return mask;
}

/** 由「主键 + 修饰键掩码」构造规范化组合串（录制、匹配共用） */
export function canonicalFrom(key: string, mask: number): string {
  const parts: string[] = [];
  if (mask & MOD_CTRL) parts.push("ctrl");
  if (mask & MOD_ALT) parts.push("alt");
  if (mask & MOD_SHIFT) parts.push("shift");
  if (mask & MOD_META) parts.push("super");
  parts.push(key);
  return parts.join("+");
}

export function comboCanonical(combo: KeyCombo): string {
  return canonicalFrom(combo.key, comboMask(combo));
}

/** 主键显示名（方向键用箭头符号，字母 / F 系列转大写） */
export function displayKey(key: string): string {
  switch (key) {
    case "up":
      return "↑";
    case "down":
      return "↓";
    case "left":
      return "←";
    case "right":
      return "→";
    case "pageUp":
      return "PageUp";
    case "pageDown":
      return "PageDown";
    case "home":
      return "Home";
    case "end":
      return "End";
    default: {
      if (/^[a-z]$/.test(key) || /^f\d+$/.test(key)) return key.toUpperCase();
      return key;
    }
  }
}

/** 修饰键显示名（按平台：Alt/Option、Win/Cmd/Super） */
function modifierParts(combo: KeyCombo, os: string): string[] {
  const parts: string[] = [];
  if (combo.ctrl) parts.push("Ctrl");
  if (combo.alt) parts.push(os === PLATFORM_MACOS ? "Option" : "Alt");
  if (combo.shift) parts.push("Shift");
  if (combo.meta) {
    parts.push(
      os === PLATFORM_MACOS ? "Cmd" : os === PLATFORM_WINDOWS ? "Win" : "Super",
    );
  }
  return parts;
}

/** 按平台显示组合（Windows 用 Win、macOS 用 Cmd/Option、其余用 Super） */
export function displayCombo(combo: KeyCombo, os: string): string {
  return [...modifierParts(combo, os), displayKey(combo.key)].join("+");
}

/** 仅修饰键的前缀文本（无主键时显示，如「Ctrl+」「Ctrl+Shift+」；
 * 速选修饰键后等待点选主键，保证仍可被 parseModifierPrefix 解析） */
export function modifierPrefixText(combo: KeyCombo, os: string): string {
  const parts = modifierParts(combo, os);
  return parts.length ? `${parts.join("+")}+` : "";
}

/** 切换组合中的某个修饰键（保留主键与其余修饰键），返回新组合。
 * 供「主键速选」旁的修饰键按钮叠加 / 取消使用（如点 Ctrl 再点 ↓ → Ctrl+↓） */
export function withModifierToggled(combo: KeyCombo, mask: number): KeyCombo {
  return {
    ctrl: mask & MOD_CTRL ? !combo.ctrl : combo.ctrl,
    alt: mask & MOD_ALT ? !combo.alt : combo.alt,
    shift: mask & MOD_SHIFT ? !combo.shift : combo.shift,
    meta: mask & MOD_META ? !combo.meta : combo.meta,
    key: combo.key,
  };
}

/** 主键名归一化：仅支持字母 / 数字 / F1~F12 / 方向 / PageUp / PageDown / Home / End。
 * 接受常见别名（ArrowUp、Page Up、PgUp、PgDn、↓ 等），返回 camelCase 规范名；
 * 其余按键（Space / Enter / 标点等）不支持，返回 null。与 Rust normalize_key_name 一致。 */
export function normalizeKeyName(raw: string): string | null {
  const t = raw.trim();
  if (!t) return null;
  // 箭头写法（录制输入 / 手动输入容错）
  if (t === "↑") return "up";
  if (t === "↓") return "down";
  if (t === "←") return "left";
  if (t === "→") return "right";
  const folded = t.replace(/ /g, "").toLowerCase();
  // 单字符：字母 / 数字
  if (folded.length === 1) {
    if (/^[a-z0-9]$/.test(folded)) return folded;
    return null; // 单字符的非字母数字（标点等）不支持
  }
  // F1 ~ F12
  if (folded.startsWith("f")) {
    const num = folded.slice(1);
    if (!/^\d+$/.test(num)) return null;
    const n = Number(num);
    return n === 0 || n > 12 || !Number.isSafeInteger(n) ? null : `f${n}`;
  }
  // 方向 / 翻页 / Home / End
  const stripped = folded.startsWith("arrow") ? folded.slice(5) : folded;
  switch (stripped) {
    case "up":
      return "up";
    case "down":
      return "down";
    case "left":
      return "left";
    case "right":
      return "right";
    case "pageup":
    case "pgup":
      return "pageUp";
    case "pagedown":
    case "pgdown":
    case "pgdwn":
    case "pgdn":
      return "pageDown";
    case "home":
      return "home";
    case "end":
      return "end";
    default:
      return null;
  }
}

/** 修饰键名归一化（Ctrl / Alt / Shift / Super·Win·Cmd）：返回掩码位，未知返回 0 */
export function modifierMaskOf(raw: string): number {
  const folded = raw.replace(/ /g, "").toLowerCase();
  switch (folded) {
    case "ctrl":
    case "control":
    case "⌃":
      return MOD_CTRL;
    case "alt":
    case "option":
    case "opt":
    case "⌥":
      return MOD_ALT;
    case "shift":
    case "⇧":
      return MOD_SHIFT;
    case "super":
    case "meta":
    case "win":
    case "windows":
    case "cmd":
    case "command":
    case "⌘":
      return MOD_META;
    default:
      return 0;
  }
}

/** 解析「未完成前缀」组合文本：`Ctrl+` / `Ctrl+Shift+`（只有修饰键、还没有主键）。
 * 供输入框「主键速选」保留已录修饰键用；完整组合请用 parseComboText。
 * 仅接受以 + 结尾的明确未完成写法；含非法段返回 null */
export function parseModifierPrefix(input: string): KeyCombo | null {
  const trimmed = input.trim();
  if (!trimmed || !trimmed.endsWith("+")) return null;
  const combo = emptyCombo();
  let any = false;
  for (const part of trimmed.slice(0, -1).split("+")) {
    const mask = modifierMaskOf(part.trim());
    if (mask === 0) return null;
    if (mask & MOD_CTRL) combo.ctrl = true;
    if (mask & MOD_ALT) combo.alt = true;
    if (mask & MOD_SHIFT) combo.shift = true;
    if (mask & MOD_META) combo.meta = true;
    any = true;
  }
  return any ? combo : null;
}

/** 解析组合文本（手动编辑 / 录制回填统一入口）：`Ctrl+Shift+↓` / `alt+f9` / `Cmd+Down` */
export function parseComboText(input: string): KeyCombo | string {
  const parts = input
    .trim()
    .split("+")
    .map((p) => p.trim())
    .filter((p) => p.length > 0);
  if (parts.length === 0) return "按键组合为空";
  const main = parts[parts.length - 1]!;
  const mods = parts.slice(0, -1);
  const combo = emptyCombo();
  for (const part of mods) {
    const mask = modifierMaskOf(part);
    if (mask === 0) {
      return `无法识别的修饰键「${part}」（支持 Ctrl / Alt / Shift / Win·Cmd，用 + 分隔）`;
    }
    if (mask & MOD_CTRL) combo.ctrl = true;
    if (mask & MOD_ALT) combo.alt = true;
    if (mask & MOD_SHIFT) combo.shift = true;
    if (mask & MOD_META) combo.meta = true;
  }
  const key = normalizeKeyName(main);
  if (!key) {
    if (modifierMaskOf(main) !== 0) {
      // 「Ctrl+」「Cmd+Shift+」这类未完成写法：别让用户误以为修饰键本身不合法
      return `组合缺少主键：「${main}」是修饰键，请再加一个主键（如 ${main}+F9 或 ${main}+↓）`;
    }
    return `无法识别的主键「${main}」（支持字母 / 数字 / F1~F12 / 方向键 / PageUp / PageDown / Home / End）`;
  }
  combo.key = key;
  return combo;
}

/** 校验（结构化）组合：主键必须存在且已是规范化写法。返回 null = 合法 */
export function comboError(combo: KeyCombo): string | null {
  if (!combo.key) {
    return "组合缺少主键（需为字母 / 数字 / F1~F12 / 方向键 / PageUp / PageDown / Home / End）";
  }
  const canonical = normalizeKeyName(combo.key);
  if (!canonical) {
    return `主键「${combo.key}」不受支持（需为字母 / 数字 / F1~F12 / 方向键 / PageUp / PageDown / Home / End）`;
  }
  if (canonical !== combo.key) {
    return `主键写法不规范，应为「${displayKey(canonical)}」`;
  }
  return null;
}

/** 校验单个规则：平台字段必须合法；启用中的规则组合必须完整合法（禁用规则允许留空 / 非法）。返回 null = 合法 */
export function validateRule(rule: KeySyncRule, index = 0): string | null {
  const label = rule.note.trim() ? `「${rule.note.trim()}」` : `第 ${index + 1} 条`;
  if (rule.platform !== PLATFORM_WINDOWS && rule.platform !== PLATFORM_MACOS && rule.platform !== PLATFORM_ALL) {
    return `规则 ${label}：目标平台需为「全部 / 仅 Windows / 仅 macOS」`;
  }
  if (!rule.enabled) return null;
  const sourceErr = comboError(rule.source);
  if (sourceErr) return `规则 ${label}：源组合无效：${sourceErr}`;
  const targetErr = comboError(rule.target);
  if (targetErr) return `规则 ${label}：目标组合无效：${targetErr}`;
  return null;
}

/** 规则的目标平台是否包含本机 */
export function platformApplies(rule: KeySyncRule, os: string): boolean {
  switch (rule.platform) {
    case PLATFORM_WINDOWS:
      return os === PLATFORM_WINDOWS;
    case PLATFORM_MACOS:
      return os === PLATFORM_MACOS;
    default:
      return true;
  }
}

/* ---------- 录制（keydown → 组合串） ---------- */

const MODIFIER_ONLY_KEYS = new Set([
  "Control",
  "Alt",
  "AltGraph",
  "Shift",
  "Meta",
  "CapsLock",
  "NumLock",
  "ScrollLock",
  "Fn",
  "FnLock",
  "Hyper",
  "Super",
  "Symbol",
  "SymbolLock",
]);

/** 该键是「只按修饰键」的事件（尚未构成组合，继续等主键） */
export function isModifierOnlyKey(e: Pick<KeyboardEvent, "key">): boolean {
  return MODIFIER_ONLY_KEYS.has(e.key);
}

/** KeyboardEvent.key → 规范化主键；失败时用 event.code 兜底
 * （macOS Option+字母的 key 是特殊字符「é」，Windows 非美式布局 key 可能是本地字符） */
function mainKeyOfEvent(e: Pick<KeyboardEvent, "key" | "code">): string | null {
  const fromKey = normalizeKeyName(e.key);
  if (fromKey) return fromKey;
  const letter = /^Key([A-Z])$/.exec(e.code);
  if (letter) return letter[1]!.toLowerCase();
  const digit = /^Digit([0-9])$/.exec(e.code);
  if (digit) return normalizeKeyName(digit[1]!);
  return normalizeKeyName(e.code); // F9 / ArrowUp / PageUp / Home / End
}

/** 从 keydown 事件构造组合串（无主键 / 不支持的主键返回 null，继续录制 / 交给默认行为） */
export function comboFromEvent(e: Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">): string | null {
  if (isModifierOnlyKey(e)) return null;
  const key = mainKeyOfEvent(e);
  if (!key) return null;
  let mask = 0;
  if (e.ctrlKey) mask |= MOD_CTRL;
  if (e.altKey) mask |= MOD_ALT;
  if (e.shiftKey) mask |= MOD_SHIFT;
  if (e.metaKey) mask |= MOD_META;
  return canonicalFrom(key, mask);
}

/** 录制中的预览：仅修饰键时的提示，如「Ctrl+Cmd+…」（os 传当前平台名） */
export function previewCombo(
  e: Pick<KeyboardEvent, "ctrlKey" | "altKey" | "shiftKey" | "metaKey">,
  os: string,
): string {
  const parts: string[] = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push(os === PLATFORM_MACOS ? "Option" : "Alt");
  if (e.shiftKey) parts.push("Shift");
  if (e.metaKey) parts.push(os === PLATFORM_MACOS ? "Cmd" : os === PLATFORM_WINDOWS ? "Win" : "Super");
  if (parts.length === 0) return "";
  return `${parts.join("+")}+…`;
}
