// End-to-end tests of the swap client in Chromium on a local network:
// `pnpm e2e`. Starts (or reuses) the local network and the test dev server.
import { defineConfig } from "@playwright/test";

const tests = new URL(".", import.meta.url).pathname;
const web = new URL("..", import.meta.url).pathname;

export default defineConfig({
  testDir: "./e2e",
  timeout: 300_000,
  workers: 1,
  reporter: "list",
  use: { baseURL: "http://127.0.0.1:5180", browserName: "chromium" },
  webServer: [
    {
      // run.sh keeps the network up until its stdin closes; the pipe holds it open.
      command: "tail -f /dev/null | ./localnet/run.sh",
      cwd: tests,
      port: 47950,
      timeout: 900_000,
      reuseExistingServer: true,
      stdout: "ignore",
      stderr: "pipe",
    },
    {
      command: "npx vite --config tests/vite.config.ts",
      cwd: web,
      url: "http://127.0.0.1:5180/tests/harness/index.html",
      timeout: 120_000,
      reuseExistingServer: true,
    },
  ],
});
