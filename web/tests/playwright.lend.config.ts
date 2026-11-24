// The lend page's real proving path in a browser: `npx playwright test --config tests/playwright.lend.config.ts`
// after `scripts/lend-web-stack.sh start` (local network at offset 2400, the keeper stand-in, the Worker on 5301).
// The spec skips itself while that stack is down. This config starts only the harness dev server, on 5300.
import { existsSync, readFileSync } from "node:fs";
import { defineConfig } from "@playwright/test";

const root = new URL("../..", import.meta.url).pathname;
export const LEND_STACK = `${root}target/localnet/lend-web-2400/stack.json`;
const stack = existsSync(LEND_STACK) ? (JSON.parse(readFileSync(LEND_STACK, "utf8")) as { programId: string; keeper: string }) : undefined;
// Without a stack the page builds without lend; the spec skips before it opens a page.
const lendEnv = stack ? `VITE_LEND_PROGRAM_ID=${stack.programId} VITE_LEND_KEEPER=${stack.keeper}` : "";

export default defineConfig({
  testDir: "./e2e",
  testMatch: /lend-real\.spec\.ts/,
  timeout: 900_000,
  workers: 1,
  reporter: "list",
  use: { browserName: "chromium", baseURL: "http://127.0.0.1:5300" },
  webServer: {
    command: `VITE_NETWORK=local ${lendEnv} KEELER_WORKER_URL=http://127.0.0.1:5301 npx vite --host 127.0.0.1 --port 5300 --strictPort`,
    cwd: new URL("..", import.meta.url).pathname,
    url: "http://127.0.0.1:5300/tests/harness/index.html",
    timeout: 120_000,
    reuseExistingServer: false,
  },
});
