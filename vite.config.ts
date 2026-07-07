import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed port and no automatic browser opening.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    // 1420 is Tauri's conventional dev port — avoids clashing with other local
    // Vite stacks (e.g. containerized projects on 5173).
    port: 1420,
    strictPort: true,
    host: "localhost",
    watch: {
      // Never watch Rust build output or Flatpak build trees — the latter
      // unpacks a root fs with symlink loops (/var/run/udev/...) that crash
      // the file watcher (ELOOP).
      ignored: ["**/src-tauri/target/**", "**/packaging/flatpak/build-dir/**"],
    },
  },
  build: {
    target: "es2021",
    outDir: "dist",
    sourcemap: true,
  },
});
