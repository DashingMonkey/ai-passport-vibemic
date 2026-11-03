<script setup lang="ts">
// 自绘标题栏:主窗口在 tauri.conf.json 里
// 关掉了原生装饰(decorations:false),拖动/双击最大化靠根元素上的
// data-tauri-drag-region,按钮走 @tauri-apps/api 的窗口命令,权限见
// capabilities/default.json。
//
// 按钮一律 @mousedown.prevent:Chromium 点击会把焦点留在按钮上,窗口隐藏再
// show 时焦点被原样还原,面板一打开就「选中」上次的关闭/最小化按钮(回车还会
// 再触发一次)。
import { computed, onMounted, onUnmounted, ref } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  onThemeChange,
  resolvedTheme,
  themePref,
  toggleTheme,
  type Theme,
  type ThemePref,
} from "../theme";
import { fwHex, state } from "../store";
import logo from "../assets/logo.png";

// 窗口句柄**延迟取**:`getCurrentWindow()` 在没有 Tauri 环境时会抛
// (例如用浏览器直接打开构建产物调样式),放在模块顶层会让整个标题栏组件挂掉、
// 界面缺一条。放进函数里,失败只影响被点的那一下。
function win() {
  return getCurrentWindow();
}

// ---- 设备状态(标题栏常显) ----
// 数据来自共享 store:标题栏不在任何一页里,不能等某页 mount 才有值。
// 电量内芯:满格 20.8 宽(x=3.6 起,外壳内沿到 24.4 留出余量);没读到电量时不画这一格。
const battWidth = computed(() => {
  const pct = state.battery ?? 0;
  return (20.8 * Math.min(100, Math.max(0, pct))) / 100;
});

// 第三个按钮的图标在「最大化 / 向下还原」之间切:双击标题栏、Win+↑、贴靠都会
// 改窗口状态,统一在 resize 事件里回读,不自己猜状态
const maximized = ref(false);
let unlisten: (() => void) | undefined;

async function syncMaximized() {
  try {
    maximized.value = (await win().isMaximized()) ?? false;
  } catch (e) {
    // 窗口可能已被销毁(应用退出中);吞掉以免回调里冒出未处理 rejection
    console.warn("读取最大化状态失败：", e);
  }
}

function minimize() {
  try {
    void win().minimize().catch((e) => console.warn("最小化失败：", e));
  } catch (e) {
    console.warn("最小化失败：", e);
  }
}

function toggleMaximize() {
  try {
    void win().toggleMaximize().catch((e) => console.warn("切换最大化失败：", e));
  } catch (e) {
    console.warn("切换最大化失败：", e);
  }
}

// 关闭 → 只隐藏到托盘(真正的退出走托盘菜单),由 Rust 侧的 CloseRequested 拦下
function close() {
  try {
    void win().close().catch((e) => console.warn("关闭窗口失败：", e));
  } catch (e) {
    console.warn("关闭窗口失败：", e);
  }
}

// ---- 深浅主题切换(窗口按钮最左侧) ----
// 偏好存 localStorage(见 theme.ts);图标/提示跟随当前实际主题,本窗口切换、
// 系统主题变化、其它窗口改主题都经 theme.ts 的 apply() → onThemeChange 同步到这里
const theme = ref<{ theme: Theme; pref: ThemePref }>({
  theme: resolvedTheme(),
  pref: themePref(),
});

const themeTip = computed(() => {
  const cur = theme.value.theme === "dark" ? "深色" : "浅色";
  const following = theme.value.pref === "system" ? "（跟随系统）" : "";
  return `当前${cur}${following} · 点击切换到${theme.value.theme === "dark" ? "浅色" : "深色"}`;
});

function onToggleTheme() {
  // setThemePref → apply() 会同步回调 onThemeChange,状态刷新由订阅完成
  toggleTheme();
}

let unlistenTheme: (() => void) | undefined;

onMounted(async () => {
  await syncMaximized();
  try {
    unlisten = await win().onResized(() => void syncMaximized());
  } catch (e) {
    // 没有 Tauri 环境(浏览器里调样式)时拿不到窗口事件:标题栏照常显示,只是图标不自更新
    console.warn("订阅窗口尺寸变化失败：", e);
  }
  unlistenTheme = onThemeChange((s) => {
    theme.value = s;
  });
});

onUnmounted(() => {
  unlisten?.();
  unlistenTheme?.();
});
</script>

<template>
  <header class="titlebar" data-tauri-drag-region>
    <div class="titlebar-brand">
      <img class="titlebar-logo" :src="logo" alt="" data-tauri-drag-region />
      <span class="titlebar-name" data-tauri-drag-region>VibeMic</span>
    </div>

    <!-- 设备状态(纯展示,不放可点元素:这一段属于标题栏拖拽区)。
         未连接时只剩「未连接」半句 —— 电量与固件版本整块收起,不显示上次的值。 -->
    <div class="titlebar-status" :class="{ off: !state.connected }" data-tauri-drag-region>
      <span class="conn">
        <span class="conn-dot"></span>
        {{ state.connected ? "已连接" : "未连接" }}
      </span>
      <span v-if="state.connected" class="tb-meta">
        <template v-if="state.battery !== null">
          <svg class="batt-ico" viewBox="0 0 32 16" aria-hidden="true">
            <rect class="shell" x="1" y="2.6" width="24" height="10.8" rx="2.4" />
            <rect class="cap" x="26.4" y="6.2" width="2.4" height="3.6" rx="1" />
            <rect class="fill" x="3.6" y="5.2" :width="battWidth" height="5.6" rx="1" />
          </svg>
          <b>{{ state.battery }}%</b>
          <span class="tb-sep"></span>
        </template>
        <span class="tb-fw">固件 v0x{{ fwHex(state.fw_ver) }}</span>
      </span>
    </div>

    <div class="titlebar-controls">
      <button
        type="button"
        class="titlebar-btn"
        :title="themeTip"
        :aria-label="themeTip"
        @mousedown.prevent
        @click="onToggleTheme"
      >
        <!-- 深色显示月亮(点击到浅色),浅色显示太阳(点击到深色) -->
        <svg v-if="theme.theme === 'dark'" class="titlebar-icon" viewBox="0 0 16 16" aria-hidden="true">
          <path d="M13.2 9.8A5.8 5.8 0 1 1 6.2 2.8a4.6 4.6 0 0 0 7 7z" />
        </svg>
        <svg v-else class="titlebar-icon" viewBox="0 0 16 16" aria-hidden="true">
          <circle cx="8" cy="8" r="3" />
          <path d="M8 1v2M8 13v2M1 8h2M13 8h2M3 3l1.4 1.4M11.6 11.6L13 13M13 3l-1.4 1.4M4.4 11.6L3 13" />
        </svg>
      </button>

      <button
        type="button"
        class="titlebar-btn"
        title="最小化"
        aria-label="最小化"
        @mousedown.prevent
        @click="minimize"
      >
        <svg class="titlebar-icon" viewBox="0 0 16 16" aria-hidden="true">
          <path d="M3 8.5h10" />
        </svg>
      </button>

      <button
        type="button"
        class="titlebar-btn"
        :title="maximized ? '向下还原' : '最大化'"
        :aria-label="maximized ? '向下还原' : '最大化'"
        @mousedown.prevent
        @click="toggleMaximize"
      >
        <svg v-if="maximized" class="titlebar-icon" viewBox="0 0 16 16" aria-hidden="true">
          <path d="M10.5 3.5H3.5v7" />
          <rect x="5.5" y="5.5" width="7" height="7" />
        </svg>
        <svg v-else class="titlebar-icon" viewBox="0 0 16 16" aria-hidden="true">
          <rect x="3.5" y="3.5" width="9" height="9" />
        </svg>
      </button>

      <button
        type="button"
        class="titlebar-btn close"
        title="关闭"
        aria-label="关闭"
        @mousedown.prevent
        @click="close"
      >
        <svg class="titlebar-icon" viewBox="0 0 16 16" aria-hidden="true">
          <path d="M3.5 3.5l9 9M12.5 3.5l-9 9" />
        </svg>
      </button>
    </div>
  </header>
</template>
