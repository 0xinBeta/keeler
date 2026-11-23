// Port of `core/src/price.rs` and `best_quote` in `core/src/rfq.rs`, in bigint.
export type Price = { price: bigint; expo: number };
export type RawQuote = { buyAmount: bigint; expiry: number };

const U64_MAX = (1n << 64n) - 1n;
const U128_MAX = (1n << 128n) - 1n;

/**
 * `sellAmount * sell.price * 10^e / buy.price`, floored, with
 * `e = sell.expo - buy.expo + buyDecimals - sellDecimals`. Undefined when a
 * price is not positive, the result is zero or does not fit a u64, or the
 * Rust u128 arithmetic would overflow.
 */
export function fairBuyAmount(
  sellAmount: bigint,
  sell: Price,
  buy: Price,
  sellDecimals: number,
  buyDecimals: number,
): bigint | undefined {
  if (sell.price <= 0n || buy.price <= 0n) return undefined;
  const e = sell.expo - buy.expo + buyDecimals - sellDecimals;
  // 10^39 already exceeds u128; Rust's `checked_pow` refuses from there.
  if (Math.abs(e) > 38) return undefined;
  const scale = 10n ** BigInt(Math.abs(e));
  const num = e >= 0 ? sellAmount * sell.price * scale : sellAmount * sell.price;
  const den = e >= 0 ? buy.price : buy.price * scale;
  // Refuse where the Rust u128 arithmetic would overflow, so both sides agree.
  if (num > U128_MAX || den > U128_MAX) return undefined;
  const out = num / den;
  return out > 0n && out <= U64_MAX ? out : undefined;
}

/** True when `buyAmount` is within `bandBps` of `fair` on both sides. */
export function withinBand(buyAmount: bigint, fair: bigint, bandBps: number): boolean {
  if (bandBps > 10_000 || bandBps < 0) return false;
  const band = BigInt(bandBps);
  return buyAmount * 10_000n >= fair * (10_000n - band) && buyAmount * 10_000n <= fair * (10_000n + band);
}

/** Signed distance from `fair` in basis points, floored toward zero; positive is better for the trader. */
export function vsFairBps(buyAmount: bigint, fair: bigint): number {
  return fair === 0n ? 0 : Number(((buyAmount - fair) * 10_000n) / fair);
}

/** Time to live within `[minTtl, maxTtl]` and not expired. */
export function ttlOk(expiry: number, now: number, minTtl: number, maxTtl: number): boolean {
  return expiry > now && expiry - now >= minTtl && expiry - now <= maxTtl;
}

/** Index of the largest in-band quote whose TTL is within bounds; lowest index wins a tie. */
export function bestQuote(
  quotes: readonly (RawQuote | null | undefined)[],
  fair: bigint,
  bandBps: number,
  now: number,
  minTtl: number,
  maxTtl: number,
): number | undefined {
  let best: number | undefined;
  quotes.forEach((q, i) => {
    if (!q || !withinBand(q.buyAmount, fair, bandBps) || !ttlOk(q.expiry, now, minTtl, maxTtl)) return;
    if (best === undefined || q.buyAmount > quotes[best]!.buyAmount) best = i;
  });
  return best;
}
