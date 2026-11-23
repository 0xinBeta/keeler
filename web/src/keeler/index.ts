// The contract between the swap client (src/keeler) and the interface (src/ui).
// Amounts are bigint in base units; times are chain time in unix seconds.
import type { Wallet } from "@wallet-standard/base";

export type Asset = { mint: string; symbol: string; decimals: number };

export type Config = {
  network: "local" | "devnet" | "mainnet";
  programId: string;
  assets: Asset[];
  dealers: { pubkey: string }[];
  bandBps: number;
  minTtlSecs: number;
  maxTtlSecs: number;
  faucet: boolean;
};

export type Quote = {
  dealer: string;
  sell: Asset;
  buy: Asset;
  sellAmount: bigint;
  buyAmount: bigint;
  expiry: number;
  inBand: boolean;
  /** Signed distance from the reference price in basis points; positive is better for the trader. */
  vsFairBps: number;
};

export type OrderStatus = "pending" | "locked" | "settled" | "refundable" | "refunded" | "failed";

export type Order = {
  id: string;
  sell: Asset;
  buy: Asset;
  sellAmount: bigint;
  buyAmount: bigint;
  expiry: number;
  status: OrderStatus;
  error?: string;
};

export type SwapStep = "checking" | "proving" | "signing" | "locked";

export type Balance = { asset: Asset; public: bigint; private: bigint; largestNote: bigint };

export interface Keeler {
  config(): Promise<Config>;
  /** Wallets the page can use: installed Wallet Standard wallets plus Keeler's in-page wallet. */
  wallets(): Wallet[];
  connect(wallet: Wallet): Promise<{ address: string }>;
  /** One wallet signature that derives the private keys. Kept in memory only. */
  unlock(): Promise<void>;
  isRegistered(): Promise<boolean>;
  register(): Promise<void>;
  balances(): Promise<Balance[]>;
  deposit(asset: Asset, amount: bigint): Promise<void>;
  withdraw(asset: Asset, amount: bigint): Promise<void>;
  /** `quotes` is best first; out-of-band quotes are included with `inBand: false`. */
  quotes(sell: Asset, buy: Asset, sellAmount: bigint): Promise<{ fair: bigint; quotes: Quote[] }>;
  swap(quote: Quote, on: (step: SwapStep) => void): Promise<Order>;
  orders(): Promise<Order[]>;
  refund(orderId: string): Promise<void>;
  faucet(): Promise<void>;
  chainNow(): Promise<number>;
}

/** Errors the interface can show as they are: `message` says what happened and what to do. */
export class KeelerError extends Error {
  constructor(
    public code:
      | "wallet-rejected"
      | "wallet-unsupported"
      | "keys-mismatch"
      | "not-registered"
      | "insufficient-note"
      | "no-quotes"
      | "quote-expired"
      | "out-of-band"
      | "dealer-offline"
      | "storage"
      | "busy"
      | "network"
      | "proof"
      | "rate-limited",
    message: string,
  ) {
    super(message);
  }
}
