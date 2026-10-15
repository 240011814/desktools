<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { platform } from "@tauri-apps/plugin-os";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { mqttStatus, type MqttConfig } from "../store";

/* ---------------- 开机自启 ---------------- */
const autoStart = ref(false);

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

/* ---------------- 剪贴板同步（MQTT） ---------------- */
const form = reactive<MqttConfig>({
  host: "",
  port: 1883,
  username: "",
  password: "",
  tls: false,
  group: "",
  deviceId: "",
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
    ? "在桌面空白处右键 →「剪贴板历史」，直接弹出历史窗口（Windows 11 需点「显示更多选项」）"
    : "在 Finder 桌面空白处右键 →「快速操作 / 服务」→「剪贴板历史」，直接弹出历史窗口";

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

onMounted(() => {
  loadConfig();
  loadIntegration();
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

    <!-- 启动 -->
    <section class="card">
      <div class="card-head">
        <div>
          <h2>启动</h2>
          <p class="card-sub">随系统登录自动运行</p>
        </div>
      </div>
      <label class="switch">
        <input type="checkbox" :checked="autoStart" @change="toggleAutoStart" />
        <span class="track"><span class="thumb"></span></span>
        <span class="switch-text">开机自动启动</span>
      </label>
      <p class="hint">
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
</style>
