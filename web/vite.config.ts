import path from "node:path";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv } from "vite";

export default defineConfig(({ mode }) => {
  const environment = loadEnv(mode, process.cwd(), "");
  const apiTarget = environment.VITE_ASTERISM_API_PROXY_TARGET || "http://127.0.0.1:8068";

  return {
    plugins: [react(), tailwindcss()],
    resolve: {
      alias: {
        "@": path.resolve(import.meta.dirname, "./src"),
      },
    },
    server: {
      proxy: {
        "/api": apiTarget,
        "/health": apiTarget,
      },
    },
  };
});
