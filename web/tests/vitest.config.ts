import { defineConfig } from "vitest/config";

// Unit tests of the swap client, beside the interface's own under src/.
export default defineConfig({
  test: { root: ".", include: ["src/**/*.test.ts", "tests/**/*.test.ts"] },
});
