import type { SolanaSignTransactionFeature } from "@solana/wallet-standard-features";
import type { StandardConnectFeature } from "@wallet-standard/features";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { parseConfig, walletAnswer } from "../src/keeler/client";
import { KeelerError } from "../src/keeler/index";
import { testWalletAllowed, walletChain } from "../src/keeler/network";
import { createInPageWallet, IN_PAGE_WALLET_NAME } from "../src/keeler/wallet";

beforeAll(() => {
  const m = new Map<string, string>();
  globalThis.localStorage = {
    get length() {
      return m.size;
    },
    key: (i: number) => [...m.keys()][i] ?? null,
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
    clear: () => m.clear(),
  };
});

describe("the browser test wallet", () => {
  it("is named plainly", () => {
    expect(createInPageWallet().name).toBe("Browser test wallet");
    expect(IN_PAGE_WALLET_NAME).toBe("Browser test wallet");
  });

  it("is offered on local and devnet only, never on mainnet", () => {
    expect(testWalletAllowed("local")).toBe(true);
    expect(testWalletAllowed("devnet")).toBe(true);
    expect(testWalletAllowed("mainnet")).toBe(false);
    expect(testWalletAllowed(undefined)).toBe(false);
  });

  it("signs only for the chains it names", async () => {
    const w = createInPageWallet();
    const [account] = (await (w.features as StandardConnectFeature)["standard:connect"].connect()).accounts;
    const sign = (w.features as SolanaSignTransactionFeature)["solana:signTransaction"].signTransaction;
    // Not a transaction at all: the chain is checked before the bytes are decoded.
    const transaction = new Uint8Array(8);
    await expect(sign({ account: account!, transaction, chain: walletChain("mainnet") })).rejects.toThrow(/signs only for solana:localnet and solana:devnet/);
    await expect(sign({ account: account!, transaction })).rejects.toThrow(/not an unnamed chain/);
  });

  it("is asked to sign for the chain id the Wallet Standard uses", () => {
    expect(walletChain("local")).toBe("solana:localnet");
    expect(walletChain("devnet")).toBe("solana:devnet");
  });
});

describe("the build's network", () => {
  const served = { network: "local", assets: [], dealers: [] };
  it("must equal the service's network", () => {
    expect(parseConfig(served, "local").network).toBe("local");
    expect(() => parseConfig(served, "mainnet")).toThrow(/built for mainnet, but the service runs local/);
  });
  it("must be set: a build without VITE_NETWORK will not start", () => {
    expect(() => parseConfig(served, undefined)).toThrow(/built without VITE_NETWORK/);
  });
});

describe("the build's assets", () => {
  const SOL = { mint: "11111111111111111111111111111111", symbol: "SOL", decimals: 9 };
  const USD = { mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", symbol: "USDC", decimals: 6 };
  const pins = JSON.stringify([SOL, USD]);
  const vault = { id: 1, collateralMint: SOL.mint, debtMint: USD.mint, cfBps: 8000, ltBps: 8500, lmlBps: 9000, penaltyBps: 100, minCollateral: "1", minDebt: "1" };
  const lend = { programId: SOL.mint, pools: [{ mint: USD.mint, curve: [], feeBps: 1000, maxUtilisationBps: 9500, minSupply: "1" }], vaults: [vault] };
  const served = (over: Record<string, unknown> = {}) => ({ network: "mainnet", dealers: [], assets: [SOL, { ...USD, maxSell: "5" }], lend, ...over });

  it("take only maxSell from the service", () => {
    expect(parseConfig(served(), "mainnet", pins).assets).toEqual([SOL, { ...USD, maxSell: 5n }]);
  });
  it("refuse a decimals mismatch", () => {
    expect(() => parseConfig(served({ assets: [SOL, { ...USD, decimals: 9 }] }), "mainnet", pins)).toThrow(/settings for USDC differ/);
  });
  it("refuse a mint the build does not list", () => {
    const extra = { mint: "So11111111111111111111111111111111111111112", symbol: "wSOL", decimals: 9 };
    expect(() => parseConfig(served({ assets: [SOL, USD, extra] }), "mainnet", pins)).toThrow(/settings for wSOL differ/);
    expect(() => parseConfig(served({ assets: [SOL, SOL] }), "mainnet", pins)).toThrow(/assets differ/);
  });
  it("are required on mainnet, and must be well formed", () => {
    expect(() => parseConfig(served(), "mainnet", undefined)).toThrow(/built without VITE_ASSETS/);
    expect(() => parseConfig(served(), "mainnet", "[{\"mint\":\"x\"}]")).toThrow(/malformed VITE_ASSETS/);
    expect(parseConfig(served({ network: "devnet" }), "devnet", undefined).assets[1]).toEqual({ ...USD, maxSell: 5n });
  });
  it("refuse vault parameters other than the compiled ones, and lending mints outside the build", () => {
    expect(() => parseConfig(served({ lend: { ...lend, vaults: [{ ...vault, ltBps: 8600 }] } }), "mainnet", pins)).toThrow(/vault parameters differ/);
    expect(() => parseConfig(served({ lend: { ...lend, vaults: [{ ...vault, id: 2 }] } }), "mainnet", pins)).toThrow(/vault parameters differ/);
    expect(() => parseConfig(served({ network: "devnet", lend: { ...lend, vaults: [{ ...vault, cfBps: 9000 }] } }), "devnet", undefined)).toThrow(/vault parameters differ/);
    expect(() => parseConfig(served(), "mainnet", JSON.stringify([SOL]))).toThrow(/settings for USDC differ/);
    expect(() => parseConfig(served({ assets: [SOL] }), "mainnet", pins)).toThrow(/lending assets differ/);
  });
});

describe("a wallet prompt", () => {
  afterEach(() => vi.useRealTimers());
  it("that never answers fails as wallet-rejected after the timeout", async () => {
    vi.useFakeTimers();
    const stuck = walletAnswer(new Promise<never>(() => {}), "nothing was sent", 120_000);
    const caught = stuck.catch((e: unknown) => e);
    await vi.advanceTimersByTimeAsync(120_000);
    const e = await caught;
    expect(e).toBeInstanceOf(KeelerError);
    expect((e as KeelerError).code).toBe("wallet-rejected");
    expect((e as KeelerError).message).toMatch(/did not answer within 120 s, so nothing was sent/);
  });
  it("that answers in time passes the answer through", async () => {
    await expect(walletAnswer(Promise.resolve(7), "x", 1_000)).resolves.toBe(7);
  });
});
