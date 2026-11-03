/**
 * 主面板与悬浮窗共享的设备状态。
 *
 * 为什么要单独一份 store,而不是各页自己 listen:
 * 标题栏的连接状态/电量是**常显**的(不在任何一页里),侧边栏与各页也都读它。
 * 如果每页在 onMounted 里各自取一次快照,页面被 <KeepAlive> 保活后切回来不会重跑
 * onMounted,就会一直显示过期状态。所以订阅收敛到这里一份,谁需要谁 import。
 *
 * 订阅顺序有个坑(原 App.vue 的注释保留在这里):**先订阅,再取快照**,
 * 并且只在"还没收到过任何事件"时才应用快照 —— 否则 IPC 往返期间到达的新事件
 * 会被稍后返回的旧快照覆盖(表现为:松开 OK 后仍显示"录音中")。
 */

import { reactive } from "vue";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import type { AppStatePayload } from "./api";

/** 设备状态(与 Rust 侧 state.rs 的 AppState 同构,字段见 api.ts) */
export const state = reactive<AppStatePayload>({
  connected: false,
  battery: null,
  fw_ver: 0,
  ptt: false,
  asr: "missing",
  download_pct: 0,
  last_text: null,
  send_key: "enter",
  firmware: { path: "", baud: 460800, verify: true },
  pairing_needed: false,
});

/** 是否已经接过一次状态(取过快照或收到过事件)。页面用它决定"加载中"怎么显示。 */
export const ready = reactive({ value: false });

let started: Promise<void> | null = null;

/** 订阅状态事件并取一次快照。重复调用只生效一次(主面板与悬浮窗各自调用一次)。 */
export function ensureListening(): Promise<void> {
  if (!started) started = start();
  return started;
}

async function start(): Promise<void> {
  let sawEvent = false;
  await listen<AppStatePayload>("state", (e) => {
    sawEvent = true;
    ready.value = true;
    Object.assign(state, e.payload);
  });
  try {
    const snapshot = await invoke<AppStatePayload>("get_state");
    // 快照可能比刚到达的事件更旧:只有还没收到过事件时才应用它
    if (!sawEvent) Object.assign(state, snapshot);
  } catch (e) {
    console.warn("读取设备状态失败：", e);
  }
  ready.value = true;
}

/** 固件版本按 4 位十六进制显示(0x0003),与设备日志/vmctl 的口径一致 */
export function fwHex(ver: number): string {
  return ver.toString(16).toUpperCase().padStart(4, "0");
}
