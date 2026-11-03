/**
 * 深浅主题。
 *
 * 偏好存 localStorage(三态:system/dark/light),不放 config.json:主面板与
 * 悬浮窗是两个独立 webview,localStorage 同源共享、且能在首帧之前同步读到;
 * 放配置文件就得等 IPC 回来才知道选的是哪个,每次启动都会先闪一下另一种主题
 * ——悬浮窗是常驻窗口,闪一下尤其明显。
 *
 * 生效方式:给 <html> 打 data-theme="dark|light"。样式里只有两块
 * (:root 深色 / :root[data-theme="light"] 浅色),媒体查询不参与配色——
 * 「跟随系统」由这里的 matchMedia 解析成具体值,否则「系统浅色 + 用户选深色」
 * 时媒体查询和显式选择会互相打架。
 *
 * 首帧定色另有一份等价逻辑,内联在 index.html / widget.html 的 head 里:它必须
 * 在样式与模块之前同步执行,所以不能 import 这个文件。
 */

export type ThemePref = "system" | "dark" | "light";
export type Theme = "dark" | "light";

/** localStorage 键:改了要同步改**三处**——index.html / widget.html 内联脚本 + 本文件,
 * 漏一处就会出现「首帧一个色、落地另一个色」 */
const KEY = "vibemic.theme";

const lightMedia = window.matchMedia("(prefers-color-scheme: light)");

function readPref(): ThemePref {
  // localStorage 读取要防御:隐私模式/被禁用/配额异常都会抛错,而这段跑在模块
  // 加载期,抛出去就是整个页面白屏——异常一律回退「跟随系统」
  try {
    const v = localStorage.getItem(KEY);
    return v === "dark" || v === "light" ? v : "system";
  } catch {
    return "system";
  }
}

let pref: ThemePref = readPref();
let watching = false;

export function themePref(): ThemePref {
  return pref;
}

/** 偏好 → 实际该用的主题(`system` 时问系统) */
export function resolvedTheme(p: ThemePref = pref): Theme {
  if (p === "system") return lightMedia.matches ? "light" : "dark";
  return p;
}

/** 主题快照:实际显示的主题 + 用户偏好(切换按钮/菜单文案用) */
export type ThemeState = { theme: Theme; pref: ThemePref };

const listeners = new Set<(s: ThemeState) => void>();

/** 订阅主题变化,返回解绑函数。本窗口切换、系统主题变化(system 偏好时)与
 * 其它窗口改主题(storage 事件)都走 apply(),在这里统一通知。 */
export function onThemeChange(cb: (s: ThemeState) => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

function notify() {
  const snapshot: ThemeState = { theme: resolvedTheme(), pref };
  for (const cb of listeners) cb(snapshot);
}

function apply() {
  const t = resolvedTheme();
  document.documentElement.dataset.theme = t;
  document.documentElement.style.colorScheme = t; // 原生控件(滚动条/下拉)跟随
  notify();
}

export function setThemePref(next: ThemePref) {
  pref = next;
  // 先 apply() 让本窗口当场换色,再写存储:写入失败只丢跨窗口同步,
  // 不能让切主题「点了没反应」
  apply();
  try {
    // 删掉键而不是存 "system":内联脚本只认 dark/light,缺键即跟随系统
    if (next === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, next);
  } catch {
    /* 保存失败:本次切换仍生效,重启后回到跟随系统 */
  }
}

/** 深浅切换按钮/菜单用:按**当前实际显示**的主题取反,返回切换后的主题 */
export function toggleTheme(): Theme {
  const next: Theme = resolvedTheme() === "dark" ? "light" : "dark";
  setThemePref(next);
  return next;
}

/** 启动时调用一次:应用当前偏好,并挂上系统主题变化与跨窗口(storage)同步。 */
export function initTheme() {
  apply();
  if (watching) return;
  watching = true;
  lightMedia.addEventListener("change", () => {
    if (pref === "system") apply();
  });
  window.addEventListener("storage", (e) => {
    // key 为 null 表示整个 storage 被清空
    if (e.key !== null && e.key !== KEY) return;
    pref = readPref();
    apply();
  });
}
