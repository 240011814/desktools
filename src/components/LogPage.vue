<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { LogEntry, LogLevel } from "../store";
import ConfirmDialog from "./ConfirmDialog.vue";

const LEVELS: Array<LogLevel | "ALL"> = ["ALL", "INFO", "WARN", "ERROR"];

const entries = ref<LogEntry[]>([]);
const filter = ref<LogLevel | "ALL">("ALL");
const toast = ref("");
const confirmClearOpen = ref(false);
const clearing = ref(false);
let toastTimer: number | undefined;
let unlisten: UnlistenFn | undefined;

const filtered = computed(() =>
  filter.value === "ALL"
    ? entries.value
    : entries.value.filter((e) => e.level === filter.value),
);

async function load() {
  try {
    entries.value = await invoke<LogEntry[]>("get_logs");
  } catch {
    entries.value = [];
  }
}

function showToast(text: string) {
  toast.value = text;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (toast.value = ""), 1800);
}

/** 弹窗确认后清空全部日志 */
async function clearAll() {
  if (clearing.value) return;
  clearing.value = true;
  try {
    await invoke("clear_logs");
    await load();
    confirmClearOpen.value = false;
    showToast("日志已清空");
  } catch (e) {
    confirmClearOpen.value = false;
    showToast(`清空失败：${String(e)}`);
  } finally {
    clearing.value = false;
  }
}

function formatTime(ts: number): string {
  const d = new Date(ts);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

onMounted(async () => {
  await load();
  try {
    unlisten = await listen("log://changed", () => load());
  } catch {
    /* ignore */
  }
});

onUnmounted(() => {
  unlisten?.();
  clearTimeout(toastTimer);
});
</script>

<template>
  <div class="page">
    <header class="page-head">
      <h1>日志</h1>
      <p>应用运行日志，用于排查连接与同步问题，最多保留最近 1000 条</p>
    </header>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>运行日志</h2>
          <p class="card-sub">共 {{ entries.length }} 条 · 当前显示 {{ filtered.length }} 条</p>
        </div>
        <div class="actions">
          <button :disabled="!entries.length" @click="load">刷新</button>
          <button class="danger" :disabled="!entries.length" @click="confirmClearOpen = true">
            清空日志
          </button>
        </div>
      </div>

      <div class="filters">
        <button
          v-for="level in LEVELS"
          :key="level"
          class="filter"
          :class="{ active: filter === level }"
          @click="filter = level"
        >
          {{ level === "ALL" ? "全部" : level }}
        </button>
      </div>

      <div v-if="filtered.length" class="log-list">
        <div v-for="(entry, index) in filtered" :key="index" class="log-item">
          <span class="log-time">{{ formatTime(entry.ts) }}</span>
          <span class="badge" :class="entry.level.toLowerCase()">{{ entry.level }}</span>
          <span class="log-msg">{{ entry.message }}</span>
        </div>
      </div>

      <div v-else class="empty">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6">
          <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
          <path d="M14 3v5h5" />
          <path d="M9 13h6" />
          <path d="M9 17h4" />
        </svg>
        <p>{{ entries.length ? "该级别下暂无日志" : "暂无日志，连接与同步事件会记录在这里" }}</p>
      </div>

      <p class="hint">
        日志同时保存在本机日志文件中（自动轮转），重启应用后仍可查看。
      </p>
    </section>

    <!-- 清空确认弹窗（替代原两段式「确认清空？」按钮） -->
    <ConfirmDialog
      v-if="confirmClearOpen"
      title="确认清空日志"
      confirm-label="清空"
      busy-label="清空中…"
      :busy="clearing"
      @confirm="clearAll"
      @cancel="confirmClearOpen = false"
    >
      <p>将删除全部 {{ entries.length }} 条日志记录，且无法恢复。</p>
    </ConfirmDialog>

    <div v-if="toast" class="toast">{{ toast }}</div>
  </div>
</template>

<style scoped>
.filters {
  display: flex;
  gap: 6px;
  margin-bottom: 12px;
}

.filter {
  padding: 4px 13px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: transparent;
  color: var(--text-secondary);
  font-size: 0.76rem;
}

.filter:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent);
}

.filter.active {
  border-color: transparent;
  background: var(--accent-soft);
  color: var(--accent);
  font-weight: 600;
}

.log-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  max-height: 56vh;
  overflow-y: auto;
  padding: 4px 6px 4px 2px;
}

.log-item {
  display: flex;
  align-items: baseline;
  gap: 10px;
  padding: 5px 8px;
  border-radius: 6px;
  font-size: 0.79rem;
  line-height: 1.55;
}

.log-item:hover {
  background: var(--bg-hover);
}

.log-time {
  flex-shrink: 0;
  color: var(--text-muted);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.73rem;
}

.badge {
  flex-shrink: 0;
  padding: 0 8px;
  border-radius: 999px;
  font-size: 0.68rem;
  font-weight: 600;
  letter-spacing: 0.3px;
}

.badge.info {
  color: var(--accent);
  background: var(--accent-soft);
}

.badge.warn {
  color: var(--warn);
  background: var(--warn-soft);
}

.badge.error {
  color: var(--err);
  background: var(--err-soft);
}

.log-msg {
  min-width: 0;
  word-break: break-all;
  user-select: text;
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
}
</style>
