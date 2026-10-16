<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import HistoryPage from "./components/HistoryPage.vue";
import FileHubPage from "./components/FileHubPage.vue";
import LogPage from "./components/LogPage.vue";
import UpdatePage from "./components/UpdatePage.vue";
import SettingsPage from "./components/SettingsPage.vue";
import { mqttStatus, version, STATUS_TEXT, type MqttStatus } from "./store";

/** 独立历史小窗口模式（桌面右键 / 托盘菜单弹出） */
const isPopup = new URLSearchParams(location.search).get("popup") === "history";

/* 侧边栏菜单（icon 为 24x24 stroke 图标） */
const ICONS: Record<string, string> = {
  history:
    '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3.5 2"/>',
  log: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/><path d="M9 13h6"/><path d="M9 17h4"/>',
  files:
    '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M12 16v-5.5"/><path d="m9.5 12.5 2.5-2.5 2.5 2.5"/>',
  update:
    '<path d="M21 15v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-3"/><path d="M7.5 10.5 12 15l4.5-4.5"/><path d="M12 15V4"/>',
  settings:
    '<path d="M4 21v-6"/><path d="M4 11V3"/><path d="M12 21v-8"/><path d="M12 9V3"/><path d="M20 21v-4"/><path d="M20 13V3"/><path d="M1.5 15h5"/><path d="M9.5 9h5"/><path d="M17.5 17h5"/>',
};

const TABS = [
  { id: "history", label: "剪贴板历史", icon: "history" },
  { id: "files", label: "文件中转站", icon: "files" },
  { id: "log", label: "日志", icon: "log" },
  { id: "update", label: "软件更新", icon: "update" },
  { id: "settings", label: "系统设置", icon: "settings" },
] as const;

type TabId = (typeof TABS)[number]["id"];

const pages: Record<TabId, unknown> = {
  history: HistoryPage,
  files: FileHubPage,
  log: LogPage,
  update: UpdatePage,
  settings: SettingsPage,
};

function tabFromHash(): TabId {
  const hash = location.hash.slice(1) as TabId;
  return TABS.some((tab) => tab.id === hash) ? hash : "history";
}

const active = ref<TabId>(tabFromHash());
function select(tab: TabId) {
  active.value = tab;
  if (location.hash.slice(1) !== tab) {
    location.hash = tab;
  }
}

const currentPage = computed(() => pages[active.value]);
const statusLabel = computed(() => STATUS_TEXT[mqttStatus.value.state] ?? "未知");

onMounted(async () => {
  try {
    version.value = await getVersion();
  } catch {
    version.value = "未知";
  }
  try {
    mqttStatus.value = await invoke<MqttStatus>("get_mqtt_status");
  } catch {
    /* ignore */
  }
  listen<MqttStatus>("mqtt://status", (e) => {
    mqttStatus.value = e.payload;
  }).catch(() => {});
  window.addEventListener("hashchange", () => {
    active.value = tabFromHash();
  });
});
</script>

<template>
  <!-- 独立历史小窗口（托盘 / 桌面右键弹出） -->
  <HistoryPage v-if="isPopup" compact />

  <div v-else class="shell">
    <aside class="sidebar">
      <div class="brand">
        <div class="logo">
          <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
            <path
              fill="currentColor"
              d="M9.5 3A3.5 3.5 0 0 0 6 6.5V8H5a3 3 0 0 0-3 3v6a3 3 0 0 0 3 3h1v-1.5A3.5 3.5 0 0 0 9.5 15H15a3 3 0 0 0 3-3V6.5A3.5 3.5 0 0 0 14.5 3h-5Zm0 2h5a1.5 1.5 0 0 1 1.5 1.5V12a1 1 0 0 1-1 1H9.5A1.5 1.5 0 0 1 8 11.5V6.5A1.5 1.5 0 0 1 9.5 5ZM5 10.5h3v2.5a2.5 2.5 0 0 1-2.5 2.5H5V18h1a1 1 0 0 0 1-1v-2.5A2.5 2.5 0 0 0 4.5 12H4v-1.5h1Z"
            />
          </svg>
        </div>
        <div class="brand-text">
          <strong>DeskTools</strong>
          <span>跨设备剪贴板同步</span>
        </div>
      </div>

      <nav class="nav">
        <button
          v-for="tab in TABS"
          :key="tab.id"
          class="nav-item"
          :class="{ active: active === tab.id }"
          @click="select(tab.id)"
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.7"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            v-html="ICONS[tab.icon]"
          />
          <span>{{ tab.label }}</span>
        </button>
      </nav>

      <div class="sidebar-foot">
        <div class="status-pill" :class="mqttStatus.state">
          <span class="dot"></span>
          {{ statusLabel }}
        </div>
        <span class="foot-version">DeskTools · v{{ version }}</span>
      </div>
    </aside>

    <main class="content">
      <KeepAlive>
        <component :is="currentPage" />
      </KeepAlive>
    </main>
  </div>
</template>

<style scoped>
.shell {
  display: flex;
  height: 100vh;
  overflow: hidden;
}

/* ---------- 侧边栏 ---------- */
.sidebar {
  display: flex;
  flex-direction: column;
  width: 214px;
  flex-shrink: 0;
  padding: 18px 12px 14px;
  border-right: 1px solid var(--border);
  background: var(--bg-elevated);
}

.brand {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 2px 6px 0;
}

.logo {
  display: grid;
  place-items: center;
  width: 36px;
  height: 36px;
  border-radius: 10px;
  color: #fff;
  background: linear-gradient(135deg, var(--accent), #7b5cf0);
  flex-shrink: 0;
}

.brand-text {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.brand-text strong {
  font-size: 0.98rem;
  line-height: 1.3;
  letter-spacing: 0.2px;
}

.brand-text span {
  font-size: 0.68rem;
  color: var(--text-muted);
  white-space: nowrap;
}

.nav {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: 22px;
  flex: 1;
}

.nav-item {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 9px 12px;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--text-secondary);
  font-size: 0.87rem;
  text-align: left;
}

.nav-item svg {
  width: 17px;
  height: 17px;
  flex-shrink: 0;
}

.nav-item:hover {
  background: var(--bg-hover);
  color: var(--text);
  border: none;
}

.nav-item.active {
  background: var(--accent-soft);
  color: var(--accent);
  font-weight: 600;
  border: none;
}

.sidebar-foot {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 8px;
  padding: 12px 6px 2px;
  border-top: 1px solid var(--border);
}

.foot-version {
  font-size: 0.7rem;
  color: var(--text-muted);
}

/* ---------- 主内容区 ---------- */
.content {
  flex: 1;
  overflow-y: auto;
  min-width: 0;
}
</style>
