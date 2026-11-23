// Exact conversions between base units (bigint) and decimal text. Nothing here rounds.

/** Full-precision amount, thousands grouped, trailing zeros of the fraction dropped. */
export function formatUnits(amount: bigint, decimals: number): string {
  const neg = amount < 0n;
  const s = (neg ? -amount : amount).toString().padStart(decimals + 1, "0");
  const whole = s.slice(0, s.length - decimals).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  const frac = s.slice(s.length - decimals).replace(/0+$/, "");
  return (neg ? "-" : "") + whole + (frac ? "." + frac : "");
}

/** Parses what a person typed. Returns an error sentence instead of guessing. */
export function parseUnits(text: string, decimals: number, symbol: string): bigint | string {
  const t = text.trim().replace(/,/g, "");
  if (t === "") return "Enter an amount.";
  const m = /^(\d*)(?:\.(\d*))?$/.exec(t);
  if (!m || (m[1] === "" && !m[2])) return "Enter a number, like 12.5.";
  const frac = m[2] ?? "";
  if (frac.length > decimals) return `${symbol} has ${decimals} decimal places at most.`;
  const v = BigInt((m[1] || "0") + frac.padEnd(decimals, "0"));
  return v === 0n ? "Enter an amount above zero." : v;
}

export const bps = (b: number) => `${b >= 0 ? "+" : "−"}${(Math.abs(b) / 100).toFixed(2)}%`;

export const short = (key: string) => `${key.slice(0, 4)}…${key.slice(-4)}`;

export function countdown(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}
