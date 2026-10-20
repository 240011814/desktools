/* 按键同步纯逻辑的单元测试（与 src-tauri/src/key_sync.rs 的 mod tests 对应）
 *
 * 运行：pnpm test:unit（Node 24 原生 TypeScript 类型剥离 + 内置 test runner）
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  canonicalFrom,
  comboCanonical,
  comboError,
  comboFromEvent,
  comboMask,
  displayCombo,
  emptyCombo,
  isModifierOnlyKey,
  MOD_CTRL,
  MOD_META,
  MOD_SHIFT,
  normalizeKeyName,
  parseComboText,
  platformApplies,
  PLATFORM_ALL,
  PLATFORM_MACOS,
  PLATFORM_WINDOWS,
  previewCombo,
  validateRule,
  type KeySyncRule,
} from "../src/keysync.ts";

/** Rust 测试里的 combo() 帮手：解析失败直接抛错 */
function combo(text: string) {
  const parsed = parseComboText(text);
  if (typeof parsed === "string") throw new Error(parsed);
  return parsed;
}

/** Rust 测试里的 rule() 帮手 */
function rule(
  enabled: boolean,
  source: string,
  target: string,
  platform: string,
  note: string,
): KeySyncRule {
  return { id: note, enabled, source: combo(source), target: combo(target), platform, note };
}

/** 键名原样承载（不解析）——构造「非法组合」的规则用 */
function rawRule(
  enabled: boolean,
  sourceKey: string,
  targetKey: string,
  platform: string,
  note: string,
): KeySyncRule {
  return {
    id: note,
    enabled,
    source: { ...emptyCombo(), key: sourceKey },
    target: { ...emptyCombo(), key: targetKey },
    platform,
    note,
  };
}

/* ---------- 键名归一化 ---------- */

test("normalize: 常见别名归一为 camelCase 规范名", () => {
  assert.equal(normalizeKeyName("PageUp"), "pageUp");
  assert.equal(normalizeKeyName("PgUp"), "pageUp");
  assert.equal(normalizeKeyName("Page Up"), "pageUp");
  assert.equal(normalizeKeyName("PgDn"), "pageDown");
  assert.equal(normalizeKeyName("pagedown"), "pageDown");
  assert.equal(normalizeKeyName("ArrowLeft"), "left");
  assert.equal(normalizeKeyName("←"), "left");
  assert.equal(normalizeKeyName("↓"), "down");
  assert.equal(normalizeKeyName("Home"), "home");
  assert.equal(normalizeKeyName("End"), "end");
  assert.equal(normalizeKeyName("F9"), "f9");
  assert.equal(normalizeKeyName("f12"), "f12");
  assert.equal(normalizeKeyName("A"), "a");
  assert.equal(normalizeKeyName("9"), "9");
});

test("normalize: F1~F12 之外的 F 键与非法键不支持", () => {
  assert.equal(normalizeKeyName("F13"), null);
  assert.equal(normalizeKeyName("f0"), null);
  assert.equal(normalizeKeyName("Space"), null);
  assert.equal(normalizeKeyName("Enter"), null);
  assert.equal(normalizeKeyName("é"), null);
  assert.equal(normalizeKeyName("f#9"), null);
  assert.equal(normalizeKeyName("  "), null);
});

/* ---------- 组合解析 ---------- */

test("parse: 常见写法统一为规范化组合", () => {
  const ctrlShiftDown = combo("Ctrl+Shift+Down");
  assert.equal(ctrlShiftDown.key, "down");
  assert.equal(ctrlShiftDown.ctrl, true);
  assert.equal(ctrlShiftDown.shift, true);
  assert.deepEqual(combo("alt+f9"), combo("Option+F9"));
  assert.equal(combo("Cmd+Down").meta, true);
  assert.equal(combo("Alt+PgDn").key, "pageDown");
  assert.deepEqual(combo("Ctrl+Page Up"), combo("Ctrl+PageUp"));
});

test("parse: 变体写法（Rust parse_combo_variants 对应）", () => {
  assert.deepEqual(combo("Ctrl+Shift+↓"), combo("ctrl+shift+down"));
  assert.deepEqual(combo("⌘+F9"), combo("meta+f9"));
  assert.deepEqual(combo("Cmd+F9"), combo("Win+F9"));
  assert.ok(parseComboText("win+down") !== null);
  assert.ok(parseComboText("ctrl+win+down") !== null);
});

test("parse: 非法输入报错（Rust parse_combo_rejects_invalid 对应）", () => {
  assert.equal(parseComboText("   "), "按键组合为空");
  assert.match(String(parseComboText("ctrl++")), /无法识别的主键「ctrl」/);
  assert.match(String(parseComboText("Ctrl+Space")), /无法识别的主键/);
  assert.match(String(parseComboText("Foo+Down")), /无法识别的修饰键/);
  assert.match(String(parseComboText("Alt+ArrowLeft&&")), /无法识别的主键/);
});

/* ---------- 规范串与显示 ---------- */

test("canonical: 规范串稳定且与掩码互换（Rust canonical_from 对应）", () => {
  assert.equal(comboCanonical(combo("Ctrl+Shift+Down")), "ctrl+shift+down");
  assert.equal(canonicalFrom("down", MOD_CTRL), "ctrl+down");
  assert.equal(
    comboCanonical(combo("Ctrl+Down")),
    canonicalFrom("down", comboMask(combo("Ctrl+Down"))),
  );
  assert.equal(comboMask(combo("Ctrl+Shift+F1")), MOD_CTRL | MOD_SHIFT);
  assert.equal(comboMask(combo("Cmd+Down")), MOD_META);
});

test("display: 按平台显示（Rust display 对应）", () => {
  assert.equal(displayCombo(combo("Ctrl+Shift+Down"), "windows"), "Ctrl+Shift+↓");
  assert.equal(displayCombo(combo("Ctrl+Shift+Down"), "macos"), "Ctrl+Shift+↓");
  assert.equal(displayCombo(combo("Cmd+F9"), "macos"), "Cmd+F9");
  assert.equal(displayCombo(combo("Cmd+F9"), "windows"), "Win+F9");
  assert.equal(displayCombo(combo("alt+f1"), "macos"), "Option+F1");
  assert.equal(displayCombo(combo("alt+f1"), "windows"), "Alt+F1");
  assert.equal(displayCombo(combo("pgdn"), "windows"), "PageDown");
});

/* ---------- 结构化组合校验 ---------- */

test("comboError: 结构化组合的非法情况", () => {
  assert.match(comboError(emptyCombo())!, /组合缺少主键/);
  assert.match(comboError({ ...emptyCombo(), key: "Enter" })!, /不受支持/);
  assert.match(comboError({ ...emptyCombo(), key: "PAGEUP" })!, /写法不规范/);
  assert.equal(comboError(combo("Ctrl+Down")), null);
});

/* ---------- 规则校验 / 平台过滤 ---------- */

test("validateRule: 平台字段与启用规则完整性（Rust validate_rules 对应）", () => {
  assert.match(validateRule(rawRule(true, "F9", "ctrl+down", "bogus", "x"), 0)!, /目标平台/);
  assert.match(validateRule(rawRule(true, "", "ctrl+down", PLATFORM_ALL, "y"), 0)!, /源组合无效/);
  assert.match(validateRule(rawRule(true, "f9", "Space", PLATFORM_ALL, "z"), 1)!, /目标组合无效/);
  // 禁用规则允许非法组合
  assert.equal(validateRule(rawRule(false, "", "", PLATFORM_ALL, "w"), 0), null);
  // 启用规则的组合合法时通过
  assert.equal(validateRule(rule(true, "F9", "Ctrl+Down", PLATFORM_ALL, "v"), 0), null);
});

test("validateRule: 错误信息用备注 / 序号定位（Rust rule_label 对应）", () => {
  const noteErr = validateRule(rawRule(true, "Space", "ctrl+down", PLATFORM_ALL, "映射1"), 3);
  assert.match(noteErr!, /^规则 「映射1」/);
  const numberedErr = validateRule(rawRule(true, "Space", "ctrl+down", PLATFORM_ALL, "  "), 3);
  assert.match(numberedErr!, /^规则 第 4 条/);
});

test("platformApplies: 目标平台过滤", () => {
  const os = "macos";
  assert.equal(platformApplies(rule(true, "F9", "F10", PLATFORM_MACOS, "r"), os), true);
  assert.equal(platformApplies(rule(true, "F9", "F10", PLATFORM_WINDOWS, "r"), os), false);
  assert.equal(platformApplies(rule(true, "F9", "F10", PLATFORM_ALL, "r"), os), true);
});

/* ---------- fromEvent（录制） ---------- */

function fakeEvent(
  key: string,
  opts: Partial<{ code: string; ctrlKey: boolean; altKey: boolean; shiftKey: boolean; metaKey: boolean }> = {},
) {
  return {
    key,
    code: opts.code ?? key,
    ctrlKey: opts.ctrlKey ?? false,
    altKey: opts.altKey ?? false,
    shiftKey: opts.shiftKey ?? false,
    metaKey: opts.metaKey ?? false,
  };
}

test("comboFromEvent: 修饰键 + 主键", () => {
  assert.equal(comboFromEvent(fakeEvent("ArrowDown", { code: "ArrowDown", ctrlKey: true })), "ctrl+down");
  assert.equal(comboFromEvent(fakeEvent("a", { code: "KeyA", ctrlKey: true })), "ctrl+a");
  assert.equal(comboFromEvent(fakeEvent("F9", { code: "F9", altKey: true })), "alt+f9");
  assert.equal(
    comboFromEvent(fakeEvent("ArrowDown", { code: "ArrowDown", ctrlKey: true, shiftKey: true })),
    "ctrl+shift+down",
  );
});

test("comboFromEvent: macOS Option+字母的 key 特殊字符用 code 兜底", () => {
  // Option+E 的 key 是「é」，code 是 KeyE
  assert.equal(comboFromEvent(fakeEvent("é", { code: "KeyE", altKey: true })), "alt+e");
});

test("comboFromEvent: 修饰键单独按下 / 不支持的键返回 null（继续录制）", () => {
  assert.equal(comboFromEvent(fakeEvent("Control", { code: "ControlLeft", ctrlKey: true })), null);
  assert.equal(comboFromEvent(fakeEvent("Shift", { code: "ShiftLeft", shiftKey: true })), null);
  assert.equal(comboFromEvent(fakeEvent("Space", { code: "Space" })), null);
});

test("isModifierOnlyKey", () => {
  assert.equal(isModifierOnlyKey({ key: "Meta" }), true);
  assert.equal(isModifierOnlyKey({ key: "CapsLock" }), true);
  assert.equal(isModifierOnlyKey({ key: "f" }), false);
});

test("previewCombo: 仅修饰键时给出提示", () => {
  assert.equal(
    previewCombo({ ctrlKey: true, shiftKey: true, altKey: false, metaKey: false }, "windows"),
    "Ctrl+Shift+…",
  );
  assert.equal(
    previewCombo({ ctrlKey: false, altKey: true, shiftKey: false, metaKey: true }, "macos"),
    "Option+Cmd+…",
  );
  assert.equal(
    previewCombo({ ctrlKey: false, altKey: false, shiftKey: false, metaKey: false }, "windows"),
    "",
  );
});
