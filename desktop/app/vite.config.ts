import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// 多入口:主面板(index)与悬浮窗(widget)各自独立页面
export default defineConfig({
  plugins: [vue()],
  // 相对路径产物:dist 直接用 file:// 打开也能跑(改 UI 时截图核对用得上),
  // Tauri 走自定义协议时相对/绝对都能解析,不影响打包。
  base: "./",
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    outDir: "dist",
    rollupOptions: {
      input: {
        index: new URL("./index.html", import.meta.url).pathname,
        widget: new URL("./widget.html", import.meta.url).pathname,
      },
    },
  },
});
