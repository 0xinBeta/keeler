import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// The swap circuits' digests, pinned into the bundle (src/keeler/swap-wasm.ts): the prover
// refuses a circuit.xbc or vk.bin the server serves later that differs from these.
const sha256 = (path: string) => createHash("sha256").update(readFileSync(new URL(path, import.meta.url))).digest("hex");
const swapPins = (c: string) =>
  existsSync(new URL(`public/swap/${c}/vk.bin`, import.meta.url))
    ? { xbc: sha256(`public/swap/${c}/circuit.xbc`), vk: sha256(`public/swap/${c}/vk.bin`) }
    : undefined;
// The lend package (scripts/setup-lend.sh copies it to public/lend): its key pins and circuit digests, or none.
const lendPins = () => {
  if (!existsSync(new URL("public/lend/proving-keys.json", import.meta.url))) return { keys: {}, artifacts: {} };
  const at = (c: string) =>
    existsSync(new URL(`public/lend/${c}/vk.bin`, import.meta.url))
      ? [[c, { xbc: sha256(`public/lend/${c}/circuit.xbc`), vk: sha256(`public/lend/${c}/vk.bin`) }]]
      : [];
  return {
    keys: JSON.parse(readFileSync(new URL("public/lend/proving-keys.json", import.meta.url), "utf8")),
    artifacts: Object.fromEntries(["intent", "cancel_intent", "close"].flatMap(at)),
  };
};

// The Worker (wrangler dev, port 8787, see scripts/dev.sh) serves /api and /v1/spr in development.
// It refuses cross-origin requests, and the dev server's origin is not its own, so the proxy drops Origin.
const worker = {
  target: "http://127.0.0.1:8787",
  configure: (proxy: { on: (event: "proxyReq", fn: (req: { removeHeader: (name: string) => void }) => void) => void }) =>
    proxy.on("proxyReq", (req) => req.removeHeader("origin")),
};

export default defineConfig({
  plugins: [react()],
  define: { __KEELER_SWAP_ARTIFACTS__: JSON.stringify({ make: swapPins("make"), cancel: swapPins("cancel") }), __KEELER_LEND_PINS__: JSON.stringify(lendPins()) },
  server: { proxy: { "/api": worker, "/v1/spr": worker } },
  // The Worker's CSP allows fonts from 'self' only, so small font files must not be inlined as data: URIs.
  build: { target: "es2022", assetsInlineLimit: (file) => (/\.woff2?$/.test(file) ? false : undefined) },
  test: { include: ["src/**/*.test.ts"] },
});
