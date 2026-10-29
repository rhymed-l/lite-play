import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    target: "es2021",
    sourcemap: false,
    // Two pages: the player and the floating desktop subtitle window.
    rollupOptions: {
      // Relative to the project root, which is where vite runs from.
      input: {
        main: "index.html",
        desktop: "desktop.html",
      },
    },
  },
});
