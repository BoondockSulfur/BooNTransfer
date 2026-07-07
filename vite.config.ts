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
  },
  build: {
    target: "es2021",
    outDir: "dist",
    sourcemap: true,
  },
});
