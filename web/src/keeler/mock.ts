// In-memory Keeler for building the interface without a chain. `?mock=<scenario>` makes one error reachable:
// dealer-offline, no-quotes, wallet-rejected, storage, rate-limited.
import type { Wallet } from "@wallet-standard/base";
import { KeelerError } from "./index";
import type { Asset, Balance, Config, Keeler, Order, Quote, SwapStep } from "./index";

type Scenario = "dealer-offline" | "no-quotes" | "wallet-rejected" | "storage" | "rate-limited" | null;

const tUSD: Asset = { mint: "tUSDm1nt5Y7oQqP2xG8dVt4bKcWfz3LrHn6aJe9sMpU", symbol: "tUSD", decimals: 6 };
const tSOL: Asset = { mint: "tSoLm1ntR4wQ8nZ2vY6kHc3dPx9fTb7GjEu5LsAaNoV", symbol: "tSOL", decimals: 9 };

// Reference price: 1 tSOL = 142.37 tUSD, kept as a fraction so amounts stay exact.
const PRICE_NUM = 14237n;
const PRICE_DEN = 100n;

// Dealer offsets from the reference price in basis points. The second never settles, the third is out of band.
const DEALERS = [
  { pubkey: "DeaL1xGq7mVb3NcT8rKwZ5yPh2sJfA4uEo9iLdQnRtYk", bps: 12, settles: true },
  { pubkey: "DeaL2pHk4nWc9MxR6tLvB8zQj3sFgE5aYo7uKdTiNmPr", bps: -18, settles: false },
  { pubkey: "DeaL3sRt6yUb2KxN9mQwC4vPh8jLgF7aZe5oTdHiMnYc", bps: -420, settles: true },
];

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const now = () => Math.floor(Date.now() / 1000);

const icon = `data:image/svg+xml;base64,${btoa(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" fill="#0B0C0F"/><ellipse cx="16" cy="16" rx="11" ry="4.5" fill="none" stroke="#E9B872" stroke-width="2"/><circle cx="22" cy="14" r="1.8" fill="#F4EBD6"/></svg>',
)}` as const;

function fair(sell: Asset, buy: Asset, amount: bigint): bigint {
  const scale = 10n ** BigInt(buy.decimals);
  const unscale = 10n ** BigInt(sell.decimals);
  return sell.symbol === "tUSD"
    ? (amount * scale * PRICE_DEN) / (unscale * PRICE_NUM)
    : (amount * scale * PRICE_NUM) / (unscale * PRICE_DEN);
}

export function createMockKeeler(): Keeler {
  const scenario = new URLSearchParams(location.search).get("mock") as Scenario;
  const config: Config = {
    network: "devnet",
    programId: "KeeLerSwap1111111111111111111111111111111111",
    assets: [tUSD, tSOL],
    dealers: DEALERS.map(({ pubkey }) => ({ pubkey })),
    bandBps: 100,
    // Short TTLs so the refund path is reachable in under a minute.
    minTtlSecs: 20,
    maxTtlSecs: 120,
    faucet: true,
  };

  const wallet: Wallet = {
    version: "1.0.0",
    name: "Keeler test wallet",
    icon,
    chains: ["solana:devnet"],
    features: {},
    accounts: [],
  };

  let address: string | null = null;
  let unlocked = false;
  let registered = false;
  let busy = false;
  const pub = new Map<string, bigint>([[tUSD.mint, 0n], [tSOL.mint, 0n]]);
  const notes = new Map<string, bigint[]>([[tUSD.mint, []], [tSOL.mint, []]]);
  const orders: (Order & { settlesAt: number | null })[] = [];

  const need = (what: "connect" | "unlock") => {
    if (!address) throw new KeelerError("wallet-unsupported", "Choose a wallet first.");
    if (what === "unlock" && !unlocked) throw new KeelerError("wallet-rejected", "Unlock your private balance first.");
  };
  const addNote = (asset: Asset, amount: bigint) => {
    if (amount > 0n) notes.get(asset.mint)!.push(amount);
  };
  // Spend the smallest note that covers the amount and return the change, as the client does.
  const spend = (asset: Asset, amount: bigint) => {
    const list = notes.get(asset.mint)!;
    const fit = list.filter((n) => n >= amount).sort((a, b) => (a < b ? -1 : 1))[0];
    if (fit === undefined)
      throw new KeelerError(
        "insufficient-note",
        `No single private note of ${asset.symbol} covers that amount. Enter at most your largest note, or deposit more.`,
      );
    list.splice(list.indexOf(fit), 1);
    addNote(asset, fit - amount);
  };
  const refresh = (o: (typeof orders)[number]) => {
    if (o.status !== "locked") return;
    if (o.settlesAt !== null && Date.now() >= o.settlesAt) {
      o.status = "settled";
      addNote(o.buy, o.buyAmount);
    } else if (now() > o.expiry) o.status = "refundable";
  };
  const strip = ({ settlesAt: _, ...o }: (typeof orders)[number]): Order => ({ ...o });

  return {
    async config() {
      await sleep(250);
      return config;
    },
    wallets() {
      return [wallet];
    },
    async connect() {
      await sleep(400);
      address = "7fUq3nVb8KxR2mTz5YcLwP9hJd4sGa6eNoQiBtXu1rMk";
      return { address };
    },
    async unlock() {
      need("connect");
      await sleep(900);
      if (scenario === "wallet-rejected")
        throw new KeelerError("wallet-rejected", "Your wallet declined the signature, so nothing was unlocked. Try again and approve the request.");
      unlocked = true;
    },
    async isRegistered() {
      need("unlock");
      await sleep(300);
      return registered;
    },
    async register() {
      need("unlock");
      await sleep(1800);
      registered = true;
    },
    async balances(): Promise<Balance[]> {
      need("unlock");
      await sleep(300);
      return config.assets.map((asset) => {
        const list = notes.get(asset.mint)!;
        return {
          asset,
          public: pub.get(asset.mint)!,
          private: list.reduce((a, b) => a + b, 0n),
          largestNote: list.reduce((a, b) => (b > a ? b : a), 0n),
        };
      });
    },
    async deposit(asset, amount) {
      need("unlock");
      if (amount <= 0n || amount > pub.get(asset.mint)!)
        throw new KeelerError("insufficient-note", `You do not have that much public ${asset.symbol}. Enter a smaller amount.`);
      await sleep(2200);
      pub.set(asset.mint, pub.get(asset.mint)! - amount);
      addNote(asset, amount);
    },
    async withdraw(asset, amount) {
      need("unlock");
      await sleep(2400);
      spend(asset, amount);
      pub.set(asset.mint, pub.get(asset.mint)! + amount);
    },
    async quotes(sell, buy, sellAmount) {
      need("unlock");
      await sleep(1400);
      if (scenario === "rate-limited")
        throw new KeelerError("rate-limited", "Too many quote requests from this connection. Wait a minute, then try again.");
      if (scenario === "dealer-offline")
        throw new KeelerError("dealer-offline", "No dealer answered within 3 seconds. They may be offline; try again in a few minutes.");
      if (scenario === "no-quotes")
        throw new KeelerError("no-quotes", "Dealers are online but none would quote this pair and amount. Try a smaller amount.");
      const f = fair(sell, buy, sellAmount);
      const t = now();
      const quotes: Quote[] = DEALERS.map((d, i) => ({
        dealer: d.pubkey,
        sell,
        buy,
        sellAmount,
        buyAmount: (f * BigInt(10_000 + d.bps)) / 10_000n,
        expiry: t + 45 + i * 5,
        inBand: Math.abs(d.bps) <= config.bandBps,
        vsFairBps: d.bps,
      }));
      quotes.sort((a, b) => Number(b.inBand) - Number(a.inBand) || b.vsFairBps - a.vsFairBps);
      return { fair: f, quotes };
    },
    async swap(quote, on: (step: SwapStep) => void) {
      need("unlock");
      if (busy) throw new KeelerError("busy", "A swap is already in progress. Wait for it to finish.");
      busy = true;
      try {
        on("checking");
        await sleep(700);
        if (!quote.inBand) throw new KeelerError("out-of-band", "This quote is outside the price band. Choose another quote.");
        if (quote.expiry - now() < config.minTtlSecs)
          throw new KeelerError("quote-expired", "This quote expires too soon to lock safely. Get fresh quotes.");
        if (scenario === "storage")
          throw new KeelerError("storage", "This browser could not save the order, so nothing was sent. Allow site storage for this page (private windows often block it) and try again.");
        const list = notes.get(quote.sell.mint)!;
        if (!list.some((n) => n >= quote.sellAmount))
          throw new KeelerError("insufficient-note", `No single private note of ${quote.sell.symbol} covers that amount. Enter at most your largest note.`);
        on("proving");
        await sleep(4200);
        on("signing");
        await sleep(1500);
        spend(quote.sell, quote.sellAmount);
        on("locked");
        const dealer = DEALERS.find((d) => d.pubkey === quote.dealer);
        const order = {
          id: crypto.randomUUID(),
          sell: quote.sell,
          buy: quote.buy,
          sellAmount: quote.sellAmount,
          buyAmount: quote.buyAmount,
          expiry: quote.expiry,
          status: "locked" as const,
          settlesAt: dealer?.settles ? Date.now() + 5000 : null,
        };
        orders.unshift(order);
        return strip(order);
      } finally {
        busy = false;
      }
    },
    async orders() {
      await sleep(150);
      orders.forEach(refresh);
      return orders.map(strip);
    },
    async refund(orderId) {
      const o = orders.find((x) => x.id === orderId);
      if (!o) throw new KeelerError("storage", "This browser has no record of that order.");
      refresh(o);
      if (o.status !== "refundable")
        throw new KeelerError("quote-expired", "The quote has not expired yet. You can take your funds back once the countdown ends.");
      await sleep(3000);
      o.status = "refunded";
      addNote(o.sell, o.sellAmount);
    },
    async faucet() {
      need("connect");
      await sleep(1200);
      if (scenario === "rate-limited")
        throw new KeelerError("rate-limited", "This wallet already received test tokens today. Try again tomorrow.");
      pub.set(tUSD.mint, pub.get(tUSD.mint)! + 1_000_000000n);
      pub.set(tSOL.mint, pub.get(tSOL.mint)! + 5_000000000n);
    },
    async chainNow() {
      await sleep(80);
      return now();
    },
  };
}
