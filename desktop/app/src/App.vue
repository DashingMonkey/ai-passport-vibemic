<template>
  <div class="app-shell">
    <TitleBar />
    <div class="layout">
      <aside class="sidebar">
        <button
          v-for="t in tabs"
          :key="t.id"
          type="button"
          class="nav-item"
          :class="{ active: active === t.id }"
          :aria-current="active === t.id ? 'page' : undefined"
          @click="active = t.id"
        >
          <!-- 1.3px 描边画法,与标题栏图标同一套 -->
          <svg class="nav-ico" viewBox="0 0 16 16" aria-hidden="true" v-html="t.icon"></svg>
          {{ t.label }}
        </button>
      </aside>

      <main class="content">
        <!-- 设备链接未就绪:非阻塞横幅,点「立即重连」只是提示用户(链路自己会重连),
             放在内容区顶部而不是塞进某一页 —— 任何一页都该看得见 -->
        <div v-if="!state.connected" class="warn-box banner">
          <span class="grow">设备链接未就绪：正在自动重连（已在系统蓝牙中配对后通常几秒内恢复）</span>
        </div>

        <!-- 页面一律 KeepAlive 保活:切页不销毁控件状态(烧录进度、模型下载进度、
             「检查更新」的结论都在组件里),各页从 store 读设备状态,不存在过期问题 -->
        <KeepAlive>
          <component :is="ActiveComp" />
        </KeepAlive>
      </main>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import TitleBar from "./components/TitleBar.vue";
import AsrPage from "./pages/AsrPage.vue";
import KeysPage from "./pages/KeysPage.vue";
import FirmwarePage from "./pages/FirmwarePage.vue";
import SettingsPage from "./pages/SettingsPage.vue";
import AboutPage from "./pages/AboutPage.vue";
import { ensureListening, state } from "./store";

/**
 * 导航顺序按"用到的先后":语音识别是主功能(也是默认页),按键是它的操作说明,
 * 固件烧录是设备侧的一次性动作,设置与关于是低频入口。
 * 「状态」刻意没有独立页:连接状态与电量在标题栏常显(见 TitleBar.vue)。
 */
const tabs = [
  {
    id: "asr",
    label: "语音识别",
    comp: AsrPage,
    icon: '<rect x="6" y="2.6" width="4" height="7" rx="2"/><path d="M4 8a4 4 0 0 0 8 0M8 12.6V14"/>',
  },
  {
    id: "keys",
    label: "按键",
    comp: KeysPage,
    icon: '<rect x="1.8" y="4.2" width="12.4" height="7.6" rx="1.6"/><path d="M4.4 6.6h.1M6.8 6.6h.1M9.2 6.6h.1M11.6 6.6h.1M4.6 9.4h6.8"/>',
  },
  {
    id: "firmware",
    label: "固件烧录",
    comp: FirmwarePage,
    icon: '<rect x="4.2" y="4.2" width="7.6" height="7.6" rx="1.4"/><path d="M6.6 1.8v2.4M9.4 1.8v2.4M6.6 11.8v2.4M9.4 11.8v2.4M1.8 6.6h2.4M1.8 9.4h2.4M11.8 6.6h2.4M11.8 9.4h2.4"/>',
  },
  {
    id: "settings",
    label: "设置",
    comp: SettingsPage,
    // 齿轮:8 齿的闭合轮廓 + 中心孔(半径 2.5)。
    // 不要用"圆 + 8 条放射线"那种画法 —— 在 15px 下和标题栏的太阳(主题切换)几乎一样。
    icon: '<path d="M5.93 3.01 L6.51 1.06 L9.49 1.06 L10.07 3.01 L11.85 2.04 L13.96 4.15 L12.99 5.93 L14.94 6.51 L14.94 9.49 L12.99 10.07 L13.96 11.85 L11.85 13.96 L10.07 12.99 L9.49 14.94 L6.51 14.94 L5.93 12.99 L4.15 13.96 L2.04 11.85 L3.01 10.07 L1.06 9.49 L1.06 6.51 L3.01 5.93 L2.04 4.15 L4.15 2.04 L5.93 3.01 Z"/><circle cx="8" cy="8" r="2.5"/>',
  },
  {
    id: "about",
    label: "关于",
    comp: AboutPage,
    icon: '<circle cx="8" cy="8" r="6"/><path d="M8 7.2v4M8 4.9v.1"/>',
  },
] as const;

const active = ref<(typeof tabs)[number]["id"]>("asr");
// computed 直接返回组件,避免 ref 持组件被 reactive 代理
const ActiveComp = computed(() => tabs.find((t) => t.id === active.value)?.comp ?? tabs[0].comp);

onMounted(() => {
  // 标题栏与各页都读 store;这里订阅一次,整个窗口就活了
  void ensureListening();
});
</script>
