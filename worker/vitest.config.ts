import { cloudflareTest } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

// Tests call the handler directly with their own env, so no wrangler config or built site is needed.
export default defineConfig({
  plugins: [cloudflareTest({ miniflare: { compatibilityDate: "2026-11-01" } })],
});
