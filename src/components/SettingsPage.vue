<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { platform } from "@tauri-apps/plugin-os";
import { mqttStatus, type MqttConfig, type MinioConfig } from "../store";

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
</style>
