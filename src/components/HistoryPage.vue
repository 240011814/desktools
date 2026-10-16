<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { homeDir } from "@tauri-apps/api/path";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { detectContent, type ContentInfo } from "../detect";
import { renderMarkdown } from "../markdown";
import type { HistoryItem } from "../store";

/** compact：独立历史小窗口布局（桌面右键 / 托盘弹出） */
defineProps<{ compact?: boolean }>();

const HISTORY_LIMIT = 200;

const items = ref<HistoryItem[]>([]);
const toast = ref("");
const confirmClear = ref(false);
let toastTimer: number | undefined;
let confirmTimer: number | undefined;
let unlisten: UnlistenFn | undefined;
let unlistenRefresh: UnlistenFn | undefined;

/** 条目 + 内容识别结果（纯本地识别，渲染时计算） */
const decorated = computed(() =>
  items.value.map((item) => ({ item, info: detectContent(item.text) })),
);

/** 预览弹层（JSON 格式化 / Base64 解码 / Markdown 渲染） */
interface PreviewAction {
  label: string;
  text: string;
}

interface PreviewState {
  title: string;
  text?: string;
  html?: string;
  actions: PreviewAction[];
}

const preview = ref<PreviewState | null>(null);
const PREVIEW_LIMIT = 20000;

function clipPreview(text: string): string {
  return text.length > PREVIEW_LIMIT
    ? `${text.slice(0, PREVIEW_LIMIT)}\n…（内容过长，已截断显示）`
    : text;
}

async function load() {
  try {
    items.value = await invoke<HistoryItem[]>("get_history");
  } catch {
    items.value = [];
  }
}

function showToast(text: string) {
  toast.value = text;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (toast.value = ""), 1800);
}

async function copyItem(item: HistoryItem) {
  try {
    // 走后端命令写剪贴板（跨平台一致，不受插件 IPC 权限限制）
    await invoke("copy_to_clipboard", { text: item.text });
    showToast("已复制到剪贴板");
  } catch (e) {
    showToast(`复制失败：${String(e)}`);
  }
}

async function removeItem(item: HistoryItem) {
  try {
    await invoke("delete_history", { id: item.id });
    await load();
  } catch (e) {
    showToast(`删除失败：${String(e)}`);
  }
}

async function clearAll() {
  if (!confirmClear.value) {
    confirmClear.value = true;
    clearTimeout(confirmTimer);
    confirmTimer = window.setTimeout(() => (confirmClear.value = false), 3000);
    return;
  }
  confirmClear.value = false;
  try {
    await invoke("clear_history");
    await load();
    showToast("已清空历史记录");
  } catch (e) {
    showToast(`清空失败：${String(e)}`);
  }
}

/** 通用复制（带自定义提示，供颜色 / 算式 / 预览等快捷操作复用） */
async function copyText(text: string, okText: string) {
  try {
    await invoke("copy_to_clipboard", { text });
    showToast(okText);
  } catch (e) {
    showToast(`复制失败：${String(e)}`);
  }
}

/** 用默认应用打开链接 / mailto: / tel: */
async function openExternal(url: string) {
  try {
    await openUrl(url);
  } catch (e) {
    showToast(`打开失败：${String(e)}`);
  }
}

/** 在访达 / 资源管理器中定位文件（~ 展开为本机用户目录） */
async function revealPath(path: string) {
  try {
    let target = path;
    if (target === "~" || target.startsWith("~/")) {
      let home = "";
      try {
        home = await homeDir();
      } catch {
        home = "";
      }
      if (home) {
        target = target === "~" ? home : `${home.replace(/[\\/]$/, "")}${target.slice(1)}`;
      }
    }
    await revealItemInDir(target);
    showToast("已打开文件位置");
  } catch (e) {
    showToast(`打开位置失败：${String(e)}`);
  }
}

/* 以下为模板用的薄封装（避免在模板里做类型收窄） */

function openItemLink(info: ContentInfo) {
  if (info.link) void openExternal(info.link);
}

function revealItemPath(info: ContentInfo) {
  if (info.path) void revealPath(info.path);
}

function copyColor(info: ContentInfo, format: "hex" | "rgb") {
  if (!info.color) return;
  void copyText(format === "hex" ? info.color.hex : info.color.rgb, `已复制 ${format.toUpperCase()}`);
}

function copyMathResult(info: ContentInfo) {
  if (info.result === undefined) return;
  void copyText(info.result, `计算结果：${info.result}（已复制）`);
}

/** 打开内容预览弹层（kind=json / base64 / markdown） */
function openPreview(item: HistoryItem, info: ContentInfo) {
  if (info.kind === "json" && info.json) {
    preview.value = {
      title: `JSON 格式化预览 · ${info.json.min.length} 字符`,
      text: clipPreview(info.json.pretty),
      actions: [
        { label: "复制格式化", text: info.json.pretty },
        { label: "复制压缩", text: info.json.min },
      ],
    };
  } else if (info.kind === "base64" && info.decoded !== undefined) {
    preview.value = {
      title: `Base64 解码预览 · ${info.decoded.length} 字符`,
      text: clipPreview(info.decoded),
      actions: [{ label: "复制解码结果", text: info.decoded }],
    };
  } else if (info.kind === "markdown") {
    preview.value = {
      title: "Markdown 渲染预览",
      html: renderMarkdown(clipPreview(item.text)),
      actions: [{ label: "复制原文", text: item.text }],
    };
  }
}

function closePreview() {
  preview.value = null;
}

/** 预览弹层里点击 Markdown 链接：交给系统浏览器打开（不做 webview 内导航） */
function onPreviewClick(event: MouseEvent) {
  const target = (event.target as HTMLElement).closest("a[data-url]");
  if (!target) return;
  event.preventDefault();
  const url = target.getAttribute("data-url");
  if (url) void openExternal(url);
}

function formatTime(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const diff = (now.getTime() - ts) / 1000;
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  if (diff < 60) return "刚刚";
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (d.toDateString() === now.toDateString()) return `今天 ${hm}`;
  const yesterday = new Date(now.getTime() - 86400000);
  if (d.toDateString() === yesterday.toDateString()) return `昨天 ${hm}`;
  return `${d.getMonth() + 1}-${d.getDate()} ${hm}`;
}

/** 窗口聚焦时兜底刷新（与后端 history://refresh 双保险，防止事件丢失导致旧数据） */
function onWindowFocus() {
  void load();
}

/** Esc 关闭预览弹层 */
function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape" && preview.value) closePreview();
}

onMounted(async () => {
  await load();
  try {
    unlisten = await listen("history://changed", () => load());
    // 打开 / 聚焦剪贴板历史窗口时后端触发一次刷新
    unlistenRefresh = await listen("history://refresh", () => load());
  } catch {
    /* ignore */
  }
  window.addEventListener("focus", onWindowFocus);
  window.addEventListener("keydown", onKeydown);
});

onUnmounted(() => {
  window.removeEventListener("focus", onWindowFocus);
  window.removeEventListener("keydown", onKeydown);
  unlisten?.();
  unlistenRefresh?.();
  clearTimeout(toastTimer);
  clearTimeout(confirmTimer);
});
</script>

<template>
  <div class="page" :class="{ compact }">
    <header class="page-head">
      <h1>剪贴板历史</h1>
      <p>自动保存最近 {{ HISTORY_LIMIT }} 条复制记录，点击任意条目即可重新复制；识别到链接、颜色、JSON 等内容时会提供快捷操作</p>
    </header>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>记录列表</h2>
          <p class="card-sub">共 {{ items.length }} 条 · 上限 {{ HISTORY_LIMIT }} 条</p>
        </div>
        <div class="actions">
          <button :disabled="!items.length" @click="load">刷新</button>
          <button class="danger" :disabled="!items.length" @click="clearAll">
            {{ confirmClear ? "确认清空？" : "一键清空" }}
          </button>
        </div>
      </div>

      <div v-if="items.length" class="history-list">
        <div v-for="row in decorated" :key="row.item.id" class="h-item" @click="copyItem(row.item)">
          <div class="h-body">
            <p class="h-text">{{ row.item.text }}</p>
            <div class="h-meta">
              <span class="badge" :class="row.item.source">
                {{ row.item.source === "local" ? "本机复制" : "远端同步" }}
              </span>
              <span v-if="row.info.kind !== 'text'" class="badge kind">{{ row.info.label }}</span>
              <span
                v-if="row.info.color"
                class="swatch"
                :style="{ background: row.info.color.hex }"
                :title="`${row.info.color.hex} · ${row.info.color.rgb}`"
              ></span>
              <span class="h-time">{{ formatTime(row.item.ts) }}</span>
              <span class="h-count">{{ row.item.text.length }} 字符</span>
            </div>
          </div>
          <div class="h-actions" @click.stop>
            <!-- 内容识别后的个性化快捷操作（均为点击手动触发） -->
            <button
              v-if="row.info.kind === 'url'"
              class="ghost mini"
              title="用默认浏览器打开"
              @click="openItemLink(row.info)"
            >
              打开
            </button>
            <button
              v-else-if="row.info.kind === 'email'"
              class="ghost mini"
              title="用默认邮件客户端写邮件"
              @click="openItemLink(row.info)"
            >
              写邮件
            </button>
            <button
              v-else-if="row.info.kind === 'phone'"
              class="ghost mini"
              title="拨打此号码"
              @click="openItemLink(row.info)"
            >
              拨打
            </button>
            <button
              v-else-if="row.info.kind === 'path'"
              class="ghost mini"
              title="在访达 / 资源管理器中定位"
              @click="revealItemPath(row.info)"
            >
              打开位置
            </button>
            <template v-else-if="row.info.kind === 'color'">
              <button class="ghost mini" title="复制 HEX 颜色" @click="copyColor(row.info, 'hex')">
                HEX
              </button>
              <button class="ghost mini" title="复制 RGB 颜色" @click="copyColor(row.info, 'rgb')">
                RGB
              </button>
            </template>
            <button
              v-else-if="row.info.kind === 'json'"
              class="ghost mini"
              title="格式化预览 / 压缩复制"
              @click="openPreview(row.item, row.info)"
            >
              格式化
            </button>
            <button
              v-else-if="row.info.kind === 'base64'"
              class="ghost mini"
              title="解码预览"
              @click="openPreview(row.item, row.info)"
            >
              解码
            </button>
            <button
              v-else-if="row.info.kind === 'markdown'"
              class="ghost mini"
              title="渲染预览"
              @click="openPreview(row.item, row.info)"
            >
              预览
            </button>
            <button
              v-else-if="row.info.kind === 'math'"
              class="ghost mini"
              title="复制计算结果"
              @click="copyMathResult(row.info)"
            >
              = {{ row.info.result }}
            </button>
            <button class="ghost mini" title="复制到剪贴板" @click="copyItem(row.item)">复制</button>
            <button class="ghost mini" title="删除此条" @click="removeItem(row.item)">删除</button>
          </div>
        </div>
      </div>

      <div v-else class="empty">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6">
          <rect x="8" y="2.5" width="8" height="4" rx="1.2" />
          <path d="M16 4.5h2a2 2 0 0 1 2 2v13a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2v-13a2 2 0 0 1 2-2h2" />
        </svg>
        <p>暂无记录，复制文本后会自动出现在这里</p>
      </div>

      <p class="hint">
        相同内容不会重复记录，重复复制时会更新时间并置顶；远端同步来的内容标记为「远端同步」。内容类型识别与快捷操作全部在本机完成、不联网。
      </p>
    </section>

    <!-- 内容预览弹层：JSON 格式化 / Base64 解码 / Markdown 渲染 -->
    <div v-if="preview" class="overlay" @click.self="closePreview">
      <div class="preview-panel">
        <header class="preview-head">
          <h3>{{ preview.title }}</h3>
          <button class="ghost mini" @click="closePreview">关闭</button>
        </header>
        <div
          v-if="preview.html"
          class="preview-body md-body"
          v-html="preview.html"
          @click="onPreviewClick"
        ></div>
        <pre v-else class="preview-body preview-pre">{{ preview.text }}</pre>
        <footer class="preview-foot">
          <button
            v-for="action in preview.actions"
            :key="action.label"
            class="ghost mini"
            @click="copyText(action.text, '已复制到剪贴板')"
          >
            {{ action.label }}
          </button>
        </footer>
      </div>
    </div>

    <div v-if="toast" class="toast">{{ toast }}</div>
  </div>
</template>

<style scoped>
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

.page.compact .page-head p,
.page.compact > .card > .hint {
  display: none;
}

.page.compact .card {
  margin-bottom: 0;
  padding: 14px 14px;
}

.page.compact .history-list {
  max-height: calc(100vh - 190px);
}

.history-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
  max-height: 52vh;
  overflow-y: auto;
  padding-right: 4px;
}

.h-item {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-input);
  cursor: pointer;
  transition: border-color 0.15s, background 0.15s;
}

.h-item:hover {
  border-color: var(--accent);
  background: var(--bg-hover);
}

.h-body {
  min-width: 0;
  flex: 1;
}

.h-text {
  margin: 0 0 5px;
  font-size: 0.84rem;
  line-height: 1.5;
  white-space: pre-wrap;
  word-break: break-all;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  user-select: text;
}

.h-meta {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 0.72rem;
  color: var(--text-muted);
}

.badge {
  padding: 1px 8px;
  border-radius: 999px;
  font-weight: 500;
}

.badge.local {
  color: var(--accent);
  background: var(--accent-soft);
}

.badge.remote {
  color: var(--ok);
  background: var(--ok-soft);
}

/* 内容识别出的类型徽章 */
.badge.kind {
  color: var(--text-secondary);
  background: var(--bg-hover);
}

/* 颜色预览色块 */
.swatch {
  width: 13px;
  height: 13px;
  border-radius: 4px;
  border: 1px solid var(--border);
  flex-shrink: 0;
}

.h-actions {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 4px;
  flex-shrink: 0;
  opacity: 0;
  transition: opacity 0.15s;
}

.h-item:hover .h-actions {
  opacity: 1;
}

button.mini {
  padding: 3px 9px;
  font-size: 0.74rem;
}

/* ---------- 内容预览弹层 ---------- */
.overlay {
  position: fixed;
  inset: 0;
  z-index: 40;
  display: grid;
  place-items: center;
  padding: 24px;
  background: rgba(10, 14, 22, 0.45);
}

.page.compact .overlay {
  padding: 12px;
}

.preview-panel {
  display: flex;
  flex-direction: column;
  width: min(680px, 100%);
  max-height: 78vh;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--bg-elevated);
  box-shadow: var(--shadow);
  overflow: hidden;
}

.preview-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 11px 16px;
  border-bottom: 1px solid var(--border);
}

.preview-head h3 {
  margin: 0;
  font-size: 0.9rem;
  font-weight: 600;
}

.preview-body {
  margin: 0;
  padding: 14px 16px;
  overflow: auto;
  font-size: 0.82rem;
  line-height: 1.65;
  user-select: text;
}

.preview-pre {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  white-space: pre-wrap;
  word-break: break-all;
}

.preview-foot {
  display: flex;
  justify-content: flex-end;
  gap: 6px;
  padding: 10px 16px;
  border-top: 1px solid var(--border);
}

/* ---------- Markdown 渲染预览 ---------- */
.md-body :deep(h1),
.md-body :deep(h2),
.md-body :deep(h3),
.md-body :deep(h4),
.md-body :deep(h5),
.md-body :deep(h6) {
  margin: 0.7em 0 0.4em;
  line-height: 1.35;
}

.md-body :deep(h1) {
  font-size: 1.15rem;
}

.md-body :deep(h2) {
  font-size: 1.05rem;
}

.md-body :deep(h3) {
  font-size: 0.95rem;
}

.md-body :deep(h4),
.md-body :deep(h5),
.md-body :deep(h6) {
  font-size: 0.88rem;
}

.md-body :deep(p) {
  margin: 0.4em 0;
}

.md-body :deep(ul),
.md-body :deep(ol) {
  margin: 0.4em 0;
  padding-left: 1.5em;
}

.md-body :deep(li) {
  margin: 0.15em 0;
}

.md-body :deep(blockquote) {
  margin: 0.5em 0;
  padding: 2px 10px;
  border-left: 3px solid var(--accent);
  color: var(--text-secondary);
}

.md-body :deep(code) {
  padding: 1px 5px;
  border-radius: 5px;
  background: var(--bg-hover);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.86em;
}

.md-body :deep(.md-pre) {
  margin: 0.5em 0;
  padding: 10px 12px;
  border-radius: var(--radius-sm);
  background: var(--bg-hover);
  overflow: auto;
}

.md-body :deep(.md-pre code) {
  padding: 0;
  background: none;
}

.md-body :deep(a.md-link) {
  color: var(--accent);
  text-decoration: underline;
  cursor: pointer;
}

.md-body :deep(del) {
  color: var(--text-muted);
}

.md-body :deep(hr) {
  border: none;
  border-top: 1px solid var(--border);
  margin: 0.9em 0;
}

.md-body :deep(table) {
  border-collapse: collapse;
  margin: 0.6em 0;
}

.md-body :deep(th),
.md-body :deep(td) {
  border: 1px solid var(--border);
  padding: 3px 9px;
}

.toast {
  position: fixed;
  left: 50%;
  bottom: 34px;
  transform: translateX(-50%);
  padding: 8px 18px;
  border-radius: 999px;
  background: var(--text);
  color: var(--bg-elevated);
  font-size: 0.82rem;
  box-shadow: var(--shadow);
  animation: fade-in 0.18s ease-out;
}

@keyframes fade-in {
  from {
    opacity: 0;
    transform: translate(-50%, 6px);
  }
  to {
    opacity: 1;
    transform: translate(-50%, 0);
  }
}
</style>
