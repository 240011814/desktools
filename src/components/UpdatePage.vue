<script setup lang="ts">
import { onMounted, ref, shallowRef } from "vue";
import { openUrl } from "@tauri-apps/plugin-opener";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { version } from "../store";

const RELEASES_URL = "https://github.com/240011814/desktools/releases/latest";

const checking = ref(false);
const installing = ref(false);
const progress = ref(0);
const updateState = ref<"idle" | "latest" | "update" | "error">("idle");
// Update 是带 # 私有字段的类实例（继承 Resource），
// 必须用 shallowRef 保存：深度响应式代理会破坏私有字段访问，
// 调用 downloadAndInstall() 时报「Cannot read private member...」
const update = shallowRef<Update | null>(null);
const errorMsg = ref("");
const installError = ref("");

onMounted(() => checkUpdate());

async function checkUpdate() {
  checking.value = true;
  updateState.value = "idle";
  errorMsg.value = "";
  installError.value = "";
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
  installError.value = "";
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
    installError.value = String(e);
    installing.value = false;
  }
}

function openReleases() {
  openUrl(RELEASES_URL);
}
</script>

<template>
  <div class="page">
    <header class="page-head">
      <h1>软件更新</h1>
      <p>检查并安装新版本，安装完成后自动重启应用</p>
    </header>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>当前版本</h2>
          <p class="card-sub">版本号由 git 提交自动生成</p>
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

      <p class="version">v{{ version }}</p>

      <div v-if="installing" class="progress">
        <div class="bar" :style="{ width: progress + '%' }"></div>
      </div>

      <p v-if="installError" class="hint err">
        升级失败：{{ installError }}
        <button class="link" @click="installUpdate">重试</button>
      </p>
      <p v-else-if="updateState === 'latest'" class="hint ok">当前已是最新版本</p>
      <p v-else-if="updateState === 'update' && !installing" class="hint warn">
        发现新版本 {{ update?.version ?? "" }}，安装完成后将自动重启应用
        <button class="link" @click="openReleases">手动下载</button>
      </p>
      <p v-else-if="updateState === 'error'" class="hint err">
        版本检测失败{{ errorMsg ? `：${errorMsg}` : "" }}
      </p>
      <p v-else class="hint">正在检查更新…</p>
    </section>
  </div>
</template>

<style scoped>
.version {
  margin: 0;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 1.25rem;
  font-weight: 600;
}
</style>
