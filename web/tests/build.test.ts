// What the site's build decides: the compiled keepers and pool key against the service's, and a mainnet build's page.
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { lendCompiled, parseConfig } from "../src/keeler/client";
import { Faq } from "../src/ui/Landing";

const PROGRAM = "A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC";
const K1 = "GgBaCs3NCBuZN12kCJgAW63ydqohFkHEdfdEXBPzLHq";
const K2 = "F25s3DdjXdCxYBhh2z8FBusVEMT4b9bGNFVKJi3wFoF4";
const POOL_VIEWING = "02" + "ab".repeat(32);
const built = { programId: PROGRAM, keepers: JSON.stringify([K1, K2]), poolViewing: POOL_VIEWING };

describe("the compiled lend settings", () => {
  it("are used when the service runs the same program and lists the same keepers, in any order", () => {
    const got = lendCompiled({ programId: PROGRAM, keepers: [K2, K1] }, built);
    expect(got?.keepers).toEqual([K1, K2]);
    expect(got?.poolViewing).toEqual(Uint8Array.of(2, ...new Array(32).fill(0xab)));
  });

  it("hide lending when the keepers, the program or the pool key differ or are missing", () => {
    const served = { programId: PROGRAM, keepers: [K1, K2] };
    for (const [s, b] of [
      [{ ...served, keepers: [K1] }, built],
      [{ ...served, keepers: [K1, K2, "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin"] }, built],
      [{ programId: PROGRAM }, built],
      [{ ...served, keepers: [K1, "nope"] }, { ...built, keepers: JSON.stringify([K1, "nope"]) }],
      [{ ...served, programId: K1 }, built],
      [served, { ...built, keepers: undefined }],
      [served, { ...built, keepers: "[" }],
      [served, { ...built, keepers: "[]" }],
      [served, { ...built, poolViewing: undefined }],
      [served, { ...built, poolViewing: POOL_VIEWING.slice(2) }],
      [served, { ...built, poolViewing: POOL_VIEWING.toUpperCase() }],
      [served, { ...built, programId: undefined }],
    ] as const) {
      expect(lendCompiled(s, b), JSON.stringify([s, b])).toBeUndefined();
    }
  });
});

describe("a mainnet build", () => {
  const served = { network: "mainnet", rpcPath: "/api/rpc", sprPath: "/v1/spr", programId: PROGRAM, assets: [], dealers: [], bandBps: 0, minTtlSecs: 0, maxTtlSecs: 0, faucet: true, paused: false };

  it("offers no faucet, whatever the service says", () => {
    expect(parseConfig(served, "mainnet").faucet).toBe(false);
    expect(parseConfig({ ...served, network: "devnet" }, "devnet").faucet).toBe(true);
  });

  it("answers the network's FAQ for mainnet: no test tokens, test keys, test price or browser test wallet", () => {
    const html = renderToStaticMarkup(createElement(Faq, { lend: true, network: "mainnet" }));
    expect(html).toContain("This site runs on Solana mainnet");
    expect(html).toContain("SOL → USDC");
    expect(html).not.toMatch(/devnet|test tokens|test proving keys|test price|browser test wallet|tSOL|tUSD/i);
    const devnet = renderToStaticMarkup(createElement(Faq, { lend: true, network: "devnet" }));
    expect(devnet).toContain("Not yet.");
    expect(devnet).toContain("browser test wallet");
  });
});
