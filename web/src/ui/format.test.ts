import { expect, test } from "vitest";
import { formatUnits, parseUnits } from "./format";

test("formatUnits keeps every digit", () => {
  expect(formatUnits(1_234_567_891n, 6)).toBe("1,234.567891");
  expect(formatUnits(5n, 9)).toBe("0.000000005");
  expect(formatUnits(5_000_000_000n, 9)).toBe("5");
  expect(formatUnits(0n, 6)).toBe("0");
});

test("parseUnits is exact and refuses extra precision", () => {
  expect(parseUnits("1,234.567891", 6, "tUSD")).toBe(1_234_567_891n);
  expect(parseUnits(".5", 9, "tSOL")).toBe(500_000_000n);
  expect(parseUnits("1.0000001", 6, "tUSD")).toBe("tUSD has 6 decimal places at most.");
  expect(typeof parseUnits("abc", 6, "tUSD")).toBe("string");
  expect(typeof parseUnits("0", 6, "tUSD")).toBe("string");
});
