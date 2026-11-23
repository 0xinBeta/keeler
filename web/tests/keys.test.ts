import { describe, expect, it } from "vitest";
import { checkPinForNetwork, pinnedKey } from "../src/keeler/swap-wasm";

describe("proving-key pins", () => {
  it("come from the bundled proving-keys.json", () => {
    expect(pinnedKey("make").sha256.length).toBe(32);
    expect(pinnedKey("cancel").sha256.length).toBe(32);
  });
  it("refuse a missing or malformed pin", () => {
    expect(() => pinnedKey("make", {})).toThrow(/no proving-key pin/);
    expect(() => pinnedKey("make", { make: { provingKeySha256: [1, 2], insecureTestSetup: false } })).toThrow();
  });
  it("refuse a test-setup key on mainnet only", () => {
    const insecure = { sha256: new Uint8Array(32), insecureTestSetup: true };
    expect(() => checkPinForNetwork("make", insecure, "mainnet")).toThrow(/insecure test setup/);
    expect(() => checkPinForNetwork("make", insecure, "devnet")).not.toThrow();
    expect(() => checkPinForNetwork("make", { ...insecure, insecureTestSetup: false }, "mainnet")).not.toThrow();
  });
});
