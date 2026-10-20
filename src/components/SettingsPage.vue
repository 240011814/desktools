<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { platform } from "@tauri-apps/plugin-os";
import { mqttStatus, type KeyCombo, type KeySyncRule, type MqttConfig, type MinioConfig } from "../store";
import {
  comboFromEvent,
  displayCombo,
  emptyCombo,
  isModifierOnlyKey,
  parseComboText,
  parseModifierPrefix,
  previewCombo,
  validateRule,
} from "../keysync";
import { useToast } from "../toast";

/* ---------------- 开机自启 ---------------- */
interface AutostartState {
  /** 系统实际是否已注册开机自启 */
  enabled: boolean;
  /** 当前构建是否管理开机自启（debug 构建为 false） */
  managed: boolean;
}

const autoStart = ref(false);
const autoStartManaged = ref(true);
const autoStartBusy = ref(false);
const autoStartError = ref("");

/** 加载系统实际状态（打开设置页时调用，避免开关状态与实际不符） */
async function loadAutoStart() {
  try {
    const state = await invoke<AutostartState>("get_autostart_state");
    autoStart.value = state.enabled;
    autoStartManaged.value = state.managed;
  } catch {
    /* 浏览器预览等无 Tauri 环境时保持默认值 */
  }
}

async function toggleAutoStart() {
  if (autoStartBusy.value || !autoStartManaged.value) return;
  autoStartBusy.value = true;
  autoStartError.value = "";
  try {
    const state = await invoke<AutostartState>("set_autostart", {
      enabled: !autoStart.value,
    });
    autoStart.value = state.enabled;
    autoStartManaged.value = state.managed;
  } catch (e) {
    autoStartError.value = String(e);
    await loadAutoStart(); // 失败时回读系统实际状态
  } finally {
    autoStartBusy.value = false;
  }
}

/* ---------------- 剪贴板同步（MQTT） ---------------- */
const form = reactive<MqttConfig>({
  host: "",
  port: 1883,
  username: "",
  password: "",
  tls: false,
  group: "",
  deviceId: "",
  autoLock: false,
  autoLockDelaySecs: 15,
  // 锁屏同步（收发分离 + 活跃守卫）
  lockSyncPublish: false,
  lockSyncFollow: false,
  lockSyncIdleGuardSecs: 300,
  // 按键同步
  keySyncEnabled: false,
  keySyncRules: [],
  autoStart: true,
  minio: { host: "", port: 9000, bucket: "hub", tls: false, accessKey: "", secretKey: "" },
});

const saving = ref(false);
const configError = ref("");
const configSaved = ref(false);

const topicPreview = computed(
  () => `texthub/${form.group.trim() || "<group号>"}`,
);

async function loadConfig() {
  try {
    const cfg = await invoke<MqttConfig>("get_mqtt_config");
    Object.assign(form, cfg);
    rows.value = buildRows(cfg.keySyncRules ?? []);
  } catch {
    /* 浏览器预览等无 Tauri 环境时使用默认值 */
  }
}

async function saveConfig() {
  if (saving.value) return;
  saving.value = true;
  configError.value = "";
  configSaved.value = false;
  try {
    const saved = await invoke<MqttConfig>("save_mqtt_config", { cfg: { ...form } });
    Object.assign(form, saved);
    configSaved.value = true;
    setTimeout(() => (configSaved.value = false), 4000);
  } catch (e) {
    configError.value = String(e);
  } finally {
    saving.value = false;
  }
}

/* ---------------- 文件中转站（MinIO） ---------------- */
const minioBusy = ref(false);
const minioTesting = ref(false);
const minioError = ref("");
const minioSaved = ref("");

async function saveMinioConfig() {
  if (minioBusy.value) return;
  minioBusy.value = true;
  minioError.value = "";
  minioSaved.value = "";
  try {
    form.minio.port = Math.min(65535, Math.max(1, Number(form.minio.port) || 9000));
    const saved = await invoke<MinioConfig>("save_minio_config", {
      minio: { ...form.minio },
    });
    form.minio = saved;
    minioSaved.value = saved.host ? "MinIO 配置已保存" : "MinIO 配置已清除";
    setTimeout(() => (minioSaved.value = ""), 4000);
  } catch (e) {
    minioError.value = String(e);
  } finally {
    minioBusy.value = false;
  }
}

async function testMinioConnection() {
  if (minioTesting.value) return;
  minioTesting.value = true;
  minioError.value = "";
  minioSaved.value = "";
  try {
    minioSaved.value = await invoke<string>("minio_test_connection", {
      minio: { ...form.minio },
    });
    setTimeout(() => (minioSaved.value = ""), 6000);
  } catch (e) {
    minioError.value = String(e);
  } finally {
    minioTesting.value = false;
  }
}

/* ---------------- 离人自动锁屏 / 锁屏同步（窄接口保存开关） ---------------- */
/* 开关类设置走 set_auto_lock / set_lock_sync：以持久化配置为基只改目标字段,
   避免把表单里「已修改但未提交」的 MQTT 其他字段一并写入并重连 */
const csiTopic = "csi/notify";
const autoLockBusy = ref(false);
const autoLockError = ref("");
const autoLockSaved = ref(false);

async function saveAutoLockFlag(enabled = form.autoLock) {
  if (autoLockBusy.value) return;
  autoLockBusy.value = true;
  autoLockError.value = "";
  autoLockSaved.value = false;
  try {
    form.autoLockDelaySecs = Math.min(300, Math.max(1, Number(form.autoLockDelaySecs) || 15));
    const saved = await invoke<MqttConfig>("set_auto_lock", {
      enabled,
      delaySecs: form.autoLockDelaySecs,
    });
    Object.assign(form, saved);
    autoLockSaved.value = true;
    setTimeout(() => (autoLockSaved.value = false), 4000);
  } catch (e) {
    autoLockError.value = String(e);
    await loadConfig(); // 保存失败回读持久化配置,开关显示与实际状态对齐
  } finally {
    autoLockBusy.value = false;
  }
}

async function toggleAutoLock() {
  if (autoLockBusy.value) return;
  await saveAutoLockFlag(!form.autoLock);
}

/* ---------------- 锁屏同步（收发两个开关 + 活跃守卫阈值，走窄接口保存） ---------------- */
const lockSyncTopic = computed(() => `locksync/${form.group.trim() || "<group号>"}`);
const lockSyncBusy = ref(false);
const lockSyncError = ref("");
const lockSyncSaved = ref(false);

/** 钳制活跃守卫阈值：0~3600；清空 / 非法输入回落 300（守卫误关的代价是误锁，宁保守） */
function clampIdleGuardSecs() {
  const n = Math.round(Number(form.lockSyncIdleGuardSecs));
  form.lockSyncIdleGuardSecs = Number.isFinite(n) ? Math.min(3600, Math.max(0, n)) : 300;
}

async function saveLockSyncFlag(publish = form.lockSyncPublish, follow = form.lockSyncFollow) {
  if (lockSyncBusy.value) return;
  lockSyncBusy.value = true;
  lockSyncError.value = "";
  lockSyncSaved.value = false;
  try {
    clampIdleGuardSecs();
    const saved = await invoke<MqttConfig>("set_lock_sync", {
      publish,
      follow,
      idleGuardSecs: form.lockSyncIdleGuardSecs,
    });
    Object.assign(form, saved);
    lockSyncSaved.value = true;
    setTimeout(() => (lockSyncSaved.value = false), 4000);
  } catch (e) {
    lockSyncError.value = String(e);
    await loadConfig(); // 保存失败回读持久化配置,开关显示与实际状态对齐
  } finally {
    lockSyncBusy.value = false;
  }
}

async function togglePublish() {
  if (lockSyncBusy.value) return;
  await saveLockSyncFlag(!form.lockSyncPublish, form.lockSyncFollow);
}

async function toggleFollow() {
  if (lockSyncBusy.value) return;
  await saveLockSyncFlag(form.lockSyncPublish, !form.lockSyncFollow);
}

/* ---------------- 按键同步（窄接口 set_key_sync；规则 CRUD + 5s 防抖自动保存） ---------------- */
/* 文本框是「展示 / 录制」视图；真正保存的是最近一次解析成功的结构化组合：
   行内解析失败时保留旧组合并显示行错误，禁用的规则允许保留非法组合（与后端 validate_rules 一致） */
const keysyncToast = useToast();

interface KeysyncRow {
  id: string;
  enabled: boolean;
  sourceText: string;
  targetText: string;
  platform: string;
  note: string;
  sourceCombo: KeyCombo;
  targetCombo: KeyCombo;
  sourceError: string;
  targetError: string;
}

const rows = ref<KeysyncRow[]>([]);
const rowsDirty = ref(false);
const keySyncBusy = ref(false);
const keySyncError = ref("");
const keySyncSaved = ref(false);
let keySyncSaveTimer: number | undefined;

const keySyncTopic = computed(() => `keysync/${form.group.trim() || "<group号>"}`);

/** 按当前平台美化组合显示（Windows 用 Win，macOS 用 Cmd/Option） */
function comboText(combo: KeyCombo): string {
  return combo.key ? displayCombo(combo, osName) : "";
}

function buildRows(rules: KeySyncRule[]): KeysyncRow[] {
  return rules.map((rule) => ({
    id: rule.id || newUuid(),
    enabled: rule.enabled,
    sourceText: comboText(rule.source),
    targetText: comboText(rule.target),
    platform: rule.platform || "all",
    note: rule.note,
    sourceCombo: { ...rule.source },
    targetCombo: { ...rule.target },
    sourceError: "",
    targetError: "",
  }));
}

function newUuid(): string {
  return typeof crypto.randomUUID === "function" ? crypto.randomUUID() : `rule-${Date.now()}-${Math.random()}`;
}

function scheduleKeySyncSave() {
  rowsDirty.value = true;
  clearTimeout(keySyncSaveTimer);
  // 5s 防抖自动保存：连续编辑不打断输入；失败不自动重试（保持 UI + toast）
  keySyncSaveTimer = window.setTimeout(() => void saveKeySync(), 5000);
}

/** 把行文本解析进结构化组合；失败时保留旧组合并返回 false */
function parseRowField(row: KeysyncRow, field: "source" | "target"): boolean {
  const text = (field === "source" ? row.sourceText : row.targetText).trim();
  const err = field === "source" ? "sourceError" : "targetError";
  if (text === "") {
    // 未填写：保留旧结构化组合，清掉错误提示
    row[err] = "";
    return true;
  }
  const parsed = parseComboText(text);
  if (typeof parsed === "string") {
    row[err] = parsed;
    return false;
  }
  row[field === "source" ? "sourceCombo" : "targetCombo"] = parsed;
  row[err] = "";
  return true;
}

function restoreComboText(row: KeysyncRow, field: "source" | "target") {
  if (field === "source") row.sourceText = comboText(row.sourceCombo);
  else row.targetText = comboText(row.targetCombo);
}

/** 录制 / 解析成功后提交组合并触发防抖保存 */
function applyCombo(row: KeysyncRow, field: "source" | "target", combo: KeyCombo) {
  const slot = field === "source" ? "sourceCombo" : "targetCombo";
  const text = field === "source" ? "sourceText" : "targetText";
  const err = field === "source" ? "sourceError" : "targetError";
  if (JSON.stringify(row[slot]) !== JSON.stringify(combo)) {
    row[slot] = combo;
    row[text] = comboText(combo);
    scheduleKeySyncSave();
  } else {
    row[text] = comboText(combo); // 内容没变也要把预览残留换回来
  }
  row[err] = "";
}

/** keydown 录制：识别出的组合立即提交；修饰键单独按下时先显示预览；
 *  其余键（退格 / 方向键等）不拦截，留给手动编辑。Escape 取消恢复 */
function comboKeydown(e: KeyboardEvent, row: KeysyncRow, field: "source" | "target") {
  if (e.key === "Enter") {
    // 手动键入后回车 = 提交解析（与失焦一致）
    e.preventDefault();
    (e.target as HTMLInputElement).blur();
    return;
  }
  if (e.key === "Escape") {
    e.preventDefault();
    restoreComboText(row, field);
    if (field === "source") row.sourceError = "";
    else row.targetError = "";
    return;
  }
  const comboStr = comboFromEvent(e);
  if (comboStr) {
    // 仅修饰键（尚未构成组合）不提交；此处 comboStr 非空意味着已有主键
    const parsed = parseComboText(comboStr);
    if (typeof parsed !== "string") {
      // 无修饰键时只对 F 键直接录制（字母 / 方向键留给手动编辑，避免打字被吞）
      const isBareCombo = !parsed.ctrl && !parsed.alt && !parsed.shift && !parsed.meta;
      if (isBareCombo && !/^f\d+$/.test(parsed.key)) return; // 不拦截：交给手动编辑
      e.preventDefault();
      applyCombo(row, field, parsed);
    }
    return;
  }
  if (isModifierOnlyKey(e)) {
    e.preventDefault();
    const preview = previewCombo(e, osName);
    if (preview) {
      if (field === "source") row.sourceText = preview;
      else row.targetText = preview;
    }
  }
}

/** 修饰键松开且没按主键 → 恢复已设定组合的展示（预览残留清理） */
function comboKeyup(e: KeyboardEvent, row: KeysyncRow, field: "source" | "target") {
  if (!isModifierOnlyKey(e)) return;
  const text = (field === "source" ? row.sourceText : row.targetText).trim();
  if (text.endsWith("…")) restoreComboText(row, field);
}

/** 失焦：把当前文本解析提交；解析失败恢复旧组合展示 + 保留行错误 */
function comboBlur(row: KeysyncRow, field: "source" | "target") {
  const text = (field === "source" ? row.sourceText : row.targetText).trim();
  if (text.endsWith("…")) {
    restoreComboText(row, field);
    return;
  }
  if (text === "") {
    if (field === "source") row.sourceError = "";
    else row.targetError = "";
    return;
  }
  if (!parseRowField(row, field)) {
    restoreComboText(row, field); // 旧组合保留（行错误已标记原因）
    scheduleKeySyncSave();
  } else {
    // 解析成功但写法可能不规范（如 PgUp → PageUp），统一成规范展示
    const combo = field === "source" ? row.sourceCombo : row.targetCombo;
    const slot = field === "source" ? "sourceText" : "targetText";
    if (row[slot] !== comboText(combo)) {
      row[slot] = comboText(combo);
      scheduleKeySyncSave();
    }
  }
}

function addRow() {
  rows.value.push({
    id: newUuid(),
    enabled: true,
    sourceText: "",
    targetText: "",
    platform: "all",
    note: "",
    sourceCombo: { ...emptyCombo() },
    targetCombo: { ...emptyCombo() },
    sourceError: "",
    targetError: "",
  });
  scheduleKeySyncSave();
}

function removeRow(row: KeysyncRow) {
  rows.value = rows.value.filter((item) => item.id !== row.id);
  scheduleKeySyncSave();
}

/** 单条规则开关（禁用的规则不参与匹配，组合可保留非法写法） */
function toggleRowEnabled(row: KeysyncRow) {
  row.enabled = !row.enabled;
  scheduleKeySyncSave();
}

/** 主键速选集合（canonical 名 → 显示名） */
const QUICK_KEYS: Array<[string, string]> = [
  ["up", "↑"],
  ["down", "↓"],
  ["left", "←"],
  ["right", "→"],
  ["pageUp", "PgUp"],
  ["pageDown", "PgDn"],
  ["home", "Home"],
  ["end", "End"],
];

/** 点击主键速选：构造「输入框现有修饰键（或鼠标按着的修饰键）+ 该主键」的组合。
 * 修饰键来源优先级：鼠标点击时物理按着的 > 输入框文本（含 `Ctrl+` 未完前缀）> 无；
 * 框为空且未按修饰键时即产生单键映射（无修饰键） */
function pickQuickKey(
  e: MouseEvent,
  row: KeysyncRow,
  field: "source" | "target",
  keyName: string,
) {
  let combo: KeyCombo = { ...emptyCombo(), key: keyName };
  if (e.ctrlKey || e.altKey || e.shiftKey || e.metaKey) {
    combo.ctrl = e.ctrlKey;
    combo.alt = e.altKey;
    combo.shift = e.shiftKey;
    combo.meta = e.metaKey;
  } else {
    const slot = field === "source" ? "sourceText" : "targetText";
    const text = row[slot].trim();
    if (text.endsWith("…")) {
      // 录制预览残留（keyup 恢复被点击打断）不可靠：恢复原文并退出
      restoreComboText(row, field);
      return;
    }
    const prefix = parseModifierPrefix(text);
    if (prefix) {
      combo = { ...prefix, key: keyName };
    } else {
      const parsed = parseComboText(text);
      if (typeof parsed !== "string") combo = { ...parsed, key: keyName };
    }
  }
  applyCombo(row, field, combo);
  (e.currentTarget as HTMLElement | null)?.blur();
}

async function saveKeySync() {
  if (keySyncBusy.value) return;
  clearTimeout(keySyncSaveTimer);
  keySyncBusy.value = true;
  keySyncError.value = "";
  keySyncSaved.value = false;
  try {
    rows.value.forEach((row) => {
      parseRowField(row, "source");
      parseRowField(row, "target");
    });
    const rules: KeySyncRule[] = rows.value.map((row) => ({
      id: row.id,
      enabled: row.enabled,
      source: { ...row.sourceCombo },
      target: { ...row.targetCombo },
      platform: row.platform,
      note: row.note,
    }));
    for (let index = 0; index < rules.length; index++) {
      const err = validateRule(rules[index]!, index);
      if (err) {
        keySyncError.value = err;
        keysyncToast.showToast(err);
        rowsDirty.value = true;
        return;
      }
    }
    const result = await invoke<{ config: MqttConfig; warning: string }>("set_key_sync", {
      enabled: form.keySyncEnabled,
      rules,
    });
    Object.assign(form, result.config);
    rows.value = buildRows(result.config.keySyncRules ?? []);
    rowsDirty.value = false;
    keySyncSaved.value = true;
    setTimeout(() => (keySyncSaved.value = false), 4000);
    // macOS 缺权限等部分成功场景：后端已回退开关，前端弹提示
    if (result.warning) keysyncToast.showToast(result.warning);
  } catch (e) {
    keySyncError.value = String(e);
    keysyncToast.showToast(String(e));
    // 保存失败回读持久化配置，行视图与实际状态对齐（不自动重试，避免错误循环）
    await loadConfig();
    rowsDirty.value = false;
  } finally {
    keySyncBusy.value = false;
  }
}

async function toggleKeySync() {
  if (keySyncBusy.value) return;
  form.keySyncEnabled = !form.keySyncEnabled;
  await saveKeySync();
}

onUnmounted(() => {
  clearTimeout(keySyncSaveTimer);
  // 切换页面时若还有未保存改动，立即落一次（失败静默，配置仍在）
  if (rowsDirty.value && !keySyncBusy.value) void saveKeySync();
});

/* ---------------- 桌面右键菜单集成 ---------------- */
interface IntegrationStatus {
  supported: boolean;
  enabled: boolean;
  detail: string;
}

const integration = ref<IntegrationStatus>({ supported: false, enabled: false, detail: "" });
const integrationBusy = ref(false);
const integrationError = ref("");

const osName = (() => {
  try {
    return platform();
  } catch {
    return "unknown";
  }
})();

const integrationTitle = osName === "windows" ? "桌面右键菜单" : "Finder 快速操作";
const integrationDesc =
  osName === "windows"
    ? "在桌面空白处右键 →「剪贴板历史」/「文件中转站」，直接弹出对应窗口（Windows 11 需点「显示更多选项」）"
    : "在 Finder 桌面空白处右键 →「快速操作 / 服务」→「剪贴板历史」/「文件中转站」，直接弹出对应窗口";

async function loadIntegration() {
  try {
    integration.value = await invoke<IntegrationStatus>("get_desktop_integration");
  } catch {
    /* ignore */
  }
}

async function toggleIntegration() {
  if (integrationBusy.value) return;
  integrationBusy.value = true;
  integrationError.value = "";
  try {
    integration.value = await invoke<IntegrationStatus>("set_desktop_integration", {
      enabled: !integration.value.enabled,
    });
  } catch (e) {
    integrationError.value = String(e);
  } finally {
    integrationBusy.value = false;
  }
}

/* ---------------- 锁屏能力自检（macOS） ---------------- */
interface LockReport {
  platform: string;
  /** 结论：当前会使用的锁屏方式 */
  summary: string;
  /** 修复建议（空串 = 无需处理） */
  advice: string;
  /** 是否存在「锁不住 / 只能降级」风险 */
  degraded: boolean;
  /** 逐项探测明细 */
  details: string[];
}

const lockReport = ref<LockReport | null>(null);

async function loadLockReport() {
  if (osName !== "macos") return;
  try {
    lockReport.value = await invoke<LockReport>("get_lock_capability");
  } catch {
    /* ignore */
  }
}

onMounted(() => {
  loadConfig();
  loadIntegration();
  loadAutoStart();
  loadLockReport();
});
</script>

<template>
  <div class="page">
    <header class="page-head">
      <h1>系统设置</h1>
      <p>剪贴板同步、系统集成与启动方式</p>
    </header>

    <!-- 剪贴板同步 -->
    <section class="card">
      <div class="card-head">
        <div>
          <h2>剪贴板同步（MQTT）</h2>
          <p class="card-sub">保存后自动连接并订阅同步 Topic</p>
        </div>
      </div>

      <div class="info-grid">
        <div class="info">
          <span class="info-label">同步 Topic</span>
          <code>{{ topicPreview }}</code>
        </div>
        <div class="info">
          <span class="info-label">本机标识</span>
          <code class="ellipsis">{{ form.deviceId || "—" }}</code>
        </div>
      </div>

      <form class="form-grid" @submit.prevent="saveConfig">
        <label class="field span-2">
          <span>服务器地址</span>
          <input
            v-model.trim="form.host"
            type="text"
            placeholder="例如 broker.emqx.io"
            spellcheck="false"
          />
        </label>
        <label class="field">
          <span>端口</span>
          <input v-model.number="form.port" type="number" min="1" max="65535" placeholder="1883" />
        </label>
        <label class="field">
          <span>Group 号</span>
          <input
            v-model.trim="form.group"
            type="text"
            placeholder="如 my-family-001"
            spellcheck="false"
          />
        </label>
        <label class="field">
          <span>用户名（可选）</span>
          <input
            v-model.trim="form.username"
            type="text"
            placeholder="留空则匿名连接"
            autocomplete="off"
            spellcheck="false"
          />
        </label>
        <label class="field">
          <span>密码（可选）</span>
          <input
            v-model="form.password"
            type="password"
            placeholder="••••••••"
            autocomplete="new-password"
          />
        </label>

        <div class="form-foot span-2">
          <label class="switch">
            <input v-model="form.tls" type="checkbox" />
            <span class="track"><span class="thumb"></span></span>
            <span class="switch-text">TLS 加密连接</span>
          </label>
          <button class="primary" type="submit" :disabled="saving">
            {{ saving ? "保存中…" : "保存并连接" }}
          </button>
        </div>
      </form>

      <p v-if="configError" class="hint err">{{ configError }}</p>
      <p v-else-if="configSaved" class="hint ok">
        配置已保存，已自动订阅 {{ topicPreview }}
      </p>
      <p v-else-if="mqttStatus.message && mqttStatus.state !== 'connected'" class="hint warn">
        {{ mqttStatus.message }}
      </p>
      <p v-else class="hint">
        本机每次复制的文本会发布到 {{ topicPreview }}，收到其他设备的内容自动写入剪贴板，
        两者都会记入「剪贴板历史」。
      </p>
    </section>

    <!-- 文件中转站（MinIO） -->
    <section class="card">
      <div class="card-head">
        <div>
          <h2>文件中转站（MinIO）</h2>
          <p class="card-sub">S3 兼容存储；文件存放在指定 bucket 中（默认 hub，不存在时自动创建）</p>
        </div>
      </div>

      <form class="form-grid" @submit.prevent="saveMinioConfig">
        <label class="field span-2">
          <span>服务器地址</span>
          <input
            v-model.trim="form.minio.host"
            type="text"
            placeholder="例如 192.168.1.5 或直接粘贴 http://192.168.1.5:9000"
            spellcheck="false"
          />
        </label>
        <label class="field">
          <span>端口</span>
          <input v-model.number="form.minio.port" type="number" min="1" max="65535" placeholder="9000" />
        </label>
        <label class="field">
          <span>存储桶</span>
          <input
            v-model.trim="form.minio.bucket"
            type="text"
            placeholder="hub（默认）"
            spellcheck="false"
          />
        </label>
        <label class="field">
          <span>Access Key</span>
          <input
            v-model.trim="form.minio.accessKey"
            type="text"
            placeholder="MinIO 访问密钥"
            autocomplete="off"
            spellcheck="false"
          />
        </label>
        <label class="field">
          <span>Secret Key</span>
          <input
            v-model="form.minio.secretKey"
            type="password"
            placeholder="••••••••"
            autocomplete="new-password"
          />
        </label>

        <div class="form-foot span-2">
          <label class="switch">
            <input v-model="form.minio.tls" type="checkbox" />
            <span class="track"><span class="thumb"></span></span>
            <span class="switch-text">TLS（https）</span>
          </label>
          <div class="actions">
            <button type="button" :disabled="minioTesting || minioBusy" @click="testMinioConnection">
              {{ minioTesting ? "测试中…" : "测试连接" }}
            </button>
            <button class="primary" type="submit" :disabled="minioBusy">
              {{ minioBusy ? "保存中…" : "保存" }}
            </button>
          </div>
        </div>
      </form>

      <p v-if="minioError" class="hint err">{{ minioError }}</p>
      <p v-else-if="minioSaved" class="hint ok">{{ minioSaved }}</p>
      <p v-else class="hint">
        存储桶留空按 hub 处理（不存在时自动创建，名称需为 3~63 位小写字母、数字、中划线或点）。
        Access Key / Secret Key 以机器绑定加密后存本机（配置文件中不落明文），
        更换电脑或重装系统后需要重新填写；地址留空保存 = 清除配置。
        「测试连接」用当前表单值验证，不影响已保存的配置。
      </p>
    </section>

    <!-- 系统集成 -->
    <section class="card">
      <div class="card-head">
        <div>
          <h2>系统集成</h2>
          <p class="card-sub">{{ integrationDesc }}</p>
        </div>
      </div>
      <label class="switch" :class="{ disabled: !integration.supported || integrationBusy }">
        <input
          type="checkbox"
          :checked="integration.enabled"
          :disabled="!integration.supported || integrationBusy"
          @change="toggleIntegration"
        />
        <span class="track"><span class="thumb"></span></span>
        <span class="switch-text">{{ integrationTitle }}</span>
      </label>
      <p v-if="integrationError" class="hint err">{{ integrationError }}</p>
      <p v-else class="hint">{{ integration.detail }}</p>
    </section>

    <!-- 离人自动锁屏 -->
    <section class="card">
      <div class="card-head">
        <div>
          <h2>离人自动锁屏</h2>
          <p class="card-sub">订阅 {{ csiTopic }} 人体感知事件，检测到人离开时自动锁屏</p>
        </div>
      </div>
      <label class="switch" :class="{ disabled: autoLockBusy }">
        <input
          type="checkbox"
          :checked="form.autoLock"
          :disabled="autoLockBusy"
          @change="toggleAutoLock"
        />
        <span class="track"><span class="thumb"></span></span>
        <span class="switch-text">开启自动锁屏</span>
      </label>
      <div v-if="form.autoLock" class="form-grid" style="margin-top: 12px">
        <label class="field">
          <span>锁屏前等待（秒）</span>
          <input
            v-model.number="form.autoLockDelaySecs"
            type="number"
            min="1"
            max="300"
            placeholder="15"
            @change="saveAutoLockFlag()"
          />
        </label>
      </div>
      <p v-if="autoLockError" class="hint err">{{ autoLockError }}</p>
      <p v-else-if="autoLockSaved" class="hint ok">自动锁屏设置已保存</p>
      <p v-else class="hint">
        开启后订阅 {{ csiTopic }}（需先配置 MQTT 连接）：收到「人离开」后开始倒计时，<b>期间收到「靠近」自动取消</b>，
        倒计时结束才真正锁屏。考虑消息投递延迟，建议等待 15~30 秒；两次锁屏间隔至少 60 秒。
      </p>

      <div v-if="lockReport" class="lock-check">
        <p class="hint" :class="lockReport.degraded ? 'warn' : 'ok'">
          锁屏能力自检：{{ lockReport.summary }}
        </p>
        <p v-if="lockReport.advice" class="hint warn">建议：{{ lockReport.advice }}</p>
        <ul class="lock-details">
          <li v-for="detail in lockReport.details" :key="detail">{{ detail }}</li>
        </ul>
      </div>
    </section>

    <!-- 锁屏同步 -->
    <section class="card">
      <div class="card-head">
        <div>
          <h2>锁屏同步</h2>
          <p class="card-sub">topic {{ lockSyncTopic }}：收 / 发两个开关独立控制</p>
        </div>
      </div>
      <label class="switch" :class="{ disabled: lockSyncBusy }">
        <input
          type="checkbox"
          :checked="form.lockSyncPublish"
          :disabled="lockSyncBusy"
          @change="togglePublish"
        />
        <span class="track"><span class="thumb"></span></span>
        <span class="switch-text">广播我的锁屏</span>
      </label>
      <p class="hint">本机锁定 / 解锁时通知同组设备（不勾选则本机状态对外静默）。</p>
      <label class="switch" :class="{ disabled: lockSyncBusy }">
        <input
          type="checkbox"
          :checked="form.lockSyncFollow"
          :disabled="lockSyncBusy"
          @change="toggleFollow"
        />
        <span class="track"><span class="thumb"></span></span>
        <span class="switch-text">跟随其他设备锁屏</span>
      </label>
      <label class="field">
        <span>活跃守卫阈值（秒）</span>
        <input
          v-model.number="form.lockSyncIdleGuardSecs"
          type="number"
          min="0"
          max="3600"
          placeholder="300"
          :disabled="lockSyncBusy || !form.lockSyncFollow"
          @change="saveLockSyncFlag()"
        />
      </label>
      <p v-if="lockSyncError" class="hint err">{{ lockSyncError }}</p>
      <p v-else-if="lockSyncSaved" class="hint ok">锁屏同步设置已保存</p>
      <p v-else class="hint">
        收到远端锁屏事件时：<b>本机最近 N 秒内有人键鼠操作则跳过锁定</b>——
        正在使用的设备（如工作机）不会被其他设备的锁屏联动误锁；人离开超过阈值才执行。
        填 0 关闭守卫总是跟随。远端解锁事件仅唤醒屏幕（停在锁屏界面，不会绕过密码）。
        需先配置并保存上方 MQTT 连接。
      </p>
    </section>

    <!-- 按键同步 -->
    <section class="card">
      <div class="card-head">
        <div>
          <h2>按键同步</h2>
          <p class="card-sub">topic {{ keySyncTopic }}：一台设备按下组合，同组设备按各自规则注入目标按键</p>
        </div>
      </div>

      <label class="switch" :class="{ disabled: keySyncBusy }">
        <input
          type="checkbox"
          :checked="form.keySyncEnabled"
          :disabled="keySyncBusy"
          @change="toggleKeySync"
        />
        <span class="track"><span class="thumb"></span></span>
        <span class="switch-text">开启按键同步</span>
      </label>

      <div v-if="form.keySyncEnabled" class="keysync-rules">
        <div v-for="row in rows" :key="row.id" class="keysync-row" :class="{ off: !row.enabled }">
          <label class="mini-switch" :title="row.enabled ? '停用该规则（组合保留）' : '启用该规则'">
            <input
              type="checkbox"
              :checked="row.enabled"
              :disabled="keySyncBusy"
              @change="toggleRowEnabled(row)"
            />
            <span class="track"><span class="thumb"></span></span>
          </label>

          <div class="combo-cell">
            <input
              v-model="row.sourceText"
              class="combo-input"
              :class="{ invalid: row.sourceError }"
              type="text"
              placeholder="按下组合录制，如 Ctrl+F9"
              spellcheck="false"
              autocomplete="off"
              @keydown="comboKeydown($event, row, 'source')"
              @keyup="comboKeyup($event, row, 'source')"
              @blur="comboBlur(row, 'source')"
            />
            <div class="keysync-quick">
              <button
                v-for="[value, label] in QUICK_KEYS"
                :key="value"
                type="button"
                class="quick-key"
                :disabled="keySyncBusy"
                @mousedown.prevent
                @click="pickQuickKey($event, row, 'source', value)"
              >
                {{ label }}
              </button>
            </div>
            <p v-if="row.sourceError" class="row-err">{{ row.sourceError }}</p>
          </div>

          <span class="keysync-arrow" aria-hidden="true">→</span>

          <div class="combo-cell">
            <input
              v-model="row.targetText"
              class="combo-input"
              :class="{ invalid: row.targetError }"
              type="text"
              placeholder="注入组合录制，如 Cmd+Down"
              spellcheck="false"
              autocomplete="off"
              @keydown="comboKeydown($event, row, 'target')"
              @keyup="comboKeyup($event, row, 'target')"
              @blur="comboBlur(row, 'target')"
            />
            <div class="keysync-quick">
              <button
                v-for="[value, label] in QUICK_KEYS"
                :key="value"
                type="button"
                class="quick-key"
                :disabled="keySyncBusy"
                @mousedown.prevent
                @click="pickQuickKey($event, row, 'target', value)"
              >
                {{ label }}
              </button>
            </div>
            <p v-if="row.targetError" class="row-err">{{ row.targetError }}</p>
          </div>

          <select v-model="row.platform" :disabled="keySyncBusy" @change="scheduleKeySyncSave()">
            <option value="all">全部平台</option>
            <option value="windows">仅 Windows</option>
            <option value="macos">仅 macOS</option>
          </select>

          <input
            v-model="row.note"
            type="text"
            class="keysync-note"
            placeholder="备注"
            spellcheck="false"
            autocomplete="off"
            maxlength="30"
            @input="scheduleKeySyncSave()"
          />

          <button class="ghost remove" type="button" :disabled="keySyncBusy" @click="removeRow(row)">
            删除
          </button>
        </div>

        <div class="keysync-foot">
          <button type="button" :disabled="keySyncBusy" @click="addRow">＋ 添加规则</button>
          <span v-if="rowsDirty" class="dirty-hint">有未保存的改动（5 秒后自动保存）</span>
          <span v-else-if="keySyncBusy" class="dirty-hint">保存中…</span>
        </div>
      </div>

      <p v-if="keySyncError" class="hint err">{{ keySyncError }}</p>
      <p v-else-if="keySyncSaved" class="hint ok">按键同步设置已保存</p>
      <p v-else class="hint">
        只在发送端配置就能单向生效：本机按下「源组合」即广播
        源组合 + 目标组合 + 目标平台，接收端<b>没有同源规则时按发送端映射注入</b>；
        接收端若也配置了同源规则（启用中），则以<b>本机规则</b>为准——每台设备可映射到
        不同按键。目标平台选「全部」时，同组里未单独配置规则的所有设备都会执行注入。
        目标组合也支持<strong>单键</strong>（如仅 F9 / 仅 ↓，点下方主键按钮直接填入）。
        组合框支持按下录制与手动键入键名（回车或失焦提交）；<b>方向键 ↑↓←→、Home、End
        用输入框下方的按钮点选</b>。macOS 说明：Ctrl+↑↓ / Ctrl+←→ / Ctrl+Space 等被系统
        快捷键（调度中心 / 切换空间 / 输入法）优先处理，直接按键录不到——请用按钮或键名
        输入，或到「系统设置 → 键盘 → 键盘快捷键」临时停用对应项后录制；安装引擎需允许
        键盘事件监听（系统设置 → 隐私与安全性 → 输入监视，勾选 desktools 后重新打开总开关）。
      </p>
    </section>

    <!-- 启动 -->
    <section class="card">
      <div class="card-head">
        <div>
          <h2>启动</h2>
          <p class="card-sub">随系统登录自动运行</p>
        </div>
      </div>
      <label class="switch" :class="{ disabled: !autoStartManaged || autoStartBusy }">
        <input
          type="checkbox"
          :checked="autoStart"
          :disabled="!autoStartManaged || autoStartBusy"
          @change="toggleAutoStart"
        />
        <span class="track"><span class="thumb"></span></span>
        <span class="switch-text">开机自动启动</span>
      </label>
      <p v-if="autoStartError" class="hint err">{{ autoStartError }}</p>
      <p v-else-if="!autoStartManaged" class="hint warn">
        开发构建不管理开机自启（避免自启条目指向 target/debug 二进制），请使用打包版本配置。
      </p>
      <p v-else class="hint">
        开机自启时以「仅托盘」模式运行，剪贴板同步在后台继续工作；关闭窗口将最小化到托盘，
        可在托盘菜单中选择「剪贴板历史」「显示主窗口」或「退出」。
      </p>
    </section>
  </div>
</template>

<style scoped>
.info-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
  margin-bottom: 16px;
}

.info {
  display: flex;
  flex-direction: column;
  gap: 3px;
  padding: 9px 12px;
  border-radius: var(--radius-sm);
  background: var(--accent-soft);
  min-width: 0;
}

.info-label {
  font-size: 0.72rem;
  color: var(--text-secondary);
}

.info code {
  font-size: 0.82rem;
  color: var(--accent);
  user-select: text;
}

.ellipsis {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.switch.disabled {
  opacity: 0.55;
  cursor: not-allowed;
}

.lock-check {
  margin-top: 10px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.lock-details {
  margin: 0;
  padding-left: 18px;
  font-size: 0.78rem;
  color: var(--text-secondary);
  display: flex;
  flex-direction: column;
  gap: 2px;
}

/* ---------- 按键同步规则行 ---------- */
.keysync-rules {
  margin-top: 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.keysync-row {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto minmax(0, 1fr) 108px minmax(96px, 140px) auto;
  gap: 8px;
  align-items: center;
}

.keysync-row.off {
  opacity: 0.6;
}

.combo-cell {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
}

.keysync-row input,
.keysync-row select {
  width: 100%;
  padding: 8px 11px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-input);
  color: var(--text);
  outline: none;
  transition:
    border-color 0.15s,
    box-shadow 0.15s;
  user-select: text;
}

.keysync-row input:focus,
.keysync-row select:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-soft);
}

.combo-input.invalid {
  border-color: var(--err);
}

.combo-input.invalid:focus {
  box-shadow: 0 0 0 3px var(--err-soft);
}

.keysync-arrow {
  color: var(--text-secondary);
  padding: 0 1px;
}

.row-err {
  margin: 0;
  font-size: 0.72rem;
  line-height: 1.3;
  color: var(--err);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 主键速选（方向键等系统快捷键易占用 / 单键映射的主键快速填入） */
.keysync-quick {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.quick-key {
  padding: 1px 8px;
  font-size: 0.72rem;
  line-height: 1.5;
  border-radius: 7px;
}

.quick-key:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent);
}

.keysync-foot {
  display: flex;
  align-items: center;
  gap: 10px;
}

.dirty-hint {
  font-size: 0.76rem;
  color: var(--text-muted);
}

/* 小号开关（规则行内的启用 / 禁用） */
.mini-switch {
  position: relative;
  display: inline-flex;
  align-items: center;
  cursor: pointer;
  user-select: none;
}

.mini-switch input {
  position: absolute;
  opacity: 0;
  width: 0;
  height: 0;
}

.mini-switch .track {
  position: relative;
  width: 30px;
  height: 18px;
  border-radius: 999px;
  background: rgba(128, 138, 156, 0.35);
  transition: background 0.18s;
  flex-shrink: 0;
}

.mini-switch .thumb {
  position: absolute;
  top: 2.5px;
  left: 2.5px;
  width: 13px;
  height: 13px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.25);
  transition: transform 0.18s;
}

.mini-switch input:checked + .track {
  background: var(--accent);
}

.mini-switch input:checked + .track .thumb {
  transform: translateX(12px);
}
</style>
