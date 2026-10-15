<script setup lang="ts">
import { onMounted, ref } from "vue";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";

const autoStart = ref(false);

onMounted(async () => {
  try {
    autoStart.value = await isEnabled();
  } catch {
    autoStart.value = false;
  }
});

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
</script>

<template>
  <div class="page">
    <header class="page-head">
      <h1>系统设置</h1>
      <p>启动方式与后台运行行为</p>
    </header>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>启动</h2>
          <p class="card-sub">随系统登录自动运行</p>
        </div>
      </div>
      <label class="switch">
        <input type="checkbox" :checked="autoStart" @change="toggleAutoStart" />
        <span class="track"><span class="thumb"></span></span>
        <span class="switch-text">开机自动启动</span>
      </label>
      <p class="hint">
        开机自启时以「仅托盘」模式运行，剪贴板同步在后台继续工作。
      </p>
    </section>

    <section class="card">
      <div class="card-head">
        <div>
          <h2>后台运行</h2>
          <p class="card-sub">关闭窗口不退出程序</p>
        </div>
      </div>
      <p class="hint">
        关闭窗口将最小化到系统托盘，可在托盘图标菜单中选择「显示主窗口」或「退出」。
      </p>
    </section>
  </div>
</template>
