<script setup lang="ts">
import { onMounted, ref } from "vue";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";

const RELEASES_URL = "https://github.com/240011814/desktools/releases/latest";
const RELEASES_API =
  "https://api.github.com/repos/240011814/desktools/releases/latest";

const version = ref("...");
const autoStart = ref(false);
const checking = ref(false);
const status = ref<"idle" | "latest" | "update" | "error">("idle");
const latestVersion = ref("");

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

/** 解析 "1.0.12+b497821" / "v1.0.12-b497821" → [1, 0, 12] */
function parseVersion(v: string): number[] {
  const core = v.replace(/^v/i, "").split(/[-+]/)[0];
  return core.split(".").map((n) => parseInt(n, 10) || 0);
}

function isNewer(candidate: string, current: string): boolean {
  const a = parseVersion(candidate);
  const b = parseVersion(current);
  for (let i = 0; i < 3; i++) {
    if ((a[i] || 0) !== (b[i] || 0)) return (a[i] || 0) > (b[i] || 0);
  }
  return false;
}

async function checkUpdate() {
  checking.value = true;
  status.value = "idle";
  try {
    const res = await fetch(RELEASES_API, {
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const data = await res.json();
    latestVersion.value = data.tag_name ?? "";
    status.value = isNewer(latestVersion.value, version.value)
      ? "update"
      : "latest";
  } catch {
    status.value = "error";
  } finally {
    checking.value = false;
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
        <button :disabled="checking" @click="checkUpdate">
          {{ checking ? "检测中…" : "检测新版本" }}
        </button>
      </div>
      <p v-if="status === 'latest'" class="hint ok">
        当前已是最新版本
      </p>
      <p v-else-if="status === 'update'" class="hint warn">
        发现新版本 {{ latestVersion }}
        <button class="link" @click="openReleases">前往下载</button>
      </p>
      <p v-else-if="status === 'error'" class="hint err">
        版本检测失败，请检查网络后重试
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
</style>
