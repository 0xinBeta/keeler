// Quote signatures (plan G1). Uses piece A's vector when it exists
// (tests/fixtures/quote-sig.json); otherwise only our own keypair.
import { existsSync, readFileSync } from "node:fs";
import { address, createKeyPairFromPrivateKeyBytes, getAddressFromPublicKey, signBytes } from "@solana/kit";
import { describe, expect, it } from "vitest";
import { QUOTE_MESSAGE_LEN, quoteMessage, verifyQuote, type SignedTerms } from "../src/keeler/quote-sig";

const toHex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const SOL = address("11111111111111111111111111111111");
const terms: SignedTerms = {
  programId: address("F4hjig5RJbXW6vrgYaMcuWzR55kZF6tbKyf8c36nrtvU"),
  sellMint: address("So11111111111111111111111111111111111111112"),
  buyMint: SOL,
  trader: address("AKnL4NNf3DGWZJS6cPknBuEGnVsV4A4m5tgebLHaRSZ9"),
  sellAmount: 100_000_000n,
  buyAmount: 1_998_000n,
  expiry: 1_763_900_030n,
};

describe("quote message", () => {
  it("is 167 bytes laid out as the plan says", () => {
    const m = quoteMessage(terms);
    expect(m.length).toBe(QUOTE_MESSAGE_LEN);
    expect(new TextDecoder().decode(m.slice(0, 15))).toBe("keeler-quote-v1");
    expect(toHex(m.slice(15 + 64, 15 + 96))).toBe("00".repeat(32)); // SOL as the buy mint
    expect(new DataView(m.buffer).getBigUint64(15 + 128, true)).toBe(terms.sellAmount);
    expect(new DataView(m.buffer).getBigUint64(15 + 144, true)).toBe(terms.expiry);
  });
  it("refuses values outside u64", () => {
    expect(() => quoteMessage({ ...terms, buyAmount: 1n << 64n })).toThrow(RangeError);
  });
});

describe("verifyQuote", async () => {
  const keys = await createKeyPairFromPrivateKeyBytes(new Uint8Array(32).fill(9));
  const dealer = await getAddressFromPublicKey(keys.publicKey);
  const sig = toHex(await signBytes(keys.privateKey, quoteMessage(terms)));

  it("accepts the dealer's signature over these terms", async () => {
    expect(await verifyQuote(dealer, sig, terms)).toBe(true);
  });
  it("rejects other terms, another dealer, a flipped bit and a malformed field", async () => {
    expect(await verifyQuote(dealer, sig, { ...terms, buyAmount: terms.buyAmount + 1n })).toBe(false);
    expect(await verifyQuote(dealer, sig, { ...terms, trader: SOL })).toBe(false);
    expect(await verifyQuote(terms.trader, sig, terms)).toBe(false);
    expect(await verifyQuote(dealer, (sig[0] === "0" ? "1" : "0") + sig.slice(1), terms)).toBe(false);
    for (const bad of [undefined, 42, "", sig.slice(2), sig + "00", "zz".repeat(64)]) {
      expect(await verifyQuote(dealer, bad, terms)).toBe(false);
    }
  });
});

const vector = new URL("./fixtures/quote-sig.json", import.meta.url);
describe.skipIf(!existsSync(vector))("piece A's quote signature vector", () => {
  it("verifies and reproduces the message", async () => {
    const v = JSON.parse(readFileSync(vector, "utf8"));
    const i = v.inputs;
    const t: SignedTerms = {
      programId: address(i.program_id),
      sellMint: address(i.sell_mint),
      buyMint: address(i.buy_mint),
      trader: address(i.trader),
      sellAmount: BigInt(i.sell_amount),
      buyAmount: BigInt(i.buy_amount),
      expiry: BigInt(i.expiry),
    };
    expect(toHex(quoteMessage(t))).toBe(v.message_hex);
    expect(await verifyQuote(address(v.pubkey), v.signature_hex, t)).toBe(true);
    // The dealer's HTTP reply carries the same signature, as the Worker passes it through.
    expect(v.http_reply.quote).toEqual({ buy_amount: i.buy_amount, expiry: i.expiry, signature: v.signature_hex });
    expect(await verifyQuote(address(v.pubkey), v.signature_hex, { ...t, expiry: t.expiry + 1n })).toBe(false);
  });
});
