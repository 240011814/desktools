<script setup lang="ts">
import { onMounted, onUnmounted } from "vue";

/* 通用确认弹窗：删除 / 清空等不可逆操作的二次确认（替代原「点击变确认」两段式交互）
 * 关闭方式：取消按钮 / 点击遮罩 / Esc；busy 时全部禁用，防止确认过程中误关、防重复点击。 */
const props = withDefaults(
  defineProps<{
    title: string;
    confirmLabel?: string;
    busyLabel?: string;
    busy?: boolean;
  }>(),
  { confirmLabel: "确认", busyLabel: "处理中…", busy: false },
);

const emit = defineEmits<{ confirm: []; cancel: [] }>();

function close() {
  if (props.busy) return;
  emit("cancel");
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") close();
}

onMounted(() => window.addEventListener("keydown", onKeydown));
onUnmounted(() => window.removeEventListener("keydown", onKeydown));
</script>

<template>
  <div class="overlay" @click.self="close">
    <div class="confirm-panel" role="alertdialog" aria-modal="true">
      <h3 class="confirm-title">{{ title }}</h3>
      <div class="confirm-body">
        <slot />
      </div>
      <footer class="confirm-foot">
        <button class="ghost" :disabled="busy" @click="close">取消</button>
        <button class="danger" :disabled="busy" @click="emit('confirm')">
          {{ busy ? busyLabel : confirmLabel }}
        </button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  z-index: 50;
  display: grid;
  place-items: center;
  padding: 24px;
  background: rgba(10, 14, 22, 0.45);
}

.confirm-panel {
  width: min(400px, 100%);
  padding: 18px 20px 16px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--bg-elevated);
  box-shadow: var(--shadow);
  text-align: left;
}

.confirm-title {
  margin: 0 0 8px;
  font-size: 0.95rem;
  font-weight: 600;
}

.confirm-body {
  font-size: 0.84rem;
  color: var(--text-secondary);
  line-height: 1.65;
  word-break: break-all;
  user-select: text;
}

.confirm-body :deep(p) {
  margin: 0;
}

.confirm-body :deep(p + p) {
  margin-top: 6px;
}

.confirm-foot {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 16px;
}
</style>
