import { defineConfig } from "vite";
import preact from "@preact/preset-vite";

// Tauri loads the dev server in debug builds (tauri.conf.json `devUrl`)
// and the built `dist/` in release builds.
export default defineConfig({
  plugins: [preact()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "safari16", outDir: "dist", emptyOutDir: true },
});
