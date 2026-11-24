// Browser tests against the mock client (no chain, no Worker): `pnpm e2e:mock`.
// The real-stack tests stay in playwright.config.ts, which starts the local network.
import { defineConfig } from "@playwright/test";

const port = Number(process.env.KEELER_MOCK_PORT ?? 5292);

export default defineConfig({
  testDir: "./e2e",
  testMatch: /lend\.spec\.ts/,
  timeout: 180_000,
  workers: 1,
  reporter: "list",
  use: { browserName: "chromium", baseURL: `http://127.0.0.1:${port}` },
  projects: [
    { name: "desktop", use: { viewport: { width: 1440, height: 900 } } },
    { name: "phone", use: { viewport: { width: 390, height: 844 }, deviceScaleFactor: 2 } },
  ],
  webServer: {
    command: `VITE_NETWORK=devnet VITE_KEELER_MOCK=1 npx vite --host 127.0.0.1 --port ${port} --strictPort`,
    cwd: new URL("..", import.meta.url).pathname,
    url: `http://127.0.0.1:${port}/`,
    timeout: 120_000,
    reuseExistingServer: true,
  },
});
