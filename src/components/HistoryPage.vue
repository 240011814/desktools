<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
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

onMounted(async () => {
  await load();
  try {
    unlisten = await listen("history://changed", () => load());
  } catch {
    /* ignore */
  }
});

onUnmounted(() => {
  unlisten?.();
  clearTimeout(toastTimer);
  clearTimeout(confirmTimer);
});
</script>

<template>
  <div class="page" :class="{ compact }">
    <header class="page-head">
      <h1>剪贴板历史</h1>
      <p>自动保存最近 {{ HISTORY_LIMIT }} 条复制记录，点击任意条目即可重新复制</p>
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
        <div v-for="item in items" :key="item.id" class="h-item" @click="copyItem(item)">
          <div class="h-body">
            <p class="h-text">{{ item.text }}</p>
            <div class="h-meta">
              <span class="badge" :class="item.source">
                {{ item.source === "local" ? "本机复制" : "远端同步" }}
              </span>
              <span class="h-time">{{ formatTime(item.ts) }}</span>
              <span class="h-count">{{ item.text.length }} 字符</span>
            </div>
          </div>
          <div class="h-actions" @click.stop>
            <button class="ghost mini" title="复制到剪贴板" @click="copyItem(item)">复制</button>
            <button class="ghost mini" title="删除此条" @click="removeItem(item)">删除</button>
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
        相同内容不会重复记录，重复复制时会更新时间并置顶；远端同步来的内容标记为「远端同步」。
      </p>
    </section>

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

.h-actions {
  display: flex;
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
