<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { mqttStatus, type MqttConfig } from "../store";

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

onMounted(async () => {
  try {
    const cfg = await invoke<MqttConfig>("get_mqtt_config");
    Object.assign(form, cfg);
  } catch {
    /* 浏览器预览等无 Tauri 环境时使用默认值 */
  }
});

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
</script>

<template>
  <div class="page">
    <header class="page-head">
      <h1>剪贴板同步</h1>
      <p>通过 MQTT 在同一 Group 的设备之间自动同步复制的文本</p>
    </header>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>连接配置</h2>
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
        本机每次复制的文本会发布到 {{ topicPreview }}，收到其他设备的内容将自动写入剪贴板，
        两者都会记入「剪贴板历史」。
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
</style>
