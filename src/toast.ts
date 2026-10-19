import { onUnmounted, ref } from "vue";

/* 页面级轻提示（toast）：1.8s 自动消失，多页面共用 */

/** 一个页面实例内的 toast 状态与触发器；样式使用 styles.css 的全局 .toast */
export function useToast() {
  const toast = ref("");
  let timer: number | undefined;

  function showToast(text: string) {
    toast.value = text;
    clearTimeout(timer);
    timer = window.setTimeout(() => (toast.value = ""), 1800);
  }

  onUnmounted(() => clearTimeout(timer));
  return { toast, showToast };
}
