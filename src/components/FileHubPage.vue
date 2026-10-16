<script setup lang="ts">
import { computed, onActivated, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow, type DragDropEvent } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import type { MqttConfig } from "../store";

/** compact：独立文件中转站小窗口布局（桌面右键 / 托盘弹出） */
const props = defineProps<{ compact?: boolean }>();

/** MinIO 中的文件条目（与后端 file_hub::HubFile 对应） */
interface HubFile {
  key: string;
  size: number;
  lastModified: string;
}

/** 传输进度事件负载（filehub://progress） */
interface ProgressPayload {
  transferId: string;
  key: string;
  direction: "upload" | "download";
  phase: "progress" | "done" | "error";
  transferred: number;
  total: number;
  message?: string;
}

interface Transfer {
  key: string;
  direction: "upload" | "download";
  transferred: number;
  total: number;
}

const configured = ref(true);
const configLoaded = ref(false);
const bucketName = ref("hub");
const files = ref<HubFile[]>([]);
const loading = ref(false);
const uploading = ref(false);
const uploadingImage = ref(false);
const dragging = ref(false);
const error = ref("");
const downloadedPath = ref("");
const clipboardSaved = ref("");
const transfers = ref<Record<string, Transfer>>({});
const deleteArmed = ref("");
let disarmTimer: number | undefined;
let savedTimer: number | undefined;
let initialized = false;
let unlisten: UnlistenFn | undefined;
let unlistenRefresh: UnlistenFn | undefined;
let unlistenDragDrop: UnlistenFn | undefined;

const transferList = computed(() =>
  Object.entries(transfers.value).map(([id, t]) => ({ id, ...t })),
);

async function loadConfig() {
  try {
    const cfg = await invoke<MqttConfig>("get_mqtt_config");
    configured.value = !!cfg.minio.host.trim();
    bucketName.value = cfg.minio.bucket?.trim() || "hub";
  } catch (e) {
    error.value = `读取配置失败：${String(e)}`;
  } finally {
    configLoaded.value = true;
  }
}

async function refresh() {
  if (!configured.value || loading.value) return;
  loading.value = true;
  try {
    files.value = await invoke<HubFile[]>("file_hub_list");
    error.value = "";
  } catch (e) {
    error.value = String(e);
    files.value = [];
  } finally {
    loading.value = false;
  }
}

/** 重读 MinIO 配置并刷新列表（首次挂载 / 从设置页切回时调用） */
async function reload() {
  await loadConfig();
  await refresh();
}

function baseName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

async function upload() {
  if (uploading.value) return;
  let picked: string | string[] | null = null;
  try {
    picked = await open({ multiple: true, title: "选择要上传的文件" });
  } catch (e) {
    error.value = `打开文件选择器失败：${String(e)}`;
    return;
  }
  if (!picked) return;
  await uploadPaths(Array.isArray(picked) ? picked : [picked]);
}

/** 逐个上传本地文件（文件选择器与拖拽共用的入口） */
async function uploadPaths(paths: string[]) {
  if (uploading.value || !paths.length) return;
  uploading.value = true;
  error.value = "";
  const failures: string[] = [];
  for (const path of paths) {
    const transferId = `up-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    try {
      await invoke("file_hub_upload", { path, transferId });
    } catch (e) {
      delete transfers.value[transferId];
      failures.push(`${baseName(path)}：${String(e)}`);
    }
  }
  uploading.value = false;
  if (failures.length) {
    error.value = `上传失败 — ${failures.join("；")}`;
  }
  await refresh();
}

/** 上传剪贴板图片（截图 / 复制图片后的「粘贴图片」入口，后端统一转 PNG） */
async function uploadClipboardImage() {
  if (uploadingImage.value || !configured.value) return;
  uploadingImage.value = true;
  error.value = "";
  try {
    const key = await invoke<string>("file_hub_upload_image", {
      name: `clipboard-${stamp()}.png`,
    });
    clipboardSaved.value = key;
    window.clearTimeout(savedTimer);
    savedTimer = window.setTimeout(() => {
      clipboardSaved.value = "";
    }, 4000);
    await refresh();
  } catch (e) {
    error.value = `上传剪贴板图片失败：${String(e)}`;
  } finally {
    uploadingImage.value = false;
  }
}

/** 本地时间戳（clipboard-YYYYMMDD-HHmmss.png，同名同秒会覆盖） */
function stamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return (
    `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}` +
    `-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`
  );
}

/** 原生文件拖拽：enter/over 显示遮罩，drop 后逐个上传 */
function onDragDrop(payload: DragDropEvent) {
  if (payload.type === "enter" || payload.type === "over") {
    dragging.value = true;
    return;
  }
  dragging.value = false;
  if (payload.type === "drop" && payload.paths.length) {
    void uploadPaths(payload.paths);
  }
}

/** Ctrl+V：剪贴板里是图片（或没有文本）时直接上传剪贴板图片；粘贴文本时不拦截 */
function onPaste(e: ClipboardEvent) {
  const text = e.clipboardData?.getData("text/plain") ?? "";
  const hasImage = Array.from(e.clipboardData?.items ?? []).some((item) =>
    item.type.startsWith("image/"),
  );
  if (!hasImage && text.trim()) return;
  e.preventDefault();
  void uploadClipboardImage();
}

/** 独立小窗口聚焦时兜底刷新（与后端 filehub://refresh 双保险） */
function onWindowFocus() {
  void reload();
}

async function download(file: HubFile) {
  let target: string | null = null;
  try {
    target = await save({ title: "保存文件", defaultPath: file.key });
  } catch (e) {
    error.value = `打开保存对话框失败：${String(e)}`;
    return;
  }
  if (!target) return;

  const transferId = `down-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  error.value = "";
  downloadedPath.value = "";
  try {
    await invoke("file_hub_download", {
      key: file.key,
      targetPath: target,
      transferId,
    });
    downloadedPath.value = target;
  } catch (e) {
    delete transfers.value[transferId];
    error.value = `下载失败：${String(e)}`;
  }
}

/** 删除需要二次确认：第一次点击进入 3 秒待确认态 */
function armDelete(key: string) {
  if (deleteArmed.value === key) {
    void doDelete(key);
    return;
  }
  deleteArmed.value = key;
  window.clearTimeout(disarmTimer);
  disarmTimer = window.setTimeout(() => {
    deleteArmed.value = "";
  }, 3000);
}

async function doDelete(key: string) {
  deleteArmed.value = "";
  error.value = "";
  try {
    await invoke("file_hub_delete", { key });
    await refresh();
  } catch (e) {
    error.value = `删除失败：${String(e)}`;
  }
}

function onProgress(payload: ProgressPayload) {
  if (payload.phase !== "progress") {
    // 完成 / 失败都会结束进度行（失败详情由命令返回值在错误条里展示）
    delete transfers.value[payload.transferId];
    return;
  }
  transfers.value[payload.transferId] = {
    key: payload.key,
    direction: payload.direction,
    transferred: payload.transferred,
    total: payload.total,
  };
}

function goSettings() {
  if (props.compact) {
    // 独立小窗口里没有设置页：唤起主窗口去配置
    void invoke("open_main_window").catch(() => {});
  } else {
    location.hash = "settings";
  }
}

async function revealDownloaded() {
  if (!downloadedPath.value) return;
  try {
    await revealItemInDir(downloadedPath.value);
  } catch {
    /* 文件被移动/删除时忽略 */
  }
}

function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}

function formatTime(iso: string): string {
  if (!iso) return "-";
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleString();
}

function percentOf(t: Transfer): number {
  if (t.total <= 0) return 0;
  // 大文件上传时本地读取进度可能先于实际传输到 100%，封顶 99% 直到完成事件
  return Math.min(99, Math.floor((t.transferred / t.total) * 100));
}

onMounted(async () => {
  try {
    unlisten = await listen<ProgressPayload>("filehub://progress", (e) =>
      onProgress(e.payload),
    );
    // 打开 / 聚焦文件中转站小窗口时后端触发一次刷新
    unlistenRefresh = await listen("filehub://refresh", () => {
      void reload();
    });
    // 原生文件拖拽（拖到页面任意位置即可上传；dragDropEnabled 窗口默认开启）
    unlistenDragDrop = await getCurrentWindow().onDragDropEvent((e) =>
      onDragDrop(e.payload),
    );
  } catch {
    /* 浏览器预览等无 Tauri 环境时忽略 */
  }
  // 独立小窗口聚焦时兜底刷新；Ctrl+V 上传剪贴板图片
  if (props.compact) window.addEventListener("focus", onWindowFocus);
  window.addEventListener("paste", onPaste);
  await reload();
  initialized = true;
});

onActivated(() => {
  // 切回页面时重读配置并刷新（首次挂载由 onMounted 负责，避免重复请求）
  if (initialized) void reload();
});

onUnmounted(() => {
  window.clearTimeout(disarmTimer);
  window.clearTimeout(savedTimer);
  window.removeEventListener("focus", onWindowFocus);
  window.removeEventListener("paste", onPaste);
  unlisten?.();
  unlistenRefresh?.();
  unlistenDragDrop?.();
});
</script>

<template>
  <div class="page" :class="{ compact }">
    <header class="page-head">
      <h1>文件中转站</h1>
      <p>通过 MinIO 在多台设备之间上传、下载、删除文件（bucket：{{ bucketName }}；支持拖拽文件、粘贴剪贴板图片上传；同名文件会覆盖）</p>
    </header>

    <p v-if="error" class="hint err">{{ error }}</p>
    <p v-if="downloadedPath" class="hint ok">
      已保存到 {{ downloadedPath }}
      <button class="link" @click="revealDownloaded">在文件夹中显示</button>
    </p>
    <p v-if="clipboardSaved" class="hint ok">已上传剪贴板图片 {{ clipboardSaved }}</p>

    <section v-if="transferList.length" class="card transfer-card">
      <div v-for="t in transferList" :key="t.id" class="transfer">
        <div class="transfer-head">
          <span class="transfer-name" :title="t.key">
            {{ t.direction === "upload" ? "上传" : "下载" }} · {{ t.key }}
          </span>
          <span class="transfer-size">
            {{ formatSize(t.transferred) }} / {{ t.total > 0 ? formatSize(t.total) : "…" }}
          </span>
        </div>
        <div class="progress">
          <div class="bar" :style="{ width: `${t.total > 0 ? percentOf(t) : 30}%` }"></div>
        </div>
      </div>
    </section>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>文件列表</h2>
          <p class="card-sub">
            <template v-if="configured">共 {{ files.length }} 个文件</template>
            <template v-else>未配置 MinIO</template>
          </p>
        </div>
        <div class="actions">
          <button :disabled="!configured || loading" @click="refresh">刷新</button>
          <button
            :disabled="!configured || uploadingImage"
            @click="uploadClipboardImage"
          >
            {{ uploadingImage ? "上传中…" : "粘贴图片" }}
          </button>
          <button
            class="primary"
            :disabled="!configured || uploading"
            @click="upload"
          >
            {{ uploading ? "上传中…" : "上传文件" }}
          </button>
        </div>
      </div>

      <!-- 未配置：引导去系统设置 -->
      <div v-if="configLoaded && !configured" class="empty">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6">
          <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
          <path d="M12 16v-5.5" />
          <path d="m9.5 12.5 2.5-2.5 2.5 2.5" />
        </svg>
        <p>尚未配置 MinIO，文件中转站暂不可用</p>
        <button class="primary" @click="goSettings">
          {{ compact ? "打开主窗口设置" : "去系统设置" }}
        </button>
      </div>

      <!-- 已配置：文件表格 -->
      <div v-else-if="files.length" class="table-wrap">
        <table class="file-table">
          <thead>
            <tr>
              <th>文件名</th>
              <th class="num">大小</th>
              <th class="time">修改时间</th>
              <th class="ops">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="f in files" :key="f.key">
              <td class="name" :title="f.key">{{ f.key }}</td>
              <td class="num">{{ formatSize(f.size) }}</td>
              <td class="time">{{ formatTime(f.lastModified) }}</td>
              <td class="ops">
                <button class="ghost" @click="download(f)">下载</button>
                <button
                  class="ghost"
                  :class="{ danger: deleteArmed === f.key }"
                  @click="armDelete(f.key)"
                >
                  {{ deleteArmed === f.key ? "确认删除" : "删除" }}
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <div v-else class="empty">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6">
          <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
          <path d="M12 16v-5.5" />
          <path d="m9.5 12.5 2.5-2.5 2.5 2.5" />
        </svg>
        <p>
          {{
            loading
              ? "正在加载…"
              : "暂无文件：可「上传文件」、拖拽文件到页面，或「粘贴图片」上传剪贴板截图"
          }}
        </p>
      </div>
    </section>

    <!-- 拖拽悬停遮罩（pointer-events: none，不挡原生拖放事件） -->
    <div v-if="dragging" class="drop-mask">
      <div class="drop-tip">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6">
          <path d="M12 16V4" />
          <path d="m7 9 5-5 5 5" />
          <path d="M4 20h16" />
        </svg>
        <p>释放以上传文件到「{{ bucketName }}」</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.transfer-card {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.transfer-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 6px;
}

.transfer-name {
  font-size: 0.82rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.transfer-size {
  font-size: 0.76rem;
  color: var(--text-muted);
  flex-shrink: 0;
}

.table-wrap {
  overflow-x: auto;
}

.file-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 0.84rem;
}

.file-table th {
  padding: 6px 10px;
  border-bottom: 1px solid var(--border);
  color: var(--text-muted);
  font-size: 0.76rem;
  font-weight: 500;
  text-align: left;
  white-space: nowrap;
}

.file-table td {
  padding: 9px 10px;
  border-bottom: 1px solid var(--border);
  vertical-align: middle;
}

.file-table tbody tr:last-child td {
  border-bottom: none;
}

.file-table tbody tr:hover {
  background: var(--bg-hover);
}

.file-table td.name {
  max-width: 320px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.file-table td.num {
  white-space: nowrap;
  color: var(--text-secondary);
}

.file-table td.time {
  white-space: nowrap;
  color: var(--text-muted);
}

.file-table td.ops {
  text-align: right;
  white-space: nowrap;
}

.file-table td.ops button {
  padding: 5px 10px;
  font-size: 0.78rem;
}

.file-table td.ops button + button {
  margin-left: 6px;
}

button.ghost.danger {
  color: var(--err);
  border-color: var(--err);
}

button.link {
  padding: 0;
  border: none;
  background: none;
  color: var(--accent);
  font-size: inherit;
  cursor: pointer;
  text-decoration: underline;
}

button.link:hover {
  background: none;
}

/* 拖拽上传遮罩 */
.drop-mask {
  position: fixed;
  inset: 0;
  z-index: 60;
  display: grid;
  place-items: center;
  padding: 24px;
  background: var(--accent-soft);
  pointer-events: none;
}

.drop-tip {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 26px 44px;
  border: 2px dashed var(--accent);
  border-radius: var(--radius);
  background: var(--bg-elevated);
  box-shadow: var(--shadow);
  text-align: center;
}

.drop-tip svg {
  width: 30px;
  height: 30px;
  color: var(--accent);
}

.drop-tip p {
  margin: 0;
  color: var(--text);
  font-size: 0.88rem;
}

/* 独立小窗口紧凑布局 */
.page.compact {
  padding: 14px 16px 20px;
  max-width: none;
}

.page.compact .page-head {
  margin-bottom: 12px;
}

.page.compact .page-head h1 {
  font-size: 1.05rem;
}

.page.compact .page-head p {
  display: none;
}

.page.compact .card {
  margin-bottom: 10px;
}

.page.compact .card:last-child {
  margin-bottom: 0;
}
</style>
