// What the site's build decides: the compiled keepers and pool key against the service's, and a mainnet build's page.
import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { lendCompiled, parseConfig } from "../src/keeler/client";
import * as L from "../src/ui/Landing";
import { pageHtml } from "../vite.config";

/** The landing's text, as App renders it for `network`, with or without lending. */
function landing(network: string, { lend = true, market = null as Parameters<typeof L.pairOf>[0] } = {}) {
  const html = renderToStaticMarkup(createElement(L.Landing, { config: null, market: market as never, lend, open: () => {}, network }));
  return html.replace(/<[^>]+>/g, "").replaceAll("&#x27;", "'").replaceAll("&amp;", "&");
}
const TEST_WORDS = /devnet|test tokens?|test proving keys|test price|browser test wallet|test version|unaudited|tSOL|tUSD/i;

const PROGRAM = "A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC";
const K1 = "GgBaCs3NCBuZN12kCJgAW63ydqohFkHEdfdEXBPzLHq";
const K2 = "F25s3DdjXdCxYBhh2z8FBusVEMT4b9bGNFVKJi3wFoF4";
const POOL_VIEWING = "04" + "ab".repeat(32);
const built = { programId: PROGRAM, keepers: JSON.stringify([K1, K2]), poolViewing: POOL_VIEWING };

describe("the compiled lend settings", () => {
  it("are used when the service runs the same program and lists only compiled keepers, in any order", () => {
    const got = lendCompiled({ programId: PROGRAM, keepers: [K2, K1] }, built);
    expect(got?.keepers).toEqual([K1, K2]);
    // A standby in the program's set that the service does not run: the page still accepts all compiled keepers.
    expect(lendCompiled({ programId: PROGRAM, keepers: [K2] }, built)?.keepers).toEqual([K1, K2]);
    expect(got?.poolViewing).toEqual(Uint8Array.of(4, ...new Array(32).fill(0xab)));
  });

  it("hide lending when the keepers, the program or the pool key differ or are missing", () => {
    const served = { programId: PROGRAM, keepers: [K1, K2] };
    for (const [s, b] of [
      [{ ...served, keepers: [] }, built],
      [{ ...served, keepers: [K1, K2, "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin"] }, built],
      [{ programId: PROGRAM }, built],
      [{ ...served, keepers: [K1, "nope"] }, { ...built, keepers: JSON.stringify([K1, "nope"]) }],
      [{ ...served, programId: K1 }, built],
      [served, { ...built, keepers: undefined }],
      [served, { ...built, keepers: "[" }],
      [served, { ...built, keepers: "[]" }],
      [served, { ...built, poolViewing: undefined }],
      [served, { ...built, poolViewing: POOL_VIEWING.slice(2) }],
      // A compressed point, not the uncompressed one the SDK takes.
      [served, { ...built, poolViewing: "02" + POOL_VIEWING.slice(2) }],
      [served, { ...built, poolViewing: POOL_VIEWING.toUpperCase() }],
      [served, { ...built, programId: undefined }],
    ] as const) {
      expect(lendCompiled(s, b), JSON.stringify([s, b])).toBeUndefined();
    }
  });
});

describe("a deployment without lending", () => {
  it("gets the swap-led landing: the hero and the closing chapter only, and the swap FAQ", () => {
    for (const network of ["devnet", "mainnet"]) {
      const page = landing(network, { lend: false });
      expect(page).toContain("Swap in private.");
      expect(page).toContain("Try private swaps.");
      expect(page).toContain("Start a swap");
      for (const t of ["Lend in private.", "Put ", "Borrow against", "liquidation price", "Private amounts.", "Who runs the pool?", "attested enclave"]) expect(page).not.toContain(t);
    }
    expect(landing("devnet", { lend: false })).toContain("Try it with free test tokens.");
  });
});

describe("a mainnet build", () => {
  const served = { network: "mainnet", rpcPath: "/api/rpc", sprPath: "/v1/spr", programId: PROGRAM, assets: [], dealers: [], bandBps: 0, minTtlSecs: 0, maxTtlSecs: 0, faucet: true, paused: false };

  it("offers no faucet, whatever the service says", () => {
    expect(parseConfig(served, "mainnet", JSON.stringify([{ mint: PROGRAM, symbol: "SOL", decimals: 9 }])).faucet).toBe(false);
    expect(parseConfig({ ...served, network: "devnet" }, "devnet").faucet).toBe(true);
  });

  it("names no test token and says nothing of a test version anywhere on the page, and the devnet page still does", () => {
    const mainnet = landing("mainnet");
    expect(mainnet).not.toMatch(TEST_WORDS);
    for (const t of ["Supply USDC or borrow against SOL on Solana.", "Borrow against your SOL.", "Supply or borrow from your private balance on Solana.", "attested enclave"]) expect(mainnet).toContain(t);
    expect(landing("mainnet", { lend: false })).not.toMatch(TEST_WORDS);
    // The market's symbols win when the service names them.
    expect(landing("mainnet", { market: { pools: [], vaults: [{ collateral: { symbol: "JitoSOL" }, debt: { symbol: "USDT" }, collateralFactorBps: 7500, liquidationThresholdBps: 8000 }] } as never })).toMatch(/Borrow against your JitoSOL\..*75% limit with no existing debt.*80% liquidation threshold/);
    const devnet = landing("devnet");
    for (const t of [
      "Supply tUSD or borrow against tSOL on Solana. Your lending amounts stay hidden from the public chain.",
      "Put tUSD to work.",
      "Borrow against your tSOL.",
      "Know your liquidation price.",
      "Lending amounts are visible to you and to Keeler's keeper service.",
      "Try it with free test tokens.",
      "Illustrative borrow capacity",
      "Illustrative position of 4 tSOL against 420 tUSD debt.",
      "Running the keeper in an attested enclave, so that anyone can check which code holds its keys, is planned.",
      "85% loan-to-value for tSOL → tUSD",
      "Not yet.",
    ])
      expect(devnet).toContain(t);
    expect(devnet).not.toMatch(/encrypted on-chain|only you and the keeper|sample balance|demo/i);
    const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
    expect(pageHtml(html, "devnet")).toBe(html);
    expect(html).toContain("Test version, test tokens only, unaudited.");
    expect(pageHtml(html, "mainnet")).not.toMatch(TEST_WORDS);
    expect(pageHtml(html, "mainnet")).toContain('content="Keeler: private trades on Solana through Helius Privacy Rings."');
  });

  it("answers the network's FAQ for mainnet: no test tokens, test keys, test price or browser test wallet", () => {
    const html = renderToStaticMarkup(createElement(L.Faq, { lend: true, network: "mainnet" }));
    expect(html).toContain("This site runs on Solana mainnet");
    expect(html).toContain("SOL → USDC");
    expect(html).not.toMatch(/devnet|test tokens|test proving keys|test price|browser test wallet|tSOL|tUSD/i);
    const devnet = renderToStaticMarkup(createElement(L.Faq, { lend: true, network: "devnet" }));
    expect(devnet).toContain("Not yet.");
    expect(devnet).toContain("browser test wallet");
  });
});
