// In-memory Keeler Lend for the mock client: one tUSD pool and one tSOL/tUSD vault, accruing in real time.
// `?mock=keeper-offline` refuses every keeper operation, `?mock=keeper-stalls` leaves intents unfilled until
// they expire (30 s), `?mock=no-liquidity` refuses withdrawals from Earn.
// `mock` on the returned object is a test control (price moves); the interface shows it only in a mock build.
import { KeelerError } from "./index";
import type { Asset, KeelerLend, LendAccount, LendKind, LendOperation, LendStep, Liquidation, Pool, Position, Vault } from "./index";
import * as L from "./lend-math";

const PRICE_SCALE = L.PRICE_SCALE;

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

const START_PRICE = 142_370_000_000n; // 142.37 tUSD per tSOL
const CURVE = [
  { utilisationBps: 0, rateBps: 0 },
  { utilisationBps: 8800, rateBps: 500 },
  { utilisationBps: 9300, rateBps: 700 },
  { utilisationBps: 10_000, rateBps: 1500 },
];
const CORE_CURVE: L.Curve = { kinks: CURVE.map((k) => ({ utilBps: BigInt(k.utilisationBps), rateBps: BigInt(k.rateBps) })) };

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

  const cv: L.Vault = {
    cfBps: BigInt(vault.collateralFactorBps),
    ltBps: BigInt(vault.liquidationThresholdBps),
    lmlBps: BigInt(vault.liquidationMaxLimitBps),
    penaltyBps: BigInt(vault.penaltyBps),
  };

  // Reserve: other suppliers' 2.4M tUSD, 1.73M of it borrowed. Prices are 1e12-scaled share prices.
  let r: L.Reserve = {
    supplyPrice: PRICE_SCALE,
    borrowPrice: PRICE_SCALE,
    supplyShares: 2_400_000_000000n,
    debtShares: 1_730_000_000000n,
    rateBps: 0n,
    lastUpdate: BigInt(now()),
  };
  let myShares = 0n;
  const positions: MockPosition[] = [];
  const ops: MockOp[] = [];
  let watcher = 0;

  const supplied = () => L.totalSupply(r);
  const borrowed = () => L.totalDebt(r);
  const utilisation = () => Number(L.utilisationBps(r));
  const setRate = () => {
    r = L.refreshRate(r, CORE_CURVE);
  };
  // Applies the stored rate for the elapsed time, then stores the new rate (the plan's accrual order).
  const accrueAll = () => {
    const t = BigInt(now());
    if (t <= r.lastUpdate) return;
    r = L.accrue(r, t, BigInt(pool.feeBps));
    setRate();
  };
  setRate();

  const debtOf = (p: MockPosition) => L.debtOf(p.debtShares, r.borrowPrice);
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
        let l: L.Liquidation;
        try {
          l = L.liquidate(p.debtShares, r.borrowPrice, p.collateral, vault.price, cv);
        } catch {
          continue; // not liquidatable, or nothing to take
        }
        r = { ...r, debtShares: r.debtShares - (p.debtShares - l.debtShares) };
        p.lastLiquidation = { at: now(), debtRepaid: l.repay, collateralSold: l.payout, price: vault.price };
        p.debtShares = l.debtShares;
        p.collateral = l.collateral;
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
        borrowAprBps: Number(r.rateBps),
        supplyAprBps: Number(L.aprBps(r.rateBps, BigInt(util), BigInt(pool.feeBps)).supplyBps),
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
        receipts: myShares > 0n ? [{ pool: pool.id, shares: myShares, value: L.assetsOf(myShares, r.supplyPrice) }] : [],
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
        const shares = L.sharesForSupply(amount, r.supplyPrice);
        myShares += shares;
        r = { ...r, supplyShares: r.supplyShares + shares };
      }, on);
    },

    async redeem(_poolId, amount, on) {
      s.need();
      keeper();
      accrueAll();
      const value = L.assetsOf(myShares, r.supplyPrice);
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
      const burn = amount >= L.assetsOf(myShares, r.supplyPrice) ? myShares : L.sharesToBurn(amount, r.supplyPrice);
      myShares -= burn;
      r = { ...r, supplyShares: r.supplyShares - burn };
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
      if (withdraw > p.collateral || !L.canDraw(debt + borrow, L.collateralValue(p.collateral - withdraw, vault.price), cv))
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
      const shares = L.borrowShares(borrow, r.borrowPrice); // debt shares round up
      p.debtShares += shares;
      r = { ...r, debtShares: r.debtShares + shares };
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
        const done = L.repayShares(p.debtShares, amount, r.borrowPrice);
        r = { ...r, debtShares: r.debtShares - (p.debtShares - done.shares) };
        p.debtShares = done.shares;
        s.addNote(tUSD, done.change);
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
