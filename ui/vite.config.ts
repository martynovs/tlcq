import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// `pnpm dev` proxies the API to a runner: $TLCQ_API, else the one on pve.
const api = process.env.TLCQ_API ?? "https://tlcq.pve.local";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: { proxy: { "/api": { target: api, changeOrigin: true, secure: false } } },
});
