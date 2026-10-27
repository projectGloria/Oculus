import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri dev server: fixed port (tauri.conf devUrl), and don't watch the Rust side — cargo
// already rebuilds it, a Vite reload on every .rs save is just noise.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true },
});
