<script setup lang="ts">
import { onMounted, ref } from "vue";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

const RELEASES_URL = "https://github.com/240011814/desktools/releases/latest";

const version = ref("...");
const autoStart = ref(false);
const checking = ref(false);
const installing = ref(false);
const progress = ref(0);
const status = ref<"idle" | "latest" | "update" | "error">("idle");
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
  checkUpdate();
});

async function checkUpdate() {
  checking.value = true;
  status.value = "idle";
  errorMsg.value = "";
  try {
    const found = await check();
    update.value = found ?? null;
    status.value = found ? "update" : "latest";
  } catch (e) {
    status.value = "error";
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
    status.value = "error";
    errorMsg.value = String(e);
    installing.value = false;
  }
}

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
  <main class="container">
    <h1>desktools</h1>

    <section class="card">
      <h2>版本</h2>
      <p class="version">{{ version }}</p>
      <div class="row">
        <button :disabled="checking || installing" @click="checkUpdate">
          {{ checking ? "检测中…" : "检测新版本" }}
        </button>
        <button
          v-if="status === 'update'"
          class="primary"
          :disabled="installing"
          @click="installUpdate"
        >
          {{ installing ? `下载中 ${progress}%` : "下载并安装" }}
        </button>
      </div>
      <div v-if="installing" class="progress">
        <div class="bar" :style="{ width: progress + '%' }"></div>
      </div>
      <p v-if="status === 'latest'" class="hint ok">
        当前已是最新版本
      </p>
      <p v-else-if="status === 'update' && !installing" class="hint warn">
        发现新版本 {{ update?.version ?? "" }}，安装完成后将自动重启应用
        <button class="link" @click="openReleases">手动下载</button>
      </p>
      <p v-else-if="status === 'error'" class="hint err">
        版本检测失败{{ errorMsg ? `：${errorMsg}` : "" }}
      </p>
    </section>

    <section class="card">
      <h2>系统</h2>
      <label class="switch">
        <input type="checkbox" :checked="autoStart" @change="toggleAutoStart" />
        <span>开机自动启动</span>
      </label>
      <p class="hint">
        关闭窗口将最小化到系统托盘，可在托盘图标菜单中选择「显示主窗口」或「退出」。
      </p>
    </section>
  </main>
</template>

<style scoped>
.container {
  padding-top: 6vh;
}

.card {
  max-width: 420px;
  margin: 0 auto 1.2em;
  padding: 1.2em 1.5em;
  border-radius: 12px;
  background: rgba(0, 0, 0, 0.03);
  text-align: left;
}

h1 {
  margin-bottom: 1em;
}

h2 {
  font-size: 1em;
  margin: 0 0 0.6em;
}

.version {
  margin: 0 0 0.8em;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 1.1em;
}

.row {
  display: flex;
  gap: 0.8em;
}

.switch {
  display: flex;
  align-items: center;
  gap: 0.6em;
  cursor: pointer;
  user-select: none;
}

.hint {
  margin: 0.8em 0 0;
  font-size: 0.85em;
  opacity: 0.75;
  line-height: 1.5;
}

.hint.ok {
  color: #2e9e5b;
  opacity: 1;
}

.hint.warn {
  color: #c98a12;
  opacity: 1;
}

.hint.err {
  color: #c0392b;
  opacity: 1;
}

button.link {
  background: none;
  border: none;
  padding: 0;
  color: #4b8bf5;
  text-decoration: underline;
  cursor: pointer;
  font-size: 1em;
}

button.primary {
  background: #4b8bf5;
  border: none;
  color: #fff;
}

button.primary:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.progress {
  margin-top: 0.8em;
  height: 6px;
  border-radius: 3px;
  background: rgba(0, 0, 0, 0.1);
  overflow: hidden;
}

.progress .bar {
  height: 100%;
  background: #4b8bf5;
  transition: width 0.2s;
}
</style>
