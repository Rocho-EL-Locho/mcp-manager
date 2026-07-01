import { defineConfig } from "vite";

// Vite dient nur als Dev-Server und Bundler für das statische Frontend.
// Tauri lädt im Dev-Modus http://localhost:1420, im Build die Dateien aus /dist.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "es2021",
    outDir: "dist",
    emptyOutDir: true,
  },
});
