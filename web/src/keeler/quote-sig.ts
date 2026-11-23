// Dealer quote signatures (plan G1): Ed25519 over a fixed 167-byte message.
import { getAddressEncoder, getPublicKeyFromAddress, verifySignature, type Address, type SignatureBytes } from "@solana/kit";

const DOMAIN = new TextEncoder().encode("keeler-quote-v1");
export const QUOTE_MESSAGE_LEN = 167;

export type SignedTerms = {
  programId: Address;
  sellMint: Address;
  buyMint: Address;
  trader: Address;
  sellAmount: bigint;
  buyAmount: bigint;
  expiry: bigint;
};

/** `"keeler-quote-v1"` ‖ program ‖ sell mint ‖ buy mint ‖ trader ‖ sell u64 LE ‖ buy u64 LE ‖ expiry u64 LE. */
export function quoteMessage(t: SignedTerms): Uint8Array {
  const out = new Uint8Array(QUOTE_MESSAGE_LEN);
  const enc = getAddressEncoder();
  out.set(DOMAIN, 0);
  [t.programId, t.sellMint, t.buyMint, t.trader].forEach((a, i) => out.set(enc.encode(a), DOMAIN.length + 32 * i));
  const view = new DataView(out.buffer);
  // setBigUint64 throws on a negative value; one above u64 would wrap, so refuse it.
  [t.sellAmount, t.buyAmount, t.expiry].forEach((v, i) => {
    if (v < 0n || v > 0xffff_ffff_ffff_ffffn) throw new RangeError("quote value outside u64");
    view.setBigUint64(DOMAIN.length + 128 + 8 * i, v, true);
  });
  return out;
}

const hexBytes = (s: string) => Uint8Array.from(s.match(/../g) ?? [], (b) => parseInt(b, 16));

/** True only for a well-formed 128-hex signature by `dealer` over these terms. */
export async function verifyQuote(dealer: Address, signatureHex: unknown, t: SignedTerms): Promise<boolean> {
  if (typeof signatureHex !== "string" || !/^[0-9a-fA-F]{128}$/.test(signatureHex)) return false;
  try {
    const key = await getPublicKeyFromAddress(dealer);
    return await verifySignature(key, hexBytes(signatureHex) as SignatureBytes, quoteMessage(t));
  } catch {
    return false;
  }
}
