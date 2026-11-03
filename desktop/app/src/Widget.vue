<template>
  <!-- 紧凑悬浮窗:一颗深色胶囊,状态只由**左侧元素**表达(图标 / 波形 / 旋转弧),
       右侧永远是一句纯文字,文字里不出现任何符号。
       宽度由内容决定(width:max-content),窗口尺寸跟着 syncCardWidth() 走:
       胶囊窄于窗口时,多出来的透明区会变成"看不见但吃点击"的死区。 -->
  <div
    ref="cardEl"
    class="widget-card"
    :class="`s-${view.kind}`"
    @mousedown="onMouseDown"
  >
    <span v-if="view.kind === 'off'" class="ic">
      <svg viewBox="0 0 16 16">
        <circle cx="8" cy="8" r="5.4" />
        <path d="M4.6 11.4 11.4 4.6" />
      </svg>
    </span>
    <span v-else-if="view.kind === 'idle'" class="ic">
      <svg viewBox="0 0 16 16">
        <rect x="6" y="2.6" width="4" height="7" rx="2" />
        <path d="M4 8a4 4 0 0 0 8 0M8 12.6V14" />
      </svg>
    </span>
    <span v-else-if="view.kind === 'rec'" class="wave">
      <i v-for="(h, i) in bars" :key="i" :style="{ height: `${h}%` }"></i>
    </span>
    <span v-else-if="view.kind === 'busy'" class="ic spin">
      <svg viewBox="0 0 16 16"><path d="M8 2.4a5.6 5.6 0 1 1-5.6 5.6" /></svg>
    </span>
    <span v-else class="ic">
      <svg viewBox="0 0 16 16">
        <path d="M8 2.6l5.3 9.2H2.7z" />
        <path d="M8 6.4v2.9M8 11.2v.1" />
      </svg>
    </span>
    <span class="txt">{{ view.text }}</span>
    <span v-if="view.timer" class="timer">{{ view.timer }}</span>
  </div>

  <!-- 右键菜单:打开时窗口临时撑高容纳(见 openMenu),关闭时还原 -->
  <div ref="menuEl" class="ctx-menu" @mousedown.stop>
    <div class="ctx-item" :class="{ checked: pinned }" @click="togglePin">固定位置</div>
    <div class="ctx-item" @click="closeSelf">关闭悬浮窗</div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, nextTick, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  currentMonitor,
  getCurrentWindow,
  LogicalPosition,
  LogicalSize,
} from "@tauri-apps/api/window";
import { initTheme } from "./theme";
import { ensureListening, state } from "./store";

const win = getCurrentWindow();
initTheme(); // 首帧已由 widget.html 内联脚本定色;这里补系统主题/跨窗口同步

/** Rust 建窗时经 initialization_script 注入的初始参数 */
interface WidgetInit {
  pinned: boolean;
}
const init: WidgetInit =
  (window as unknown as { __WIDGET_INIT__?: WidgetInit }).__WIDGET_INIT__ ??
  { pinned: false };

/** 尺寸口径与 Rust build_widget 的 inner_size 一致(逻辑像素) */
const CARD_H = 30;
/** 建窗时的首帧宽度(待机态);真实宽度由 syncCardWidth 实测后 setSize */
const INIT_W = 93;
/** 拖动停止多久后再读窗口位置落盘(等 OS 拖拽循环安静下来) */
const SAVE_DELAY_MS = 220;
/** 波形窗口:5 根柱子,每 120ms 推进一格(20Hz 的电平直接推会糊成一片) */
const BAR_COUNT = 5;
const BAR_MS = 120;
/** 柱子最低高度(%):静音时也要看得见一条基线 */
const BAR_MIN = 10;

const pinned = ref(init.pinned);
const menuEl = ref<HTMLElement | null>(null);
const cardEl = ref<HTMLElement | null>(null);
/** 当前胶囊宽度(逻辑像素),窗口宽度与之保持一致 */
const cardW = ref(INIT_W);

// 显示优先级:未连接 > 录音中 > 识别中/下载中 > 链路告警 > 待机
//
// **不显示识别文本**:悬浮窗只做状态指示。识别结果只键入焦点窗口,不在可能被
// 别人看到的常显窗口上留痕。
//
// **文字里不带任何符号**:状态全部交给左侧元素,文字只描述状态本身。
type ViewKind = "off" | "idle" | "rec" | "busy" | "warn";
const view = computed<{ kind: ViewKind; text: string; timer: string }>(() => {
  const s = state;
  if (!s.connected) return { kind: "off", text: "未连接", timer: "" };
  if (s.ptt) return { kind: "rec", text: "录音中", timer: fmtTimer(recSec.value) };
  if (s.asr === "recognizing") return { kind: "busy", text: "识别中", timer: "" };
  if (s.asr === "downloading") {
    return { kind: "busy", text: `下载 ${s.download_pct}%`, timer: "" };
  }
  if (s.asr.startsWith("link:")) return { kind: "warn", text: "链路不稳", timer: "" };
  return { kind: "idle", text: "按住说话", timer: "" };
});

function fmtTimer(sec: number): string {
  return `${Math.floor(sec / 60)}:${String(sec % 60).padStart(2, "0")}`;
}

// ---- 录音计时与波形 ----

const recSec = ref(0);
const bars = ref<number[]>(new Array(BAR_COUNT).fill(BAR_MIN));
/** 两次推帧之间收到的最强电平:块粒度的电平直接画会抖成噪点 */
let pendingPeak = 0;
let recTimer = 0;
let barTimer = 0;

function startRecording() {
  recSec.value = 0;
  pendingPeak = 0;
  bars.value = new Array(BAR_COUNT).fill(BAR_MIN);
  stopTimers();
  recTimer = window.setInterval(() => {
    recSec.value += 1;
  }, 1000);
  barTimer = window.setInterval(() => {
    bars.value = [
      ...bars.value.slice(1),
      BAR_MIN + Math.round(pendingPeak * (100 - BAR_MIN)),
    ];
    pendingPeak *= 0.45; // 没声音时自然落回基线,而不是留一根高的
  }, BAR_MS);
}

function stopTimers() {
  window.clearInterval(recTimer);
  window.clearInterval(barTimer);
  recTimer = 0;
  barTimer = 0;
}

function stopRecording() {
  stopTimers();
  pendingPeak = 0;
  bars.value = new Array(BAR_COUNT).fill(BAR_MIN);
}

watch(
  () => state.ptt,
  (on) => (on ? startRecording() : stopRecording()),
);

// ---- 窗口宽度跟随内容 ----

/** 量一次胶囊宽度,和窗口对齐。宽度不写死:字宽随字体/DPI/文案变(下载 42%、
   0:03 → 12:34),写死就会出现裁字或死区。 */
let widthSynced = false;
async function syncCardWidth() {
  await nextTick();
  const el = cardEl.value;
  if (!el) return;
  const w = Math.ceil(el.getBoundingClientRect().width);
  if (w <= 0 || (w === cardW.value && widthSynced)) return;
  widthSynced = true;
  cardW.value = w;
  if (menuOpen.value || menuMoving) return; // 菜单开着时尺寸归菜单逻辑管
  try {
    await win.setSize(new LogicalSize(w, CARD_H));
  } catch (e) {
    console.warn("同步悬浮窗尺寸失败：", e);
  }
}

watch(view, () => void syncCardWidth());

// ---- 数据 ----

let unLevel: (() => void) | undefined;
onMounted(async () => {
  // 设备状态走共享 store(订阅 + 快照的先后顺序那套坑见 store.ts);
  // 独立 webview 各订阅一次,互不影响。
  await ensureListening();
  // 录音电平单独走一条事件(见 device.rs push_level):它要 20Hz,而 state 是
  // 整份状态事件,20Hz 广播会让主面板跟着重渲染。
  unLevel = await listen<number>("level", (e) => {
    const v = typeof e.payload === "number" ? e.payload : 0;
    if (v > pendingPeak) pendingPeak = v;
  });
  if (state.ptt) startRecording();
  await syncCardWidth();

  document.addEventListener("mouseup", onDocMouseUp);
  document.addEventListener("contextmenu", onContextMenu);
  document.addEventListener("mousedown", onDocMouseDown);
  document.addEventListener("mouseleave", onDocMouseLeave);
  unMoved = await win.onMoved(() => {
    if (!dragging) return;
    schedulePositionSave();
  });
});
onUnmounted(() => {
  unLevel?.();
  stopTimers();
  unMoved?.();
  document.removeEventListener("mouseup", onDocMouseUp);
  document.removeEventListener("contextmenu", onContextMenu);
  document.removeEventListener("mousedown", onDocMouseDown);
  document.removeEventListener("mouseleave", onDocMouseLeave);
});

// ---- 拖动与位置记忆 ----

let dragging = false;
let unMoved: (() => void) | undefined;
/** 落盘去抖定时器:onMoved 高频触发,停稳后才读位置写配置 */
let saveTimer = 0;

async function saveCurrentPosition() {
  // 菜单会把窗口临时撑高/上移,期间的位置不是用户放置的,不落盘
  if (menuOpen.value || menuMoving) return;
  try {
    const [pos, scale] = await Promise.all([win.outerPosition(), win.scaleFactor()]);
    await invoke("save_widget_position", {
      x: Math.round(pos.x / scale),
      y: Math.round(pos.y / scale),
    });
  } catch (e) {
    console.warn("保存悬浮窗位置失败：", e);
  }
}

function schedulePositionSave() {
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => void saveCurrentPosition(), SAVE_DELAY_MS);
}

function onMouseDown(e: MouseEvent) {
  if (menuOpen.value) {
    void closeMenu(); // 点在卡片上(菜单外):只收菜单,不开始拖
    return;
  }
  if (e.button !== 0) return;
  // 双击打开主面板:第一下的 startDragging 会进 OS 拖拽循环、吞掉第二次
  // mouseup,dblclick 根本不会来——必须在 mousedown 里按 detail 区分
  if (e.detail >= 2) {
    void invoke("show_panel").catch((err) => console.warn("打开主面板失败：", err));
    return;
  }
  if (pinned.value) return;
  dragging = true;
  // startDragging 的 promise 要到 OS 拖拽循环结束才落定,天然就是「拖完了」信号
  // (Windows 上 mouseup 会被拖拽循环吞掉,不能当收尾):finally 里复位并补落盘,
  // onMoved 在拖动过程中持续触发,停稳后的最后一次就是最终位置
  win
    .startDragging()
    .catch((e) => console.warn("启动拖动失败：", e))
    .finally(() => {
      dragging = false;
      schedulePositionSave();
    });
}


/** 兜底路径(主路径是 startDragging 的 .finally):等 OS 拖拽循环安静下来再读位置 */
function onDocMouseUp() {
  if (!dragging) return;
  dragging = false;
  schedulePositionSave();
}

// ---- 右键菜单(固定位置 / 深浅切换 / 关闭) ----

const menuOpen = ref(false);
/** 菜单挪窗期间(上移→还原)为 true:这期间的位置不是用户放置的,不许落盘 */
let menuMoving = false;
/** 菜单把窗口上移过时记住的原 y(逻辑像素),关闭菜单时还原;没动过为 null */
let menuSavedY: number | null = null;

function onContextMenu(e: MouseEvent) {
  // 拦掉 WebView 自带的右键菜单,换我们自己的目录(菜单开着时不重开)
  e.preventDefault();
  if (!menuOpen.value) void openMenu(e.clientX, e.clientY);
}

function onDocMouseDown(e: MouseEvent) {
  // 点菜单之外(窗口内的任何地方)收起;点菜单项本身不收(由 click 处理器收)
  if (menuOpen.value && menuEl.value && !menuEl.value.contains(e.target as Node)) {
    void closeMenu();
  }
}

// 光标离开窗口也收起:窗口外点击我们收不到(focusable(false) 无焦点事件),
// 菜单会一直悬在别的应用上面
function onDocMouseLeave() {
  if (menuOpen.value) void closeMenu();
}

async function openMenu(cx: number, cy: number) {
  if (menuOpen.value || !menuEl.value) return;
  menuOpen.value = true;
  menuMoving = true; // 从这里到 closeMenu 还原完,窗口的移动都不是用户放置
  const el = menuEl.value;
  el.style.display = "block";
  const mw = el.offsetWidth;
  const mh = el.offsetHeight;
  // 菜单比胶囊宽(min-width:150px),窗口必须临时加宽,否则菜单右侧会被窗口
  // 矩形直接裁掉(透明窗口画不到窗口外)。
  const needW = Math.max(cardW.value, mw + 8);
  // 菜单顶边贴光标(Windows 惯例),横向夹在窗口内
  const mx = Math.min(Math.max(cx, 4), Math.max(4, needW - mw - 4));
  let my = cy + 2;
  let upShift = 0;
  let needH = Math.round(my + mh + 4);
  try {
    const mon = await currentMonitor();
    if (mon) {
      const scale = mon.scaleFactor;
      const pos = await win.outerPosition();
      const winTop = pos.y / scale;
      const cursorY = winTop + cy;
      const screenBottom = (mon.position.y + mon.size.height) / scale;
      if (cursorY + mh + 6 > screenBottom) {
        // 菜单朝下会伸出屏幕底 → 朝上翻:底边贴光标上沿,窗口顶边抬到菜单上方
        const wantTop = cursorY - mh - 6;
        upShift = Math.max(0, winTop - wantTop);
        // 别把窗口抬出屏幕顶
        upShift = Math.min(upShift, winTop - mon.position.y / scale);
        // 窗口上移 upShift 后,同一屏幕位置在窗口里的相对 y 变小
        my = Math.max(4, cursorY - 2 - mh - (winTop - upShift));
        needH = Math.round(my + mh + 4);
      }
    }
    if (upShift > 0 || needH > CARD_H || needW !== cardW.value) {
      if (upShift > 0) {
        const pos = await win.outerPosition();
        const scale = await win.scaleFactor();
        // 只在还没记过时写入原 y:任何路径都不许把已上移的 y 当成原位置
        if (menuSavedY === null) menuSavedY = pos.y / scale;
        await win.setPosition(new LogicalPosition(pos.x / scale, menuSavedY - upShift));
      }
      if (needH > CARD_H || needW !== cardW.value) {
        await win.setSize(new LogicalSize(needW, Math.max(needH, CARD_H)));
      }
    }
  } catch {
    /* 窗口几何操作失败就按原尺寸显示,菜单可能被裁一点,可接受 */
  }
  el.style.left = `${mx}px`;
  el.style.top = `${my}px`;
}

async function closeMenu() {
  if (!menuOpen.value && !menuMoving) return;
  menuOpen.value = false;
  if (menuEl.value) menuEl.value.style.display = "none";
  const savedY = menuSavedY;
  menuSavedY = null;
  try {
    if (savedY !== null) {
      const pos = await win.outerPosition();
      const scale = await win.scaleFactor();
      await win.setPosition(new LogicalPosition(pos.x / scale, savedY));
    }
    await win.setSize(new LogicalSize(cardW.value, CARD_H));
  } catch {
    /* 窗口可能已被销毁 */
  }
  menuMoving = false;
}

async function togglePin() {
  pinned.value = !pinned.value;
  try {
    await invoke("set_widget_pinned", { pinned: pinned.value });
  } catch (e) {
    console.warn("保存固定状态失败：", e);
  }
  await closeMenu();
}

async function closeSelf() {
  await closeMenu();
  try {
    await invoke("close_widget");
  } catch (e) {
    console.warn("关闭悬浮窗失败：", e);
  }
}
</script>
