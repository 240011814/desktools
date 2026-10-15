<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

const RELEASES_URL = "https://github.com/240011814/desktools/releases/latest";

/* ---------------- MQTT 剪贴板同步 ---------------- */

interface MqttConfig {
  host: string;
  port: number;
  username: string;
  password: string;
  tls: boolean;
  group: string;
  deviceId: string;
}

interface MqttStatus {
  state: "idle" | "connecting" | "connected" | "reconnecting" | "error";
  message: string;
  topic: string;
}

const form = reactive<MqttConfig>({
  host: "",
  port: 1883,
  username: "",
  password: "",
  tls: false,
  group: "",
  deviceId: "",
});

const status = ref<MqttStatus>({ state: "idle", message: "", topic: "" });
const saving = ref(false);
const configError = ref("");
const configSaved = ref(false);

const STATUS_TEXT: Record<MqttStatus["state"], string> = {
  idle: "未启用",
  connecting: "连接中",
  connected: "已连接",
  reconnecting: "重连中",
  error: "连接失败",
};

const statusLabel = computed(() => STATUS_TEXT[status.value.state] ?? "未知");
const topicPreview = computed(
  () => `texthub/${form.group.trim() || "<group号>"}`,
);

async function loadConfig() {
  try {
    const cfg = await invoke<MqttConfig>("get_mqtt_config");
    Object.assign(form, cfg);
  } catch {
    /* 浏览器预览等无 Tauri 环境时使用默认值 */
  }
  try {
    status.value = await invoke<MqttStatus>("get_mqtt_status");
  } catch {
    /* ignore */
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

/* ---------------- 版本更新 ---------------- */

const version = ref("...");
const autoStart = ref(false);
const checking = ref(false);
const installing = ref(false);
const progress = ref(0);
const updateState = ref<"idle" | "latest" | "update" | "error">("idle");
const update = ref<Update | null>(null);
const errorMsg = ref("");

onMounted(async () => {
  try {
    version.value = await getVersion();
  } catch {
    version.value = "未知";
  }
  try {
    autoStart.value = await isEnabled();
  } catch {
    autoStart.value = false;
  }
  loadConfig();
  listen<MqttStatus>("mqtt://status", (e) => {
    status.value = e.payload;
  }).catch(() => {});
  checkUpdate();
});

async function checkUpdate() {
  checking.value = true;
  updateState.value = "idle";
  errorMsg.value = "";
  try {
    const found = await check();
    update.value = found ?? null;
    updateState.value = found ? "update" : "latest";
  } catch (e) {
    updateState.value = "error";
    errorMsg.value = String(e);
  } finally {
    checking.value = false;
  }
}

async function installUpdate() {
  if (!update.value || installing.value) return;
  installing.value = true;
  progress.value = 0;
  try {
    let downloaded = 0;
    let contentLength = 0;
    await update.value.downloadAndInstall((event) => {
      switch (event.event) {
        case "Started":
          contentLength = event.data.contentLength ?? 0;
          break;
        case "Progress":
          downloaded += event.data.chunkLength;
          if (contentLength > 0) {
            progress.value = Math.round((downloaded / contentLength) * 100);
          }
          break;
        case "Finished":
          progress.value = 100;
          break;
      }
    });
    await relaunch();
  } catch (e) {
    updateState.value = "error";
    errorMsg.value = String(e);
    installing.value = false;
  }
}

/* ---------------- 系统 ---------------- */

async function toggleAutoStart() {
  try {
    if (autoStart.value) {
      await disable();
    } else {
      await enable();
    }
  } finally {
    autoStart.value = await isEnabled();
  }
}

function openReleases() {
  openUrl(RELEASES_URL);
}
</script>

<template>
  <div class="page">
    <header class="topbar">
      <div class="brand">
        <div class="logo">
          <svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true">
            <path
              fill="currentColor"
              d="M9.5 3A3.5 3.5 0 0 0 6 6.5V8H5a3 3 0 0 0-3 3v6a3 3 0 0 0 3 3h1v-1.5A3.5 3.5 0 0 0 9.5 15H15a3 3 0 0 0 3-3V6.5A3.5 3.5 0 0 0 14.5 3h-5Zm0 2h5a1.5 1.5 0 0 1 1.5 1.5V12a1 1 0 0 1-1 1H9.5A1.5 1.5 0 0 1 8 11.5V6.5A1.5 1.5 0 0 1 9.5 5ZM5 10.5h3v2.5a2.5 2.5 0 0 1-2.5 2.5H5V18h1a1 1 0 0 0 1-1v-2.5A2.5 2.5 0 0 0 4.5 12H4v-1.5h1Z"
            />
          </svg>
        </div>
        <div class="brand-text">
          <h1>DeskTools</h1>
          <p>跨设备剪贴板同步</p>
        </div>
      </div>
      <div class="status-pill" :class="status.state">
        <span class="dot"></span>
        {{ statusLabel }}
      </div>
    </header>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>剪贴板同步</h2>
          <p class="card-sub">复制文本自动同步到同一 Group 的其他设备</p>
        </div>
      </div>

      <div class="info-grid">
        <div class="info">
          <span class="info-label">同步 Topic</span>
          <code>{{ topicPreview }}</code>
        </div>
        <div class="info">
          <span class="info-label">本机标识</span>
          <code class="mono-ellipsis">{{ form.deviceId || "—" }}</code>
        </div>
      </div>

      <form class="form" @submit.prevent="saveConfig">
        <div class="form-grid">
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
            <input
              v-model.number="form.port"
              type="number"
              min="1"
              max="65535"
              placeholder="1883"
            />
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
        </div>

        <div class="form-foot">
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
      <p v-else-if="status.message && status.state !== 'connected'" class="hint warn">
        {{ status.message }}
      </p>
      <p v-else class="hint">
        保存后程序会自动连接并订阅 {{ topicPreview }}；本机复制的文本会发布到该 Topic，
        收到其他设备的内容将自动写入剪贴板。
      </p>
    </section>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>软件更新</h2>
          <p class="card-sub">
            当前版本 <strong class="mono">v{{ version }}</strong>
          </p>
        </div>
        <div class="actions">
          <button :disabled="checking || installing" @click="checkUpdate">
            {{ checking ? "检测中…" : "检测新版本" }}
          </button>
          <button
            v-if="updateState === 'update'"
            class="primary"
            :disabled="installing"
            @click="installUpdate"
          >
            {{ installing ? `下载中 ${progress}%` : "下载并安装" }}
          </button>
        </div>
      </div>

      <div v-if="installing" class="progress">
        <div class="bar" :style="{ width: progress + '%' }"></div>
      </div>

      <p v-if="updateState === 'latest'" class="hint ok">当前已是最新版本</p>
      <p v-else-if="updateState === 'update' && !installing" class="hint warn">
        发现新版本 {{ update?.version ?? "" }}，安装完成后将自动重启应用
        <button class="link" @click="openReleases">手动下载</button>
      </p>
      <p v-else-if="updateState === 'error'" class="hint err">
        版本检测失败{{ errorMsg ? `：${errorMsg}` : "" }}
      </p>
    </section>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>系统</h2>
          <p class="card-sub">启动与后台运行方式</p>
        </div>
      </div>
      <label class="switch">
        <input type="checkbox" :checked="autoStart" @change="toggleAutoStart" />
        <span class="track"><span class="thumb"></span></span>
        <span class="switch-text">开机自动启动</span>
      </label>
      <p class="hint">
        关闭窗口将最小化到系统托盘，可在托盘图标菜单中选择「显示主窗口」或「退出」。
      </p>
    </section>

    <footer class="footer">DeskTools · v{{ version }}</footer>
  </div>
</template>

<style scoped>
.page {
  max-width: 720px;
  margin: 0 auto;
  padding: 26px 22px 32px;
}

/* ---------- 顶栏 ---------- */
.topbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 18px;
}

.brand {
  display: flex;
  align-items: center;
  gap: 12px;
}

.logo {
  display: grid;
  place-items: center;
  width: 42px;
  height: 42px;
  border-radius: 12px;
  color: #fff;
  background: linear-gradient(135deg, var(--accent), #7b5cf0);
  box-shadow: var(--shadow);
}

.brand-text h1 {
  margin: 0;
  font-size: 1.25rem;
  letter-spacing: 0.2px;
  line-height: 1.3;
}

.brand-text p {
  margin: 0;
  font-size: 0.8rem;
  color: var(--text-muted);
}

.status-pill {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 5px 12px;
  border-radius: 999px;
  font-size: 0.8rem;
  font-weight: 500;
  color: var(--text-secondary);
  background: var(--accent-soft);
  white-space: nowrap;
}

.status-pill .dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: currentColor;
}

.status-pill.connected {
  color: var(--ok);
  background: var(--ok-soft);
}

.status-pill.connecting,
.status-pill.reconnecting {
  color: var(--warn);
  background: var(--warn-soft);
}

.status-pill.error {
  color: var(--err);
  background: var(--err-soft);
}

.status-pill.connecting .dot,
.status-pill.reconnecting .dot {
  animation: pulse 1.2s ease-in-out infinite;
}

@keyframes pulse {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.35;
  }
}

/* ---------- 卡片 ---------- */
.card {
  margin-bottom: 14px;
  padding: 18px 20px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--bg-elevated);
  box-shadow: var(--shadow);
  text-align: left;
}

.card-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 14px;
}

.card-head h2 {
  margin: 0;
  font-size: 1rem;
  font-weight: 600;
}

.card-sub {
  margin: 2px 0 0;
  font-size: 0.8rem;
  color: var(--text-muted);
}

.actions {
  display: flex;
  gap: 8px;
  flex-shrink: 0;
}

/* ---------- 信息展示 ---------- */
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
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.82rem;
  color: var(--accent);
  user-select: text;
}

.mono-ellipsis {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

/* ---------- 表单 ---------- */
.form-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 5px;
  min-width: 0;
}

.field.span-2 {
  grid-column: span 2;
}

.field > span {
  font-size: 0.78rem;
  font-weight: 500;
  color: var(--text-secondary);
}

.field input {
  width: 100%;
  padding: 8px 11px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-input);
  color: var(--text);
  outline: none;
  transition: border-color 0.15s, box-shadow 0.15s;
  user-select: text;
}

.field input::placeholder {
  color: var(--text-muted);
}

.field input:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-soft);
}

.form-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-top: 16px;
  flex-wrap: wrap;
}

/* ---------- 开关 ---------- */
.switch {
  display: inline-flex;
  align-items: center;
  gap: 9px;
  cursor: pointer;
  user-select: none;
}

.switch input {
  position: absolute;
  opacity: 0;
  width: 0;
  height: 0;
}

.switch .track {
  position: relative;
  width: 38px;
  height: 22px;
  border-radius: 999px;
  background: rgba(128, 138, 156, 0.35);
  transition: background 0.18s;
  flex-shrink: 0;
}

.switch .thumb {
  position: absolute;
  top: 3px;
  left: 3px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.25);
  transition: transform 0.18s;
}

.switch input:checked + .track {
  background: var(--accent);
}

.switch input:checked + .track .thumb {
  transform: translateX(16px);
}

.switch input:focus-visible + .track {
  box-shadow: 0 0 0 3px var(--accent-soft);
}

.switch-text {
  font-size: 0.86rem;
}

/* ---------- 按钮 ---------- */
button {
  padding: 8px 15px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-input);
  color: var(--text);
  font-size: 0.85rem;
  font-weight: 500;
  cursor: pointer;
  transition: background 0.15s, border-color 0.15s, transform 0.05s;
}

button:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent);
}

button:active:not(:disabled) {
  transform: translateY(1px);
}

button:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}

button.primary {
  border: none;
  background: var(--accent);
  color: #fff;
}

button.primary:hover:not(:disabled) {
  background: var(--accent-hover);
  color: #fff;
}

button.link {
  padding: 0;
  border: none;
  background: none;
  color: var(--accent);
  text-decoration: underline;
}

button.link:hover {
  color: var(--accent-hover);
}

/* ---------- 提示与进度 ---------- */
.hint {
  margin: 12px 0 0;
  font-size: 0.79rem;
  color: var(--text-muted);
  line-height: 1.65;
}

.hint.ok {
  color: var(--ok);
}

.hint.warn {
  color: var(--warn);
}

.hint.err {
  color: var(--err);
}

.progress {
  margin-top: 4px;
  height: 6px;
  border-radius: 3px;
  background: var(--border);
  overflow: hidden;
}

.progress .bar {
  height: 100%;
  background: var(--accent);
  transition: width 0.2s;
}

.footer {
  margin-top: 18px;
  text-align: center;
  font-size: 0.75rem;
  color: var(--text-muted);
}
</style>
