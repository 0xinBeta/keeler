// Session authorisation, request signatures and keeper tickets (docs/lend-api.md). The committed fixture is
// regenerated from fixed keys and must match byte for byte; UPDATE_FIXTURES=1 rewrites it.
import { readFileSync, writeFileSync } from "node:fs";
import { address, createKeyPairFromPrivateKeyBytes, getAddressFromPublicKey, signBytes } from "@solana/kit";
import { ed25519DerivationMessage } from "@heliuslabs/spr/keypair";
import { describe, expect, it } from "vitest";
import {
  authorisationMessage,
  authorisationText,
  canonical,
  hex,
  requestMessage,
  sessionKey,
  sessionSeed,
  signRequest,
  ticketMessage,
  verifyRequest,
  verifyTicket,
  type Fields,
  type Session,
} from "../src/keeler/lend-auth";

const FIXTURE = new URL("./fixtures/lend-requests.json", import.meta.url);
const NETWORK = "devnet";
const PROGRAM = "A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC";
const NOW = 1_800_000_000;
const ORIGIN = "https://keeler.test";
const POOL = "CktRuQ2mttgRGkXJtyksdKHjUdc2C4TgDzyB98oEzy8";

async function walletKeys(byte: number) {
  const seed = new Uint8Array(32).fill(byte);
  const keys = await createKeyPairFromPrivateKeyBytes(seed);
  return { seed, keys, address: await getAddressFromPublicKey(keys.publicKey) };
}

async function generate() {
  const wallet = await walletKeys(0x11);
  const keeper = await walletKeys(0x22);
  const pub = new Uint8Array(await crypto.subtle.exportKey("raw", wallet.keys.publicKey));
  const derivation = new Uint8Array(await signBytes(wallet.keys.privateKey, ed25519DerivationMessage(pub as never)));
  const seed = await sessionSeed(derivation);
  const sk = await sessionKey(seed);
  const expiry = NOW + 86_000;
  const auth = { network: NETWORK, origin: ORIGIN, programId: PROGRAM, wallet: wallet.address, sessionKey: sk.key, expiry };
  const session: Session = { ...sk, expiry, origin: ORIGIN, auth: hex(new Uint8Array(await signBytes(wallet.keys.privateKey, authorisationMessage(auth)))) };
  const ticket = { programId: address(PROGRAM), wallet: wallet.address, id: "ab".repeat(16), purpose: 1, target: 1n, amount: 250_000000n, expiry: NOW + 300 };
  const ticketSignature = hex(new Uint8Array(await signBytes(keeper.keys.privateKey, ticketMessage(ticket))));
  // The ticket as the page holds it and sends it with account reads: the keeper's reply and the terms asked for.
  const held = { id: ticket.id, expiry: ticket.expiry, signature: ticketSignature, keeper: keeper.address, purpose: "supply", amount: "250000000", pool: POOL };
  const nonce = (b: number) => new Uint8Array(16).fill(b);
  const requests = [];
  for (const [path, body, unsigned, n] of [
    ["/lend/account", {}, { tickets: [held] }, 1],
    ["/lend/ticket", { purpose: "supply", amount: "250000000", pool: POOL }, {}, 2],
    ["/lend/build", { op: "draw", vault: 1, position: "7", borrow: "100000000", withdraw: "0" }, {}, 3],
  ] as const) {
    const signed = await signRequest(session, wallet.address, "POST", path, body, NOW, nonce(n));
    requests.push({ method: "POST" as const, path, fields: { ...signed, ...unsigned }, canonical: canonical(signed), signed_message_hex: hex(await requestMessage("POST", path, signed)) });
  }
  return {
    about:
      "Signed keeper requests from fixed keys (docs/lend-api.md). wallet and keeper are Ed25519 keys from the 32-byte seeds below. session_seed = SHA-256(\"keeler-lend-session-v1\" || derivation_signature), where derivation_signature is the wallet's signature over the SPR key-derivation message. sessionAuth is the wallet's signature over authorisation_text (UTF-8), which names origin. Each request's fields are its whole JSON body; its signature is the session key's over signed_message_hex, whose canonical part leaves out signature, nonce, timestamp and tickets (an account read's open tickets, each signed by its keeper). The ticket signature is the keeper's over its message_hex; ticket.held is the ticket as the page sends it back.",
    network: NETWORK,
    origin: ORIGIN,
    program_id: PROGRAM,
    now: NOW,
    wallet: { seed_hex: hex(wallet.seed), address: wallet.address, derivation_signature_hex: hex(derivation) },
    session: { seed_hex: hex(seed), key: sk.key, expiry, authorisation_text: authorisationText(auth), auth_hex: session.auth },
    requests,
    ticket: {
      keeper_seed_hex: hex(keeper.seed),
      keeper: keeper.address,
      fields: { ...ticket, target: ticket.target.toString(), amount: ticket.amount.toString() },
      message_hex: hex(ticketMessage(ticket)),
      signature_hex: ticketSignature,
      held,
    },
  };
}

describe("keeper request authentication", () => {
  it("matches the committed fixture", async () => {
    const fresh = JSON.parse(JSON.stringify(await generate()));
    if (process.env.UPDATE_FIXTURES === "1") writeFileSync(FIXTURE, JSON.stringify(fresh, null, 2) + "\n");
    expect(JSON.parse(readFileSync(FIXTURE, "utf8"))).toEqual(fresh);
  });

  it("verifies each request, and nothing altered", async () => {
    const f = await generate();
    for (const r of f.requests) {
      const fields = r.fields as Fields;
      expect(await verifyRequest(NETWORK, PROGRAM, r.method, r.path, fields)).toBe(true);
      expect(await verifyRequest("mainnet", PROGRAM, r.method, r.path, fields)).toBe(false);
      expect(await verifyRequest(NETWORK, PROGRAM, "GET", r.path, fields)).toBe(false);
      expect(await verifyRequest(NETWORK, PROGRAM, r.method, "/lend/other", fields)).toBe(false);
      // Every field but the signature and the tickets (each signed by its keeper) is covered.
      for (const k of Object.keys(fields).filter((k) => k !== "signature" && k !== "tickets")) {
        const v = fields[k]!;
        const changed = typeof v === "number" ? v + 1 : k === "nonce" ? "00".repeat(16) : `${String(v).slice(0, -1)}${String(v).endsWith("1") ? "2" : "1"}`;
        expect(await verifyRequest(NETWORK, PROGRAM, r.method, r.path, { ...fields, [k]: changed }), k).toBe(false);
      }
    }
    expect(f.requests[0]!.fields).toHaveProperty("tickets", [f.ticket.held]);
    expect(await verifyRequest(NETWORK, PROGRAM, "POST", "/lend/account", { ...(f.requests[0]!.fields as Fields), tickets: [] as never })).toBe(true);
  });

  it("puts the origin and what the keeper sees right after the network", async () => {
    const f = await generate();
    expect(f.session.authorisation_text.split("\n").slice(4, 8)).toEqual([
      "Network: devnet",
      "Origin: https://keeler.test",
      "The keeper can see your lending amounts.",
      `Program: ${PROGRAM}`,
    ]);
    for (const r of f.requests) expect((r.fields as Fields).origin).toBe(ORIGIN);
    expect(await verifyRequest(NETWORK, PROGRAM, "POST", "/lend/ticket", { ...(f.requests[1]!.fields as Fields), origin: "https://evil.test" })).toBe(false);
  });

  it("derives the same session key from the same signature, and another from another", async () => {
    const a = await sessionKey(await sessionSeed(new Uint8Array(64).fill(1)));
    const b = await sessionKey(await sessionSeed(new Uint8Array(64).fill(1)));
    const c = await sessionKey(await sessionSeed(new Uint8Array(64).fill(2)));
    expect(a.key).toBe(b.key);
    expect(c.key).not.toBe(a.key);
  });

  it("names everything the wallet authorises in plain text", () => {
    const t = authorisationText({ network: "devnet", origin: ORIGIN, programId: PROGRAM, wallet: "W", sessionKey: "S", expiry: NOW });
    expect(t).toContain("The keeper can see your lending amounts.");
    expect(t).toContain("Program: A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC");
    expect(t).toContain("Session key: S");
    expect(t).toContain("Expires: 1800000000 (2027-01-15T08:00:00Z)");
    expect(t).toContain("It cannot move funds");
  });

  it("refuses fields that could be read two ways", () => {
    expect(() => canonical({ a: "x\ny=1" })).toThrow();
    expect(() => canonical({ a: -1 })).toThrow();
    expect(canonical({ b: 1, a: "x", nonce: "n", timestamp: 2, signature: "s" })).toBe("a=x\nb=1\n");
  });

  it("verifies a keeper ticket only for its exact terms", async () => {
    const f = await generate();
    const t = { ...f.ticket.fields, programId: address(PROGRAM), wallet: address(f.wallet.address), target: 1n, amount: 250_000000n };
    expect(ticketMessage(t).length).toBe(21 + 32 + 32 + 16 + 1 + 24);
    expect(await verifyTicket(address(f.ticket.keeper), f.ticket.signature_hex, t)).toBe(true);
    expect(await verifyTicket(address(f.ticket.keeper), f.ticket.signature_hex, { ...t, amount: 250_000001n })).toBe(false);
    expect(await verifyTicket(address(f.wallet.address), f.ticket.signature_hex, t)).toBe(false);
    expect(await verifyTicket(address(f.ticket.keeper), "zz", t)).toBe(false);
  });
});
