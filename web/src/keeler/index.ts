// The contract between the swap client (src/keeler) and the interface (src/ui).
// Amounts are bigint in base units; times are chain time in unix seconds.
import type { Wallet } from "@wallet-standard/base";

export type Asset = { mint: string; symbol: string; decimals: number; maxSell?: bigint };

export type Config = {
  network: "local" | "devnet" | "mainnet";
  programId: string;
  assets: Asset[];
  dealers: { pubkey: string }[];
  bandBps: number;
  minTtlSecs: number;
  maxTtlSecs: number;
  faucet: boolean;
  paused: boolean;
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
  quotes(sell: Asset, buy: Asset, sellAmount: bigint): Promise<{ fair: bigint; priceTime: number; quotes: Quote[] }>;
  swap(quote: Quote, on: (step: SwapStep) => void): Promise<Order>;
  orders(): Promise<Order[]>;
  rescanOrders(): Promise<Order[]>;
  refund(orderId: string): Promise<void>;
  faucet(): Promise<void>;
  chainNow(): Promise<number>;
  /** Keeler Lend. Absent while the deployment has no lend program; the page then hides Earn and Borrow. */
  lend?: KeelerLend;
}

// ---- Keeler Lend (docs/plan-lend.md). Units, everywhere below:
// - token amounts: bigint, base units of the token named beside them;
// - rates, factors, fees and health: number, basis points (10_000 = 100%);
// - prices: bigint scaled by 1e12, in debt base units per collateral base unit;
// - times: number, chain time in unix seconds.

/** One curve point: the borrow rate at a utilisation. The curve is linear between points. */
export type RatePoint = { utilisationBps: number; rateBps: number };

/** A shared Earn pool. Rates, utilisation and size are the keeper's coarse published figures, not exact state. */
export type Pool = {
  /** The pool's mint, which also fixes the reserve note's address. */
  id: string;
  asset: Asset;
  supplyAprBps: number;
  borrowAprBps: number;
  utilisationBps: number;
  /** Total supplied, base units of `asset`, rounded by the keeper. */
  size: bigint;
  /** Chain time of the published bucket these figures come from. */
  asOf: number;
  minSupply: bigint;
  /** Share of borrowers' interest not paid on to suppliers. */
  feeBps: number;
  /** A borrow may not take utilisation above this. */
  maxUtilisationBps: number;
  curve: RatePoint[];
};

/** A borrowing vault: one collateral token, debt in its pool's token. */
export type Vault = {
  id: number;
  /** `Pool.id` of the pool the debt is borrowed from. */
  pool: string;
  collateral: Asset;
  debt: Asset;
  /** Borrowing (and withdrawing collateral) may leave debt at most this share of the collateral's value. */
  collateralFactorBps: number;
  /** Above this loan-to-value a position can be liquidated, partially, back to this line. */
  liquidationThresholdBps: number;
  /** Above this loan-to-value a liquidation repays the whole debt. */
  liquidationMaxLimitBps: number;
  /** Extra collateral sold on a liquidation, as a share of the debt repaid; it stays in the pool's buffer. */
  penaltyBps: number;
  /** Base units of `collateral`. */
  minCollateral: bigint;
  /** Base units of `debt`. */
  minDebt: bigint;
  /** Oracle price, 1e12-scaled debt base units per collateral base unit. */
  price: bigint;
  priceTime: number;
};

/** An Earn deposit: private shares of one pool. */
export type Receipt = {
  pool: string;
  shares: bigint;
  /** What the shares are worth now, base units of the pool's asset. Redeeming this much burns them all. */
  value: bigint;
};

export type Liquidation = {
  at: number;
  /** Base units of the vault's debt token. */
  debtRepaid: bigint;
  /** Base units of the vault's collateral token, penalty included. */
  collateralSold: bigint;
  /** The price bound it was sold at, 1e12-scaled. */
  price: bigint;
};

/** A borrowing position. Health and liquidation price are not here: the page computes them from the vault's price. */
export type Position = {
  vault: number;
  id: string;
  /** Base units of the vault's collateral token. */
  collateral: bigint;
  /** Base units of the vault's debt token, interest included, rounded up. */
  debt: bigint;
  /** The most recent liquidation of this position, if any. */
  lastLiquidation: Liquidation | null;
};

export type LendKind = "supply" | "redeem" | "deposit" | "draw" | "repay" | "close";

/**
 * Where an operation is. Intents (supply, deposit, repay): proving → signing → keeper → landed.
 * Keeper-built (redeem, draw): keeper → signing → landed. Close: proving → signing → landed.
 */
export type LendStep = "proving" | "signing" | "keeper" | "landed";

/** `refundable`: an intent the keeper did not fill before `expiry`; `refundIntent` takes the funds back. */
export type LendStatus = "pending" | "landed" | "refundable" | "refunded" | "failed";

export type LendOperation = {
  id: string;
  kind: LendKind;
  /** `Pool.id` for supply and redeem; otherwise the vault's pool. */
  pool: string;
  vault: number | null;
  /** The position it acts on; set for a deposit that opened one once the keeper has filled it. */
  positionId: string | null;
  /** What moves, each in its own token's base units: in for supply, deposit and repay; out otherwise. A draw lists borrow, then withdraw. */
  amounts: { asset: Asset; amount: bigint }[];
  step: LendStep;
  status: LendStatus;
  /** Chain time after which an unfilled intent can be refunded; null for operations that are not intents. */
  expiry: number | null;
  createdAt: number;
  error?: string;
};

export type LendAccount = {
  receipts: Receipt[];
  positions: Position[];
  /** Newest first. */
  operations: LendOperation[];
  /** Chain time the values above were computed at. */
  asOf: number;
};

/**
 * Money-moving calls resolve once the wallet's part is done: an intent with status "pending" at step "keeper",
 * anything else "landed". `lendAccount()` then tracks it. `on` reports each step as it starts.
 */
export interface KeelerLend {
  pools(): Promise<{ pools: Pool[]; vaults: Vault[] }>;
  lendAccount(): Promise<LendAccount>;
  /** Earn: `amount` of the pool's asset from one private note into the pool. */
  supply(pool: string, amount: bigint, on: (step: LendStep) => void): Promise<LendOperation>;
  /** Earn: `amount` of the pool's asset out of the receipt into the private balance. */
  redeem(pool: string, amount: bigint, on: (step: LendStep) => void): Promise<LendOperation>;
  /** Adds collateral from one private note; `positionId` null opens a new position. */
  deposit(vault: number, positionId: string | null, amount: bigint, on: (step: LendStep) => void): Promise<LendOperation>;
  /** Borrows `borrow` debt and withdraws `withdraw` collateral in one keeper-built transaction; either may be 0n. */
  draw(positionId: string, borrow: bigint, withdraw: bigint, on: (step: LendStep) => void): Promise<LendOperation>;
  /** Repays from one private note; anything above the debt comes back as change. */
  repay(positionId: string, amount: bigint, on: (step: LendStep) => void): Promise<LendOperation>;
  /** Closes a position with no debt and returns its collateral, without the keeper. */
  closePosition(positionId: string, on: (step: LendStep) => void): Promise<LendOperation>;
  /** Takes back the funds of an intent the keeper did not fill, once it is refundable. */
  refundIntent(operationId: string): Promise<void>;
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
      | "rate-limited"
      | "paused"
      // Lend
      | "keeper-offline"
      | "no-liquidity"
      | "below-minimum"
      | "unhealthy"
      | "price-stale",
    message: string,
  ) {
    super(message);
  }
}
