// The lend page against the real keeper: `npx playwright test --config tests/playwright.lend.config.ts` after
// `scripts/lend-web-stack.sh start` (local network at offset 2400, the keeper on 47810, the Worker on 5301).
// The spec skips itself while that stack is down. This config starts only the app's dev server, on 5300.
import { existsSync, readFileSync } from "node:fs";
import { defineConfig } from "@playwright/test";

const root = new URL("../..", import.meta.url).pathname;
export const LEND_STACK = `${root}target/localnet/lend-web-2400/stack.json`;
type Stack = { programId: string; keeper: string; keepers?: string[]; poolViewing?: string; tusd: string };
const stack = existsSync(LEND_STACK) ? (JSON.parse(readFileSync(LEND_STACK, "utf8")) as Stack) : undefined;
// Without a stack the page builds without lend; the spec skips before it opens a page. Without poolViewing in the
// stack (an older lend-web-stack.sh) the page hides lending too.
const lendEnv = stack
  ? `VITE_LEND_PROGRAM_ID=${stack.programId} VITE_LEND_KEEPERS='${JSON.stringify(stack.keepers ?? [stack.keeper])}' VITE_LEND_POOL_VIEWING=${stack.poolViewing ?? ""} VITE_ASSETS='${JSON.stringify([{ mint: "11111111111111111111111111111111", symbol: "SOL", decimals: 9 }, { mint: stack.tusd, symbol: "tUSD", decimals: 6 }])}'`
  : "";

export default defineConfig({
  testDir: "./e2e",
  testMatch: /lend-real\.spec\.ts/,
  timeout: 900_000,
  workers: 1,
  reporter: "list",
  // A click or fill that cannot happen fails in a minute instead of at the test's timeout.
  use: { browserName: "chromium", baseURL: "http://127.0.0.1:5300", viewport: { width: 1440, height: 900 }, actionTimeout: 60_000 },
  webServer: {
    command: `VITE_NETWORK=local ${lendEnv} KEELER_WORKER_URL=http://127.0.0.1:5301 npx vite --host 127.0.0.1 --port 5300 --strictPort`,
    cwd: new URL("..", import.meta.url).pathname,
    url: "http://127.0.0.1:5300/",
    timeout: 120_000,
    reuseExistingServer: false,
  },
});
