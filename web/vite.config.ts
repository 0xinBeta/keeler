import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// The Worker (wrangler dev, port 8787, see scripts/dev.sh) serves /api and /v1/spr in development.
// It refuses cross-origin requests, and the dev server's origin is not its own, so the proxy drops Origin.
const worker = {
  target: "http://127.0.0.1:8787",
  configure: (proxy: { on: (event: "proxyReq", fn: (req: { removeHeader: (name: string) => void }) => void) => void }) =>
    proxy.on("proxyReq", (req) => req.removeHeader("origin")),
};

export default defineConfig({
  plugins: [react()],
  server: { proxy: { "/api": worker, "/v1/spr": worker } },
  // The Worker's CSP allows fonts from 'self' only, so small font files must not be inlined as data: URIs.
  build: { target: "es2022", assetsInlineLimit: (file) => (/\.woff2?$/.test(file) ? false : undefined) },
  test: { include: ["src/**/*.test.ts"] },
});
