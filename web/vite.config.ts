import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// The Worker (wrangler dev, port 8787) serves /api and /v1/spr in development.
export default defineConfig({
  plugins: [react()],
  server: { proxy: { "/api": "http://127.0.0.1:8787", "/v1/spr": "http://127.0.0.1:8787" } },
  build: { target: "es2022" },
  test: { include: ["src/**/*.test.ts"] },
});
