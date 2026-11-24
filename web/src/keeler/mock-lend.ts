// In-memory Keeler Lend for the mock client: one tUSD pool and one tSOL/tUSD vault, accruing in real time.
// `?mock=keeper-offline` refuses every keeper operation, `?mock=keeper-stalls` leaves intents unfilled until
// they expire (30 s), `?mock=no-liquidity` refuses withdrawals from Earn.
// `mock` on the returned object is a test control (price moves); the interface shows it only in a mock build.
import { KeelerError } from "./index";
import type { Asset, KeelerLend, LendAccount, LendKind, LendOperation, LendStep, Liquidation, Pool, Position, Vault } from "./index";
import { accrue, borrowRateBps, liquidationSize, maxBorrow, maxWithdraw, PRICE_SCALE, supplyRateBps } from "../ui/lend-math";

export type MockLendControls = {
  /** Moves the oracle price by `bps` (negative is down). */
  movePrice(bps: number): void;
  /** Drops the price so the first position with debt sits between the threshold and the max limit, then the keeper liquidates it. */
  crash(): void;
  resetPrice(): void;
};

type Shared = {
  scenario: string | null;
  need: () => void;
  /** Spends one private note covering `amount`, or throws insufficient-note. */
  spend: (asset: Asset, amount: bigint) => void;
  addNote: (asset: Asset, amount: bigint) => void;
  hasNote: (asset: Asset, amount: bigint) => boolean;
};

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const now = () => Math.floor(Date.now() / 1000);
const ceilDiv = (a: bigint, b: bigint) => (a + b - 1n) / b;

const START_PRICE = 142_370_000_000n; // 142.37 tUSD per tSOL
const CURVE = [
  { utilisationBps: 0, rateBps: 0 },
  { utilisationBps: 8800, rateBps: 500 },
  { utilisationBps: 9300, rateBps: 700 },
  { utilisationBps: 10_000, rateBps: 1500 },
];

type MockPosition = { id: string; vault: number; collateral: bigint; debtShares: bigint; lastLiquidation: Liquidation | null };
type MockOp = LendOperation & { fill: (() => void) | null };

export function createMockLend(tUSD: Asset, tSOL: Asset, s: Shared): KeelerLend & { mock: MockLendControls } {
  const pool: Pool = {
    id: tUSD.mint,
    asset: tUSD,
    supplyAprBps: 0,
    borrowAprBps: 0,
    utilisationBps: 0,
    size: 0n,
    asOf: now(),
    minSupply: 1_000000n,
    feeBps: 1000,
    maxUtilisationBps: 9500,
    curve: CURVE,
  };
  const vault: Vault = {
    id: 1,
    pool: pool.id,
    collateral: tSOL,
    debt: tUSD,
    collateralFactorBps: 8000,
    liquidationThresholdBps: 8500,
    liquidationMaxLimitBps: 9000,
    penaltyBps: 100,
    minCollateral: 50_000000n, // 0.05 tSOL
    minDebt: 5_000000n,
    price: START_PRICE,
    priceTime: now(),
  };

  // Reserve: other suppliers' 2.4M tUSD, 1.73M of it borrowed. Prices are 1e12-scaled share prices.
  const r = {
    supplyPrice: PRICE_SCALE,
    borrowPrice: PRICE_SCALE,
    supplyShares: 2_400_000_000000n,
    debtShares: 1_730_000_000000n,
    buffer: 0n,
    rateBps: 0,
    last: Date.now() / 1000,
  };
  let myShares = 0n;
  const positions: MockPosition[] = [];
  const ops: MockOp[] = [];
  let watcher = 0;

  const supplied = () => (r.supplyShares * r.supplyPrice) / PRICE_SCALE;
  const borrowed = () => ceilDiv(r.debtShares * r.borrowPrice, PRICE_SCALE);
  const utilisation = () => {
    const sup = supplied();
    return sup === 0n ? 0 : Number((borrowed() * 10_000n) / sup);
  };
  const setRate = () => {
    r.rateBps = borrowRateBps(utilisation(), CURVE);
  };
  // Applies the stored rate for the elapsed time, then stores the new rate (the plan's accrual order).
  const accrueAll = () => {
    const t = Date.now() / 1000;
    const dt = t - r.last;
    if (dt <= 0) return;
    const u = utilisation();
    r.borrowPrice = accrue(r.borrowPrice, r.rateBps, dt) + 1n; // borrow price rounds up
    r.supplyPrice = accrue(r.supplyPrice, supplyRateBps(r.rateBps, u, pool.feeBps), dt);
    r.last = t;
    setRate();
  };
  setRate();

  const debtOf = (p: MockPosition) => ceilDiv(p.debtShares * r.borrowPrice, PRICE_SCALE);
  const find = (id: string) => {
    const p = positions.find((x) => x.id === id);
    if (!p) throw new KeelerError("storage", "This browser has no record of that position. Reload the page to read your account again.");
    return p;
  };
  const keeper = () => {
    if (s.scenario === "keeper-offline")
      throw new KeelerError("keeper-offline", "The keeper is not answering right now, so nothing was sent. Your funds have not moved; try again in a few minutes.");
  };
  const fresh = () => {
    if (now() - vault.priceTime > 90)
      throw new KeelerError("price-stale", "The tSOL price is more than 90 seconds old, so borrowing waits for a fresh one. Try again in a minute.");
  };

  const op = (kind: LendKind, amounts: { asset: Asset; amount: bigint }[], positionId: string | null, intent: boolean): MockOp => {
    const o: MockOp = {
      id: crypto.randomUUID(),
      kind,
      pool: pool.id,
      vault: kind === "supply" || kind === "redeem" ? null : vault.id,
      positionId,
      amounts,
      step: intent || kind === "close" ? "proving" : "keeper",
      status: "pending",
      expiry: intent ? now() + (s.scenario === "keeper-stalls" ? 30 : 120) : null,
      createdAt: now(),
      fill: null,
    };
    ops.unshift(o);
    return o;
  };
  const strip = ({ fill: _, ...o }: MockOp): LendOperation => ({ ...o, amounts: [...o.amounts] });
  const step = (o: MockOp, st: LendStep, on: (s: LendStep) => void) => {
    o.step = st;
    on(st);
  };
  const landed = (o: MockOp, on: (s: LendStep) => void) => {
    o.status = "landed";
    step(o, "landed", on);
  };

  /** An intent: proved in the browser, signed, then filled by the keeper about 3 s later (or never, when stalled). */
  const intent = async (o: MockOp, asset: Asset, amount: bigint, apply: () => void, on: (s: LendStep) => void) => {
    step(o, "proving", on);
    await sleep(2200);
    step(o, "signing", on);
    await sleep(900);
    s.spend(asset, amount);
    step(o, "keeper", on);
    o.fill = () => {
      if (o.status !== "pending") return;
      accrueAll();
      apply();
      setRate();
      o.status = "landed";
      o.step = "landed";
    };
    if (s.scenario !== "keeper-stalls") setTimeout(() => o.fill?.(), 3000);
    return strip(o);
  };

  const refresh = (o: MockOp) => {
    if (o.status === "pending" && o.step === "keeper" && o.expiry !== null && now() > o.expiry) o.status = "refundable";
  };

  // The keeper's watcher: liquidates any position past the threshold, a few seconds after the price moves.
  const watch = () => {
    clearTimeout(watcher);
    watcher = window.setTimeout(() => {
      accrueAll();
      for (const p of positions) {
        const debt = debtOf(p);
        const size = liquidationSize(p.collateral, debt, vault.price, vault);
        if (!size) continue;
        const debtValue = (size.repaid * PRICE_SCALE) / r.borrowPrice;
        p.debtShares = size.repaid >= debt ? 0n : p.debtShares - debtValue;
        p.collateral -= size.sold;
        r.debtShares -= debtValue;
        // The penalty's proceeds stay in the reserve as its buffer.
        r.buffer += (size.repaid * BigInt(vault.penaltyBps)) / 10_000n;
        p.lastLiquidation = { at: now(), debtRepaid: size.repaid, collateralSold: size.sold, price: vault.price };
      }
      setRate();
    }, 4000);
  };
  const setPrice = (price: bigint) => {
    vault.price = price;
    vault.priceTime = now();
    watch();
  };

  return {
    async pools() {
      await sleep(200);
      accrueAll();
      const util = utilisation();
      // Coarse published figures: size to 10,000 tUSD, utilisation to 1%, rates to 0.01%.
      Object.assign(pool, {
        size: (supplied() / 10_000_000000n) * 10_000_000000n,
        utilisationBps: Math.round(util / 100) * 100,
        borrowAprBps: r.rateBps,
        supplyAprBps: supplyRateBps(r.rateBps, util, pool.feeBps),
        asOf: now(),
      });
      vault.priceTime = now(); // the mock oracle is always fresh
      return { pools: [{ ...pool }], vaults: [{ ...vault }] };
    },

    async lendAccount(): Promise<LendAccount> {
      s.need();
      await sleep(150);
      accrueAll();
      ops.forEach(refresh);
      const positionsOut: Position[] = positions.map((p) => ({
        vault: p.vault,
        id: p.id,
        collateral: p.collateral,
        debt: debtOf(p),
        lastLiquidation: p.lastLiquidation,
      }));
      return {
        receipts: myShares > 0n ? [{ pool: pool.id, shares: myShares, value: (myShares * r.supplyPrice) / PRICE_SCALE }] : [],
        positions: positionsOut,
        operations: ops.map(strip),
        asOf: now(),
      };
    },

    async supply(_poolId, amount, on) {
      s.need();
      keeper();
      if (amount < pool.minSupply) throw new KeelerError("below-minimum", "The pool takes at least 1 tUSD at a time. Enter a larger amount.");
      if (!s.hasNote(tUSD, amount))
        throw new KeelerError("insufficient-note", "No single private note of tUSD covers that amount. Enter at most your largest note, or deposit more.");
      const o = op("supply", [{ asset: tUSD, amount }], null, true);
      return intent(o, tUSD, amount, () => {
        const shares = (amount * PRICE_SCALE) / r.supplyPrice;
        myShares += shares;
        r.supplyShares += shares;
      }, on);
    },

    async redeem(_poolId, amount, on) {
      s.need();
      keeper();
      accrueAll();
      const value = (myShares * r.supplyPrice) / PRICE_SCALE;
      if (amount > value) throw new KeelerError("insufficient-note", "That is more than your deposit is worth. Enter a smaller amount.");
      const idle = supplied() - borrowed();
      if (s.scenario === "no-liquidity" || amount > idle)
        throw new KeelerError("no-liquidity", "The pool has no idle funds for that right now; most of it is borrowed. The rate rises until repayments come in. Try a smaller amount, or again later.");
      const o = op("redeem", [{ asset: tUSD, amount }], null, false);
      step(o, "keeper", on);
      await sleep(1500);
      step(o, "signing", on);
      await sleep(900);
      accrueAll();
      const burn = amount >= (myShares * r.supplyPrice) / PRICE_SCALE ? myShares : ceilDiv(amount * PRICE_SCALE, r.supplyPrice);
      myShares -= burn;
      r.supplyShares -= burn;
      s.addNote(tUSD, amount);
      setRate();
      landed(o, on);
      return strip(o);
    },

    async deposit(_vaultId, positionId, amount, on) {
      s.need();
      keeper();
      const p = positionId ? find(positionId) : null;
      if (!p && amount < vault.minCollateral)
        throw new KeelerError("below-minimum", "A new position needs at least 0.05 tSOL of collateral. Enter a larger amount.");
      if (!s.hasNote(tSOL, amount))
        throw new KeelerError("insufficient-note", "No single private note of tSOL covers that amount. Enter at most your largest note, or deposit more.");
      const o = op("deposit", [{ asset: tSOL, amount }], positionId, true);
      return intent(o, tSOL, amount, () => {
        if (p) p.collateral += amount;
        else {
          const id = crypto.randomUUID();
          positions.push({ id, vault: vault.id, collateral: amount, debtShares: 0n, lastLiquidation: null });
          o.positionId = id;
        }
      }, on);
    },

    async draw(positionId, borrow, withdraw, on) {
      s.need();
      keeper();
      fresh();
      accrueAll();
      const p = find(positionId);
      const debt = debtOf(p);
      if (borrow > 0n && debt + borrow < vault.minDebt)
        throw new KeelerError("below-minimum", "A loan must be at least 5 tUSD. Enter a larger amount.");
      if (borrow > maxBorrow(p.collateral - withdraw, debt, vault.price, vault.collateralFactorBps) ||
          withdraw > maxWithdraw(p.collateral, debt + borrow, vault.price, vault.collateralFactorBps))
        throw new KeelerError("unhealthy", "That would take the position past 80% of its collateral's value. Enter a smaller amount.");
      const sup = supplied();
      if (borrow > 0n && (borrowed() + borrow) * 10_000n > sup * BigInt(pool.maxUtilisationBps))
        throw new KeelerError("no-liquidity", "The pool can't lend that much right now: a borrow may not take it above 95% used. Try a smaller amount.");
      const amounts = [
        ...(borrow > 0n ? [{ asset: tUSD, amount: borrow }] : []),
        ...(withdraw > 0n ? [{ asset: tSOL, amount: withdraw }] : []),
      ];
      const o = op("draw", amounts, positionId, false);
      step(o, "keeper", on);
      await sleep(1500);
      step(o, "signing", on);
      await sleep(900);
      accrueAll();
      const shares = ceilDiv(borrow * PRICE_SCALE, r.borrowPrice); // debt shares round up
      p.debtShares += shares;
      r.debtShares += shares;
      p.collateral -= withdraw;
      s.addNote(tUSD, borrow);
      s.addNote(tSOL, withdraw);
      setRate();
      landed(o, on);
      return strip(o);
    },

    async repay(positionId, amount, on) {
      s.need();
      keeper();
      const p = find(positionId);
      if (!s.hasNote(tUSD, amount))
        throw new KeelerError("insufficient-note", "No single private note of tUSD covers that amount. Enter at most your largest note, or deposit more.");
      const o = op("repay", [{ asset: tUSD, amount }], positionId, true);
      return intent(o, tUSD, amount, () => {
        const debt = debtOf(p);
        const paid = amount > debt ? debt : amount;
        const shares = paid >= debt ? p.debtShares : (paid * PRICE_SCALE) / r.borrowPrice; // burned rounded down
        p.debtShares -= shares;
        r.debtShares -= shares;
        s.addNote(tUSD, amount - paid); // change
      }, on);
    },

    async closePosition(positionId, on) {
      s.need();
      const p = find(positionId);
      accrueAll();
      if (debtOf(p) > 0n) throw new KeelerError("unhealthy", "This position still has debt. Repay it all, then close.");
      const o = op("close", [{ asset: tSOL, amount: p.collateral }], positionId, false);
      step(o, "proving", on);
      await sleep(2000);
      step(o, "signing", on);
      await sleep(900);
      s.addNote(tSOL, p.collateral);
      positions.splice(positions.indexOf(p), 1);
      landed(o, on);
      return strip(o);
    },

    async refundIntent(operationId) {
      s.need();
      const o = ops.find((x) => x.id === operationId);
      if (!o) throw new KeelerError("storage", "This browser has no record of that request.");
      refresh(o);
      if (o.status !== "refundable")
        throw new KeelerError("quote-expired", "This request has not expired yet. You can take the funds back once the countdown ends.");
      await sleep(2500);
      o.status = "refunded";
      o.fill = null;
      s.addNote(o.amounts[0]!.asset, o.amounts[0]!.amount);
    },

    mock: {
      movePrice(bps) {
        setPrice((vault.price * BigInt(10_000 + bps)) / 10_000n);
      },
      crash() {
        accrueAll();
        const p = positions.find((x) => x.debtShares > 0n && x.collateral > 0n);
        if (!p) return setPrice((vault.price * 7n) / 10n);
        // Loan-to-value 87.5%: past the threshold, under the max limit, so the liquidation is partial.
        setPrice((debtOf(p) * 10_000n * PRICE_SCALE) / (p.collateral * 8750n));
      },
      resetPrice() {
        setPrice(START_PRICE);
      },
    },
  };
}
