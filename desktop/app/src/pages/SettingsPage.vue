<template>
  <section>
    <div class="page-head">
      <h2 class="page-title">设置</h2>
      <span class="page-sub">改动即时落盘（%APPDATA%\vibemic\config.json）</span>
    </div>

    <div class="card">
      <h3 class="card-title">启动</h3>
      <div class="setting-row">
        <div class="setting-text">
          <span class="setting-label">开机自启动</span>
          <span class="hint">登录 Windows 后自动运行；只启托盘与悬浮窗，不弹本页</span>
        </div>
        <!-- 开关的选中态由系统启动项的真实状态决定(isEnabled 回读),不是本地一份副本 -->
        <label class="switch" :class="{ busy: autoBusy }" title="开机自启动">
          <input
            type="checkbox"
            :checked="autostart"
            :disabled="autoBusy"
            @change="toggleAutostart(($event.target as HTMLInputElement).checked)"
          />
          <span class="track"></span>
        </label>
      </div>
      <div v-if="autoError" class="warn-box mt-8">
        <div class="row between">
          <span class="grow">{{ autoError }}</span>
          <button class="btn ghost" @click="loadAutostart">重试</button>
        </div>
      </div>
    </div>

    <div class="card">
      <h3 class="card-title">外观</h3>
      <div class="setting-row">
        <span class="setting-label">主题</span>
        <div class="radio-group" role="radiogroup" aria-label="主题">
          <label class="radio-btn">
            <input type="radio" name="theme" value="system" :checked="theme.pref === 'system'" @change="setPref('system')" />
            <span>跟随系统</span>
          </label>
          <label class="radio-btn">
            <input type="radio" name="theme" value="dark" :checked="theme.pref === 'dark'" @change="setPref('dark')" />
            <span>深色</span>
          </label>
          <label class="radio-btn">
            <input type="radio" name="theme" value="light" :checked="theme.pref === 'light'" @change="setPref('light')" />
            <span>浅色</span>
          </label>
        </div>
      </div>
      <!-- 这里刻意**不放**「悬浮窗」开关:悬浮窗自己还有右键菜单能关掉它,
           前端拿不到"当前是否显示"的通知,开关会和实际状态对不上(显示"开"而窗口已关)。
           与其放一个会说谎的开关,不如写清楚怎么找回来 —— 托盘菜单随时能切。 -->
      <div class="setting-row">
        <div class="setting-text">
          <span class="setting-label">悬浮窗</span>
          <span class="hint">关掉后从托盘右键菜单「切换悬浮窗」唤回；双击悬浮窗打开本页</span>
        </div>
      </div>
    </div>

    <div class="card">
      <h3 class="card-title">本机信息</h3>
      <dl class="kv-row">
        <div><dt>蓝牙名称</dt><dd>AI Passport Vibe Mic</dd></div>
        <div><dt>链接方式</dt><dd>BLE · WinRT</dd></div>
        <div><dt>配置文件</dt><dd>{{ paths.config_file || "（读取中…）" }}</dd></div>
      </dl>
      <dl class="kv-row mt-10">
        <div><dt>模型目录</dt><dd>{{ paths.model_dir || "（读取中…）" }}</dd></div>
        <div><dt>日志目录</dt><dd>{{ paths.log_dir || "（读取中…）" }}</dd></div>
        <div><dt>许可</dt><dd>MIT</dd></div>
      </dl>
      <div class="row tight mt-10">
        <button class="btn ghost" @click="openDir('config')">打开配置目录</button>
        <button class="btn ghost" @click="openDir('log')">打开日志目录</button>
        <button class="btn ghost" @click="copyEnv">{{ copied ? "已复制 ✓" : "复制环境信息" }}</button>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import type { AppPaths } from "../api";
import { state } from "../store";
import {
  onThemeChange,
  setThemePref,
  themePref,
  type ThemePref,
} from "../theme";

const paths = reactive<AppPaths>({ config_dir: "", config_file: "", model_dir: "", log_dir: "" });

// ---- 主题:偏好存 localStorage(见 theme.ts),这里只做三态读写 ----
const theme = reactive<{ pref: ThemePref }>({ pref: themePref() });
let unTheme: (() => void) | undefined;

function setPref(p: ThemePref) {
  setThemePref(p);
}

// ---- 开机自启动:真实来源是系统的启动项,每次都以 isEnabled() 回读为准 ----
const autostart = ref(false);
const autoBusy = ref(false);
const autoError = ref("");

async function loadAutostart() {
  autoError.value = "";
  try {
    autostart.value = await isEnabled();
  } catch (e) {
    autoError.value = `读取开机自启动状态失败：${e}`;
  }
}

/**
 * 切换自启动。失败也要回读:只有系统里的真实状态才作数,
 * 否则开关会显示成"已开",而实际启动项没写进去。
 */
async function toggleAutostart(on: boolean) {
  autoBusy.value = true;
  autoError.value = "";
  try {
    if (on) await enable();
    else await disable();
  } catch (e) {
    autoError.value = `开机自启动设置失败：${e}`;
  } finally {
    await loadAutostart();
    autoBusy.value = false;
  }
}

// ---- 本机信息 ----
const copied = ref(false);
let copyTimer = 0;

onMounted(async () => {
  unTheme = onThemeChange((s) => {
    theme.pref = s.pref;
  });
  try {
    Object.assign(paths, await invoke<AppPaths>("app_paths"));
  } catch (e) {
    console.warn("读取应用路径失败：", e);
  }
  await loadAutostart();
});

onUnmounted(() => {
  unTheme?.();
  window.clearTimeout(copyTimer);
});

function openDir(which: "config" | "log" | "models") {
  void invoke("open_app_dir", { which }).catch((e) => console.warn("打开目录失败：", e));
}

/** 复制环境信息(排障反馈用):版本 + 路径 + 设备状态,一次拷全 */
async function copyEnv() {
  let version = "";
  try {
    version = await getVersion();
  } catch {
    /* 浏览器里预览时没有 Tauri 环境,忽略 */
  }
  const lines = [
    `VibeMic v${version || "?"}`,
    `设备:${state.connected ? "已连接" : "未连接"}` +
      (state.connected ? ` · 电量 ${state.battery ?? "?"}% · 固件 0x${state.fw_ver.toString(16)}` : ""),
    `配置文件:${paths.config_file}`,
    `模型目录:${paths.model_dir}`,
  ].join("\n");
  try {
    await navigator.clipboard.writeText(lines);
    copied.value = true;
    copyTimer = window.setTimeout(() => (copied.value = false), 1600);
  } catch (e) {
    console.warn("复制失败：", e);
  }
}
</script>
