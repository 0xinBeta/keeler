// Runs every case of tests/fixtures/lend-vectors.json (written by core/src/lend/tests.rs).
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import * as L from "../src/keeler/lend-math";

type J = Record<string, any>;
const doc = JSON.parse(readFileSync(new URL("./fixtures/lend-vectors.json", import.meta.url), "utf8"));

const n = (x: string) => BigInt(x);
const reserveOf = (r: J): L.Reserve => ({
  supplyShares: n(r.supply_shares),
  debtShares: n(r.debt_shares),
  supplyPrice: n(r.supply_price),
  borrowPrice: n(r.borrow_price),
  rateBps: n(r.rate_bps),
  lastUpdate: n(r.last_update),
});
const reserveJson = (r: L.Reserve) => ({
  supply_shares: `${r.supplyShares}`,
  debt_shares: `${r.debtShares}`,
  supply_price: `${r.supplyPrice}`,
  borrow_price: `${r.borrowPrice}`,
  rate_bps: `${r.rateBps}`,
  last_update: `${r.lastUpdate}`,
});
const curveOf = (c: [string, string][]): L.Curve => ({ kinks: c.map(([u, r]) => ({ utilBps: n(u), rateBps: n(r) })) });
const vaultOf = (v: J): L.Vault => ({
  cfBps: n(v.cf_bps),
  ltBps: n(v.lt_bps),
  lmlBps: n(v.lml_bps),
  penaltyBps: n(v.penalty_bps),
});
const str = (x: bigint | null) => (x === null ? null : `${x}`);

const fns: Record<string, (i: J) => unknown> = {
  total_supply: (i) => str(L.totalSupply(reserveOf(i.reserve))),
  total_debt: (i) => str(L.totalDebt(reserveOf(i.reserve))),
  utilisation_bps: (i) => str(L.utilisationBps(reserveOf(i.reserve))),
  accrue: (i) => reserveJson(L.accrue(reserveOf(i.reserve), n(i.now), n(i.fee_bps))),
  curve_rate: (i) => str(L.curveRate(curveOf(i.curve), n(i.util_bps))),
  refresh_rate: (i) => reserveJson(L.refreshRate(reserveOf(i.reserve), curveOf(i.curve))),
  shares_for_supply: (i) => str(L.sharesForSupply(n(i.amount), n(i.supply_price))),
  shares_to_burn: (i) => str(L.sharesToBurn(n(i.amount), n(i.supply_price))),
  assets_of: (i) => str(L.assetsOf(n(i.shares), n(i.supply_price))),
  debt_of: (i) => str(L.debtOf(n(i.shares), n(i.borrow_price))),
  borrow_shares: (i) => str(L.borrowShares(n(i.amount), n(i.borrow_price))),
  repay_shares: (i) => {
    const x = L.repayShares(n(i.shares), n(i.amount), n(i.borrow_price));
    return { shares: str(x.shares), change: str(x.change) };
  },
  supply: (i) => {
    const x = L.supply(reserveOf(i.reserve), n(i.amount), curveOf(i.curve));
    return { reserve: reserveJson(x.reserve), shares: str(x.shares) };
  },
  redeem: (i) => {
    const x = L.redeem(reserveOf(i.reserve), n(i.idle), n(i.held), n(i.amount), curveOf(i.curve));
    return { reserve: reserveJson(x.reserve), burned: str(x.burned) };
  },
  borrow: (i) => {
    const x = L.borrow(reserveOf(i.reserve), n(i.idle), n(i.amount), curveOf(i.curve));
    return { reserve: reserveJson(x.reserve), shares: str(x.shares) };
  },
  repay: (i) => {
    const x = L.repay(reserveOf(i.reserve), n(i.position_shares), n(i.amount), curveOf(i.curve));
    return { reserve: reserveJson(x.reserve), shares: str(x.shares), change: str(x.change) };
  },
  collateral_value: (i) => str(L.collateralValue(n(i.collateral), n(i.price))),
  can_draw: (i) => L.canDraw(n(i.debt), n(i.value), vaultOf(i.vault)),
  is_liquidatable: (i) => L.isLiquidatable(n(i.debt), n(i.value), vaultOf(i.vault)),
  above_max_limit: (i) => L.aboveMaxLimit(n(i.debt), n(i.value), vaultOf(i.vault)),
  liquidate: (i) => {
    const x = L.liquidate(n(i.debt_shares), n(i.borrow_price), n(i.collateral), n(i.price), vaultOf(i.vault));
    return {
      repay: str(x.repay),
      payout: str(x.payout),
      debt_shares: str(x.debtShares),
      collateral: str(x.collateral),
    };
  },
  health_bps: (i) => str(L.healthBps(n(i.debt), n(i.value))),
  liquidation_price: (i) => str(L.liquidationPrice(n(i.debt), n(i.collateral), n(i.lt_bps))),
  apr_bps: (i) => {
    const x = L.aprBps(n(i.rate_bps), n(i.util_bps), n(i.fee_bps));
    return { borrow_bps: str(x.borrowBps), supply_bps: str(x.supplyBps) };
  },
};

function run(f: string, input: J): J {
  const call = fns[f];
  if (!call) throw new Error(`no TypeScript twin for ${f}`);
  try {
    return { out: call(input) };
  } catch (e) {
    if (e instanceof L.LendError) return { error: e.code };
    throw e;
  }
}

describe("lend vectors", () => {
  it("covers every function", () => {
    expect(new Set(doc.cases.map((c: J) => c.fn))).toEqual(new Set(Object.keys(fns)));
  });
  for (const c of doc.cases as J[]) {
    it(`${c.fn}: ${c.name}`, () => {
      expect(run(c.fn, c.in)).toEqual("out" in c ? { out: c.out } : { error: c.error });
    });
  }
});

describe("constants", () => {
  it("match the Rust side", () => {
    const k = doc.constants;
    expect([L.PRICE_SCALE, L.BPS, L.YEAR, L.MAX_BORROW_UTIL_BPS].map(String)).toEqual([
      k.PRICE_SCALE,
      k.BPS,
      k.YEAR,
      k.MAX_BORROW_UTIL_BPS,
    ]);
    expect(L.TUSD_POOL).toEqual({ curve: curveOf(k.tusd_pool.curve), feeBps: n(k.tusd_pool.fee_bps) });
    expect(L.TSOL_TUSD_VAULT).toEqual(vaultOf(k.tsol_tusd_vault));
  });
  it("refuses values outside Rust's integer types", () => {
    expect(() => L.debtOf(-1n, L.PRICE_SCALE)).toThrow(L.LendError);
    expect(() => L.debtOf(1n << 64n, L.PRICE_SCALE)).toThrow(L.LendError);
  });
});

describe("a step that removes no debt share", () => {
  const curve = L.TUSD_POOL.curve;
  const borrowPrice = 1_100_000_000_000n;
  const reserve = { ...L.newReserve(0n, 0n), supplyShares: 10n ** 12n, debtShares: 10n ** 9n, borrowPrice };

  it("a partial repay of one base unit is refused", () => {
    expect(L.repayShares(10n ** 9n, 1n, borrowPrice).shares).toBe(10n ** 9n);
    expect(() => L.repay(reserve, 10n ** 9n, 1n, curve)).toThrow(expect.objectContaining({ code: "zero_shares" }));
  });

  it("a repay that removes one share, and a full repay, still pass", () => {
    expect(L.repay(reserve, 10n ** 9n, 2n, curve).shares).toBe(10n ** 9n - 1n);
    expect(L.repay(reserve, 10n ** 9n, L.debtOf(10n ** 9n, borrowPrice), curve).shares).toBe(0n);
    expect(L.repay(reserve, 0n, 0n, curve).shares).toBe(0n);
  });

  it("a liquidation that removes no share is refused", () => {
    expect(() => L.liquidate(1_000_000n, 1_000_000_000_001n, 10_000_000n, 100_000n, L.TSOL_TUSD_VAULT)).toThrow(
      expect.objectContaining({ code: "nothing_to_liquidate" }),
    );
  });
});
