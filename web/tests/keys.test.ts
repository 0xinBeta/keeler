import { describe, expect, it } from "vitest";
import { checkPinForNetwork, pinnedArtifacts, pinnedKey } from "../src/keeler/swap-wasm";

describe("proving-key pins", () => {
  it("come from the bundled proving-keys.json", () => {
    expect(pinnedKey("make").sha256.length).toBe(32);
    expect(pinnedKey("cancel").sha256.length).toBe(32);
  });
  it("refuse a missing or malformed pin", () => {
    expect(() => pinnedKey("make", {})).toThrow(/no proving-key pin/);
    expect(() => pinnedKey("make", { make: { provingKeySha256: [1, 2], insecureTestSetup: false } })).toThrow();
  });
  it("on mainnet, prove only with a key marked insecureTestSetup: false; a missing flag fails", () => {
    const sha256 = new Uint8Array(32);
    expect(() => checkPinForNetwork("make", { sha256, insecureTestSetup: true }, "mainnet")).toThrow(/real setup ceremony/);
    expect(() => checkPinForNetwork("make", { sha256 }, "mainnet")).toThrow(/real setup ceremony/);
    expect(() => checkPinForNetwork("make", pinnedKey("make", { make: { provingKeySha256: Array(32).fill(0) } }), "mainnet")).toThrow();
    expect(() => checkPinForNetwork("make", { sha256, insecureTestSetup: false }, "mainnet")).not.toThrow();
    expect(() => checkPinForNetwork("make", { sha256, insecureTestSetup: true }, "devnet")).not.toThrow();
    expect(() => checkPinForNetwork("make", { sha256, insecureTestSetup: true }, "local")).not.toThrow();
  });
  it("refuse to prove in a build without VITE_NETWORK", () => {
    expect(() => checkPinForNetwork("make", { sha256: new Uint8Array(32), insecureTestSetup: false }, undefined)).toThrow(/VITE_NETWORK/);
  });
});

describe("circuit and verifying-key pins", () => {
  const hex = "ab".repeat(32);
  it("need both digests as 64 hex characters", () => {
    expect(pinnedArtifacts("make", { make: { xbc: hex, vk: hex } })).toEqual({ xbc: hex, vk: hex });
    expect(() => pinnedArtifacts("make", {})).toThrow(/no circuit and verifying-key pins/);
    expect(() => pinnedArtifacts("make", { make: { xbc: hex, vk: "" } })).toThrow();
    expect(() => pinnedArtifacts("cancel", { make: { xbc: hex, vk: hex } })).toThrow();
  });
});
