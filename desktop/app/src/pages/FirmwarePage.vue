<template>
  <section>
    <div class="page-head">
      <h2 class="page-title">固件烧录</h2>
      <!-- 这一页用户要判断的是「要不要烧」,所以页头给当前固件版本;
           未连接时写「未知」占位,免得那一行突然短一截 -->
      <span class="page-sub">
        当前固件版本 <b class="fw-now">{{ state.connected ? "v0x" + fwHex(state.fw_ver) : "未知" }}</b>
      </span>
    </div>

    <div class="card">
      <div class="setting-row">
        <div class="setting-text">
          <span class="setting-label">镜像文件</span>
          <span class="hint mono path-text">{{ firmware.path || "（未选择）" }}</span>
        </div>
        <button class="btn ghost" @click="pickFirmware">选择 .bin…</button>
      </div>
      <div class="setting-row">
        <div class="setting-text">
          <span class="setting-label">镜像校验</span>
          <span class="hint">{{ firmwareText }}</span>
        </div>
        <!-- 只有"文件在且没有错误"才敢说可烧录:info 还没回来时 firmware.error 是 null,
             那时候显示「可烧录」等于没校验就放行 -->
        <span v-if="firmware.error === null && firmware.exists" class="ok-box">可烧录</span>
        <span v-else-if="firmware.error !== null" class="warn-box">不可烧录</span>
      </div>
      <div class="setting-row">
        <span class="setting-label">串口</span>
        <div class="row tight">
          <select v-model="flashPort" class="grow" aria-label="串口">
            <option value="">选择串口…</option>
            <option v-for="p in ports" :key="p.name" :value="p.name">
              {{ p.label }}{{ p.usb_jtag ? " · USB-JTAG" : "" }}
            </option>
          </select>
          <button class="btn ghost" @click="refreshPorts">刷新</button>
        </div>
      </div>
      <div class="setting-row">
        <span class="setting-label">烧录选项</span>
        <div class="row tight">
          <label class="check">
            <input type="checkbox" v-model="verifyDraft" @change="saveFirmwareConfig" />
            <span>写后回读校验</span>
          </label>
          <label class="check">
            <input type="checkbox" v-model="eraseAll" />
            <span>先整片擦除</span>
          </label>
        </div>
      </div>
    </div>

    <!-- 烧录:进度 + 日志(同一块,平时显示按钮)。跑在 Rust 侧的独立线程上,
         进度经 flash-progress / flash-log / flash-done 三个事件回吐 -->
    <div class="card">
      <div class="between">
        <span class="hint grow">
          {{ flashing ? "烧录中…（写入 0x0 起，约 40 秒；期间不要拔线）" : "写入时请保持设备连接，不要拔线" }}
        </span>
        <button class="btn primary" :disabled="!canFlash" @click="startFlash">
          {{ flashing ? "烧录中…" : "写入设备" }}
        </button>
      </div>
      <div v-if="flashing || flashPct > 0" class="progress">
        <div class="bar" :style="{ width: flashPct + '%' }"></div>
      </div>
      <div v-if="flashLog.length" ref="logEl" class="flash-log">
        <div v-for="(line, i) in flashLog" :key="i">{{ line }}</div>
      </div>
    </div>

    <!-- 写入前就要看到的警示:红色提示条 + 该做的动作(打开蓝牙设置)。
         与 .warn-box(错误态)分开 —— 这是正常流程里的必知事项,不是报错 -->
    <div class="notice-danger mt-10">
      <span class="grow">烧录会清空设备数据：需要删除蓝牙设备并重新添加</span>
      <button class="btn" @click="openBluetoothSettings">打开蓝牙设置</button>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { FirmwareInfo, PortEntry } from "../api";
import { fwHex, state } from "../store";

const firmware = ref<FirmwareInfo>({
  path: "",
  exe_dir: "",
  default_path: null,
  exists: false,
  size: 0,
  sha256: "",
  magic_ok: false,
  error: null,
});
const ports = ref<PortEntry[]>([]);
const flashPort = ref("");
const eraseAll = ref(false);
const flashing = ref(false);
const flashPct = ref(0);
const flashLog = ref<string[]>([]);
const logEl = ref<HTMLElement | null>(null);
/** 配置草稿:只在挂载时与保存后与后端同步,避免广播把用户正在输入的内容冲掉。 */
const verifyDraft = ref(true);
/** 手选过镜像的路径:非空即"用户挑的",后端会严格只用它(文件没了就报错,不悄悄换)。 */
const pickedPath = ref("");

const firmwareText = computed(() => {
  const f = firmware.value;
  if (!f.exists) return f.error ?? "未找到镜像";
  const kb = (f.size / 1024).toFixed(1);
  if (f.error) return `${kb} KB · ${f.error}`;
  return `${kb} KB · sha256 ${f.sha256.slice(0, 12)}… · 校验通过`;
});

const canFlash = computed(
  () => !flashing.value && !!flashPort.value && firmware.value.error === null,
);

async function refreshPorts() {
  try {
    ports.value = await invoke<PortEntry[]>("flash_ports");
  } catch (err) {
    console.warn("枚举串口失败：", err);
    return;
  }
  if (!flashPort.value && ports.value.length) {
    // 默认选 USB-JTAG 那个(设备自带下载模式),否则第一个。
    flashPort.value = (ports.value.find((p) => p.usb_jtag) ?? ports.value[0]).name;
  }
}

async function refreshFirmware() {
  try {
    firmware.value = await invoke<FirmwareInfo>("flash_info");
  } catch (err) {
    console.warn("读取固件信息失败：", err);
  }
}

async function saveFirmwareConfig() {
  try {
    await invoke("set_firmware_config", {
      path: pickedPath.value,
      baud: state.firmware.baud,
      verify: verifyDraft.value,
    });
    await refreshFirmware();
  } catch (err) {
    console.warn("保存烧录设置失败：", err);
  }
}

/**
 * 手选固件:.bin 过滤器。只把**真实存在**的镜像当默认值(后端给的 default_path):
 * 软件目录里没找到时它是 null,于是干脆不传 defaultPath,免得把不存在的文件名
 * 预填进"文件名"框。取消则什么都不改。
 *
 * 镜像只有两个来源:用户手选(存进 config.json 的 firmware.path),或者留空由后端
 * 在软件所在目录里找。换镜像就再选一次。
 */
async function pickFirmware() {
  try {
    // 现算一遍再开对话框:镜像是用户刚从别处拷进软件目录的也认。
    await refreshFirmware();
    const picked = await open({
      multiple: false,
      directory: false,
      title: "选择固件镜像",
      defaultPath: firmware.value.default_path ?? undefined,
      filters: [{ name: "固件镜像", extensions: ["bin"] }],
    });
    if (typeof picked !== "string") return; // 用户取消
    pickedPath.value = picked;
    await saveFirmwareConfig();
  } catch (err) {
    console.warn("选择固件失败：", err);
  }
}

function openBluetoothSettings() {
  void invoke("open_bluetooth_settings").catch((e) => console.warn("打开蓝牙设置失败：", e));
}

async function startFlash() {
  flashing.value = true;
  flashPct.value = 0;
  flashLog.value = [];
  try {
    await invoke("flash_start", { port: flashPort.value, erase: eraseAll.value });
  } catch (err) {
    flashing.value = false;
    flashLog.value.push(String(err));
  }
}

// 日志自动贴底:烧录中不断追加,不贴底就看不见最新一行
watch(
  () => flashLog.value.length,
  async () => {
    await nextTick();
    if (logEl.value) logEl.value.scrollTop = logEl.value.scrollHeight;
  },
);

let unFlash: Array<() => void> = [];

onMounted(async () => {
  verifyDraft.value = state.firmware.verify;
  pickedPath.value = state.firmware.path;
  await refreshPorts();
  await refreshFirmware();

  unFlash.push(
    await listen<{ done: number; total: number }>("flash-progress", (e) => {
      const { done, total } = e.payload;
      flashPct.value = total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
    }),
    await listen<string>("flash-log", (e) => {
      flashLog.value.push(e.payload);
    }),
    await listen<{ ok: boolean; error: string | null }>("flash-done", (e) => {
      flashing.value = false;
      flashPct.value = e.payload.ok ? 100 : flashPct.value;
      flashLog.value.push(
        e.payload.ok ? "烧录完成" : `烧录失败：${e.payload.error ?? "未知错误"}`,
      );
    }),
  );
});

onUnmounted(() => {
  for (const off of unFlash) off();
  unFlash = [];
});
</script>
