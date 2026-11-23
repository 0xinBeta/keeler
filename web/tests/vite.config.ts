// Dev server for the Playwright tests: the app's Vite setup with the
// test-only stand-in for the Worker (tests/standin.ts).
import { defineConfig } from "vite";
import { proxy, standin } from "./standin";

export default defineConfig({
  root: new URL("..", import.meta.url).pathname,
  plugins: [standin()],
  server: { port: 5180, strictPort: true, host: "127.0.0.1", proxy },
  worker: { format: "es" },
});
