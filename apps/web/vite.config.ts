import { defineConfig } from "vite"
import vue from "@vitejs/plugin-vue"
import { fileURLToPath, URL } from "node:url"

const apiTarget = "http://127.0.0.1:8787"

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    // Bind IPv4 loopback explicitly: Node may resolve `localhost` to ::1 only,
    // which makes http://127.0.0.1:5173 refuse connections.
    host: "127.0.0.1",
    port: 5173,
    proxy: {
      "/api": { target: apiTarget, changeOrigin: true },
      "/openai": { target: apiTarget, changeOrigin: true },
      "/anthropic": { target: apiTarget, changeOrigin: true },
      "/health": { target: apiTarget, changeOrigin: true },
    },
  },
  preview: {
    host: "127.0.0.1",
    port: 4173,
    proxy: {
      "/api": { target: apiTarget, changeOrigin: true },
      "/openai": { target: apiTarget, changeOrigin: true },
      "/anthropic": { target: apiTarget, changeOrigin: true },
      "/health": { target: apiTarget, changeOrigin: true },
    },
  },
})
