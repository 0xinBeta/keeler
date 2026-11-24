// Authentication of the page's keeper requests and the keeper's tickets. The exact bytes are in docs/lend-api.md,
// and web/tests/fixtures/lend-requests.json carries signed examples the keeper's Rust tests verify.
import {
  createKeyPairFromPrivateKeyBytes,
  getAddressEncoder,
  getAddressFromPublicKey,
  getPublicKeyFromAddress,
  signBytes,
  verifySignature,
  type Address,
  type SignatureBytes,
} from "@solana/kit";
import { SESSION_MAX_SECS, u64Bytes } from "./lend-layout";

const utf8 = (s: string) => new TextEncoder().encode(s);
const concat = (...parts: Uint8Array[]) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
};
const sha256 = async (b: Uint8Array) => new Uint8Array(await crypto.subtle.digest("SHA-256", b as BufferSource));
export const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
export const fromHex = (s: string) => Uint8Array.from(s.match(/../g) ?? [], (b) => parseInt(b, 16));

/** Domain of the session key: no other Keeler or SPR derivation hashes this prefix. */
export const SESSION_DOMAIN = "keeler-lend-session-v1";
export const REQUEST_DOMAIN = "keeler-lend-request-v1";
export const TICKET_DOMAIN = "keeler-lend-ticket-v1";

/** The 32-byte Ed25519 seed of the session key: SHA-256(domain ‖ the wallet's 64-byte key-derivation signature). */
export const sessionSeed = (derivationSignature: Uint8Array) => sha256(concat(utf8(SESSION_DOMAIN), derivationSignature));

export type Session = { keys: CryptoKeyPair; key: Address; expiry: number; auth: string };

export async function sessionKey(seed: Uint8Array): Promise<{ keys: CryptoKeyPair; key: Address }> {
  const keys = await createKeyPairFromPrivateKeyBytes(seed);
  return { keys, key: await getAddressFromPublicKey(keys.publicKey) };
}

export type Authorisation = { network: string; programId: string; wallet: string; sessionKey: string; expiry: number };

/** What the wallet signs once per session: human-readable, LF line ends, UTF-8. */
export function authorisationText(a: Authorisation): string {
  return [
    "Keeler Lend: allow this browser session",
    "",
    "This lets this browser send lending requests for this wallet to the Keeler keeper until the time below, without asking you each time. It cannot move funds: every transaction still needs your wallet's approval.",
    "",
    `Network: ${a.network}`,
    `Program: ${a.programId}`,
    `Wallet: ${a.wallet}`,
    `Session key: ${a.sessionKey}`,
    `Expires: ${a.expiry} (${new Date(a.expiry * 1000).toISOString().replace(/\.\d{3}Z$/, "Z")})`,
  ].join("\n");
}
export const authorisationMessage = (a: Authorisation) => utf8(authorisationText(a));

/** A session expiry the keeper accepts: in the future and at most a day ahead of `now`. */
export const sessionExpiryFor = (now: number) => now + SESSION_MAX_SECS - 60;

export type Fields = Record<string, string | number>;
/** Fields covered by position in the signed bytes, not by the hash. */
const POSITIONAL = new Set(["signature", "nonce", "timestamp"]);

/** `key=value\n` for every other field, keys sorted, values as their decimal or string form. */
export function canonical(fields: Fields): string {
  return Object.keys(fields)
    .filter((k) => !POSITIONAL.has(k))
    .sort()
    .map((k) => {
      const v = fields[k]!;
      const s = typeof v === "number" ? (Number.isSafeInteger(v) && v >= 0 ? String(v) : bad(k)) : v;
      if (!/^[A-Za-z0-9_]+$/.test(k) || /[=\n]/.test(s)) bad(k);
      return `${k}=${s}\n`;
    })
    .join("");
}
const bad = (k: string): never => {
  throw new Error(`field ${k} cannot be signed`);
};

/** `domain "\n" method "\n" path "\n" ‖ nonce (16) ‖ timestamp u64 LE ‖ SHA-256(canonical fields)`. */
export async function requestMessage(method: "GET" | "POST", path: string, fields: Fields): Promise<Uint8Array> {
  const nonce = fromHex(String(fields.nonce));
  if (nonce.length !== 16) throw new Error("the nonce must be 16 bytes");
  return concat(utf8(`${REQUEST_DOMAIN}\n${method}\n${path}\n`), nonce, u64Bytes(BigInt(fields.timestamp!)), await sha256(utf8(canonical(fields))));
}

/** The request's fields with the session fields, a fresh nonce, `timestamp` and the session key's signature. */
export async function signRequest(s: Session, wallet: string, method: "GET" | "POST", path: string, body: Fields, timestamp: number, nonce = crypto.getRandomValues(new Uint8Array(16))): Promise<Fields> {
  const fields: Fields = { ...body, wallet, sessionKey: s.key, sessionExpiry: s.expiry, sessionAuth: s.auth, nonce: hex(nonce), timestamp };
  const signature = await signBytes(s.keys.privateKey, await requestMessage(method, path, fields));
  return { ...fields, signature: hex(signature) };
}

const verify = async (key: Address, signatureHex: unknown, message: Uint8Array) => {
  if (typeof signatureHex !== "string" || !/^[0-9a-f]{128}$/.test(signatureHex)) return false;
  try {
    return await verifySignature(await getPublicKeyFromAddress(key), fromHex(signatureHex) as SignatureBytes, message);
  } catch {
    return false;
  }
};

/** The keeper's checks, minus freshness and replay (docs/lend-api.md): for tests and the shared fixture. */
export async function verifyRequest(network: string, programId: string, method: "GET" | "POST", path: string, fields: Fields): Promise<boolean> {
  const auth = authorisationMessage({ network, programId, wallet: String(fields.wallet), sessionKey: String(fields.sessionKey), expiry: Number(fields.sessionExpiry) });
  return (
    (await verify(String(fields.wallet) as Address, fields.sessionAuth, auth)) &&
    (await verify(String(fields.sessionKey) as Address, fields.signature, await requestMessage(method, path, fields)))
  );
}

export type Ticket = { programId: Address; wallet: Address; id: string; purpose: number; target: bigint; amount: bigint; expiry: number };

/** `domain ‖ program (32) ‖ wallet (32) ‖ id (16) ‖ purpose u8 ‖ target u64 LE ‖ amount u64 LE ‖ expiry u64 LE` (125 bytes). */
export function ticketMessage(t: Ticket): Uint8Array {
  const enc = getAddressEncoder();
  const id = fromHex(t.id);
  if (id.length !== 16) throw new Error("a ticket id is 16 bytes");
  return concat(utf8(TICKET_DOMAIN), new Uint8Array(enc.encode(t.programId)), new Uint8Array(enc.encode(t.wallet)), id, Uint8Array.of(t.purpose), u64Bytes(t.target), u64Bytes(t.amount), u64Bytes(BigInt(t.expiry)));
}

export const verifyTicket = (keeper: Address, signatureHex: unknown, t: Ticket) => verify(keeper, signatureHex, ticketMessage(t));
