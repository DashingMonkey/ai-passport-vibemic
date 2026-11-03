<template>
  <section>
    <div class="page-head">
      <h2 class="page-title">按键</h2>
      <span class="page-sub">设备三个键的固定映射；仅「确认键」可改</span>
    </div>

    <div class="card">
      <div class="key-row"><span class="kbd">OK 按住</span><span>对着设备说话，松开后文字自动输入到当前光标</span></div>
      <div class="key-row"><span class="kbd">OK 双击</span><span>删除一个字符；第二下按住不放＝连续删，松手即停</span></div>
      <div class="key-row"><span class="kbd">UP 单击</span><span>确认 / 发送（发送键见下）</span></div>
      <div class="key-row"><span class="kbd">DOWN 单击</span><span>取消（Esc）</span></div>
      <div class="key-row"><span class="kbd">DOWN 双击</span><span>停止（连发两记 Esc，支持「双击 Esc 中断」的软件）</span></div>
      <div class="key-row"><span class="kbd">DOWN 长按 10 秒</span><span>重置蓝牙配对（设备端清空配对并重新广播，PC 软件离线也能用；之后要在系统蓝牙里删除本设备再重新配对）</span></div>
    </div>

    <div class="card">
      <h3 class="card-title">确认键（UP 单击发送）</h3>
      <div class="row between">
        <span class="hint grow">绝大多数软件发送是 Enter；少数 IDE 对话框为 Ctrl+Enter</span>
        <!-- 按钮式单选(分段控件):语义由原生 radio 承担,外观做成并排按钮 -->
        <div class="radio-group" role="radiogroup" aria-label="确认键">
          <label class="radio-btn">
            <input
              type="radio"
              name="send-key"
              value="enter"
              :checked="state.send_key === 'enter'"
              @change="onSendKey('enter')"
            />
            <span>Enter</span>
          </label>
          <label class="radio-btn">
            <input
              type="radio"
              name="send-key"
              value="ctrl_enter"
              :checked="state.send_key === 'ctrl_enter'"
              @change="onSendKey('ctrl_enter')"
            />
            <span>Ctrl+Enter</span>
          </label>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import type { SendKey } from "../api";
import { state } from "../store";

async function onSendKey(v: SendKey) {
  const previous = state.send_key;
  state.send_key = v;
  try {
    await invoke("set_send_key", { sendKey: v });
  } catch (err) {
    // 落盘失败就回滚单选值:不能让界面显示一个重启后会变回去的设置。
    state.send_key = previous;
    console.warn("保存确认键失败：", err);
  }
}
</script>
