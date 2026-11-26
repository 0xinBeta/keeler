// What the keeper decides on top of the program's maths (cli/src/keeper.rs, `fn liquidation` and the draw
// bound), with its default limits; deploy/devnet's keeper configs are pinned to these by a test.
import type { Vault } from "./index";
import * as L from "./lend-math";

/** The keeper's default `bound_margin_bps`: it draws at (price - confidence) less this much, and liquidates at (price + confidence) plus this much. */
export const KEEPER_MARGIN_BPS = 50n;

/** The keeper's default `min_repay`: it leaves a liquidation that would repay less, unless the position is past the max limit. */
export const KEEPER_MIN_REPAY = 10_000n;

/** The price the keeper values a liquidation at: the high end of the oracle's band, plus its margin, rounded up. */
export const liquidationBound = (vault: Vault) => ((vault.price + vault.confidence) * (L.BPS + KEEPER_MARGIN_BPS) + L.BPS - 1n) / L.BPS;

const vaultParams = (v: Vault): L.Vault => ({
  cfBps: BigInt(v.collateralFactorBps),
  ltBps: BigInt(v.liquidationThresholdBps),
  lmlBps: BigInt(v.liquidationMaxLimitBps),
  penaltyBps: BigInt(v.penaltyBps),
});

/** The keeper's own test: `liquidate` at `liquidationBound` succeeds, and repays at least `KEEPER_MIN_REPAY` or the position is past the max limit there. */
export function keeperLiquidates(collateral: bigint, debt: bigint, vault: Vault): boolean {
  const bound = liquidationBound(vault);
  const v = vaultParams(vault);
  try {
    return L.liquidate(debt, L.PRICE_SCALE, collateral, bound, v).repay >= KEEPER_MIN_REPAY || L.aboveMaxLimit(debt, L.collateralValue(collateral, bound), v);
  } catch {
    return false;
  }
}
