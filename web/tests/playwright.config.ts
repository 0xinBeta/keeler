// End-to-end tests on a local network, `pnpm e2e`:
// - app: piece C's page, BUILT, served by the real Worker (`wrangler dev`, scripts/dev.sh),
//   driven by its visible controls;
// - client: the swap client's safety rules on the test harness page (Vite dev, proxied to the
//   same Worker), with faults injected at the browser's network layer.
// scripts/dev.sh starts (or the tests reuse) the local stack, the build and the Worker.
import { defineConfig } from "@playwright/test";

export const OFFSET = process.env.KEELER_OFFSET ?? "900";
const root = new URL("../..", import.meta.url).pathname;
const web = new URL("..", import.meta.url).pathname;
export const STACK = `${root}target/localnet/keeler-web-${OFFSET}`;

export default defineConfig({
  testDir: "./e2e",
  timeout: 900_000,
  workers: 1,
  reporter: "list",
  use: { browserName: "chromium" },
  projects: [
    { name: "app", testMatch: /app\.spec\.ts/, use: { baseURL: "http://127.0.0.1:8787" } },
    { name: "client", testMatch: /client\.spec\.ts/, use: { baseURL: "http://127.0.0.1:5180" } },
  ],
  webServer: [
    {
      command: `bash scripts/dev.sh ${OFFSET} 8787`,
      cwd: root,
      url: "http://127.0.0.1:8787/api/config",
      timeout: 900_000,
      reuseExistingServer: true,
      // dev.sh stops the stack it started when it gets SIGTERM.
      gracefulShutdown: { signal: "SIGTERM", timeout: 120_000 },
      stdout: "ignore",
      stderr: "pipe",
    },
    {
      // The harness needs the dev server; it proxies /api and /v1/spr to the Worker above.
      command: `VITE_NETWORK=local VITE_DEALERS="$(node -p 'JSON.stringify([require(process.argv[1]).dealer.pubkey])' ${STACK}.json)" VITE_ASSETS="$(node -p 'JSON.stringify(require(process.argv[1]).assets.map(({ mint, symbol, decimals }) => ({ mint, symbol, decimals })))' ${STACK}.json)" npx vite --host 127.0.0.1 --port 5180 --strictPort`,
      cwd: web,
      url: "http://127.0.0.1:5180/tests/harness/index.html",
      timeout: 120_000,
      reuseExistingServer: true,
    },
  ],
});
