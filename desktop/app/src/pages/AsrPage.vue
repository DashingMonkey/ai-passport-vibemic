<template>
  <section>
    <div class="page-head">
      <h2 class="page-title">语音识别</h2>
      <span class="page-sub">本地 SenseVoice 模型 · 离线运行，音频不出本机</span>
    </div>

    <div class="card">
      <!-- 分支必须把 asr 的每个取值都显式列出来:"没有下载模型"绝不能当兜底,
           否则识别中的 recognizing 会掉进去,松开 OK 时闪一下"未下载模型"。
           未知取值原样显示,便于发现漏掉的状态。 -->
      <div v-if="state.asr === 'ready'" class="ok-box">✓ 模型已就绪（约 160MB，随软件目录持久保存）</div>
      <div v-else-if="state.asr === 'recognizing'" class="hint">
        识别中…（松开 OK 后出字，通常 100ms 内）
      </div>
      <div v-else-if="state.asr === 'downloading'">
        <div class="hint">正在下载模型（约 160MB，来自国内镜像）… {{ state.download_pct }}%</div>
        <div class="progress"><div class="bar" :style="{ width: state.download_pct + '%' }"></div></div>
      </div>
      <div v-else-if="state.asr.startsWith('link:')" class="warn-box">⚠ {{ state.asr.slice(5) }}</div>
      <div v-else-if="state.asr.startsWith('error:')" class="row between">
        <span class="warn-box grow">✗ {{ state.asr.slice(6) }}</span>
        <button class="btn" @click="redownload">重新下载模型</button>
      </div>
      <div v-else-if="state.asr === 'missing'" class="row between">
        <span class="hint grow">尚未下载语音模型 —— 按住 OK 说话前需要先下载（一次即可）。</span>
        <button class="btn primary" @click="download">下载模型</button>
      </div>
      <div v-else class="warn-box">未知状态：{{ state.asr }}</div>

      <div class="setting-row mt-8">
        <span class="setting-label">模型位置</span>
        <div class="row tight">
          <span class="hint mono path-text">{{ paths.model_dir || "（读取中…）" }}</span>
          <button class="btn ghost" @click="openDir('models')">打开目录</button>
        </div>
      </div>
      <div class="setting-row">
        <span class="setting-label">模型自检</span>
        <button class="btn ghost" @click="redownload">重新下载</button>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { onMounted, reactive } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { AppPaths } from "../api";
import { state } from "../store";

const paths = reactive<AppPaths>({ config_dir: "", config_file: "", model_dir: "", log_dir: "" });

onMounted(async () => {
  try {
    Object.assign(paths, await invoke<AppPaths>("app_paths"));
  } catch (e) {
    console.warn("读取应用路径失败：", e);
  }
});

function openDir(which: "config" | "log" | "models") {
  void invoke("open_app_dir", { which }).catch((e) => console.warn("打开目录失败：", e));
}

function download() {
  void invoke("download_model").catch((e) => console.warn("下载启动失败：", e));
}

/** 引擎加载失败后的出口:先删掉本地模型再下,否则"文件在"会让下载直接返回成功。 */
function redownload() {
  void invoke("redownload_model").catch((e) => console.warn("重新下载失败：", e));
}
</script>
