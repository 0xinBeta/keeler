// The make and cancel encoders against piece A's golden fixtures, built by
// the swap SDK's own Rust instruction builders (cli/tests/fixtures.rs).
import { existsSync, readFileSync } from "node:fs";
import { address, isSignerRole, isWritableRole, type Address } from "@solana/kit";
import { nullifierPdaAccounts } from "@heliuslabs/spr/internal";
import { describe, expect, it } from "vitest";
import { cancelAccounts, cancelArgs, ixData, makeAccounts, makeArgs, markerData, orderAuthority, TAG, toHex } from "../src/keeler/swap";

const hex = (s: string) => Uint8Array.from(s.match(/../g) ?? [], (b) => parseInt(b, 16));
const PROGRAM = address("F4hjig5RJbXW6vrgYaMcuWzR55kZF6tbKyf8c36nrtvU");

type Fixture = {
  inputs: Record<string, unknown> & {
    payer: string;
    tree: string;
    transact: { wincode_hex: string; inputs: { nullifier_hash: string }[] };
  };
  derived: { authority_bump: number; order_authority: string };
  instruction: { program_id: string; data_hex: string; accounts: { pubkey: string; is_signer: boolean; is_writable: boolean }[] };
};

const load = (name: string): Fixture | undefined => {
  const url = new URL(`./fixtures/${name}`, import.meta.url);
  return existsSync(url) ? (JSON.parse(readFileSync(url, "utf8")) as Fixture) : undefined;
};

const proof = (p: unknown) => {
  const { a, b, c } = p as { a: string; b: string; c: string };
  return hex(a + b + c);
};

const accounts = (list: { address: Address; role: Parameters<typeof isSignerRole>[0] }[]) =>
  list.map((m) => ({ pubkey: m.address, is_signer: isSignerRole(m.role), is_writable: isWritableRole(m.role) }));

const pdas = async (f: Fixture) =>
  (await nullifierPdaAccounts(address(f.inputs.tree), f.inputs.transact.inputs.map((i) => hex(i.nullifier_hash) as never))).map((m) => m.address);

const make = load("make-ix.json");
const cancel = load("cancel-ix.json");

describe.skipIf(!make)("make instruction (golden)", () => {
  const f = make!;
  it("derives the order authority and bump", async () => {
    const a = await orderAuthority(address(f.inputs.payer), PROGRAM);
    expect([a.pda, a.bump]).toEqual([f.derived.order_authority, f.derived.authority_bump]);
  });
  it("encodes the data byte for byte", () => {
    const data = ixData(TAG.make, proof(f.inputs.make_proof), makeArgs(hex(f.inputs.viewing_pubkey as string), f.derived.authority_bump), hex(f.inputs.transact.wincode_hex));
    expect(toHex(data)).toBe(f.instruction.data_hex);
    expect(f.instruction.program_id).toBe(PROGRAM);
  });
  it("lists the accounts in order with their roles", async () => {
    const list = makeAccounts(address(f.inputs.payer), address(f.inputs.tree), await pdas(f), address(f.derived.order_authority));
    expect(accounts(list)).toEqual(f.instruction.accounts);
  });
});

describe.skipIf(!cancel)("cancel instruction (golden)", () => {
  const f = cancel!;
  it("encodes the data byte for byte", () => {
    const data = ixData(TAG.cancel, proof(f.inputs.cancel_proof), cancelArgs(BigInt(f.inputs.order_expiry as number), f.derived.authority_bump), hex(f.inputs.transact.wincode_hex));
    expect(toHex(data)).toBe(f.instruction.data_hex);
  });
  it("lists the accounts in order with their roles", async () => {
    const list = cancelAccounts(address(f.inputs.payer), address(f.inputs.maker as string), address(f.inputs.tree), await pdas(f), address(f.derived.order_authority));
    expect(accounts(list)).toEqual(f.instruction.accounts);
  });
});

describe("encoders", () => {
  it("refuse a proof that is not 128 bytes", () => {
    expect(() => ixData(TAG.make, new Uint8Array(127), new Uint8Array(), new Uint8Array())).toThrow();
  });
  it("lay out the cancel args as u64 LE then the bump", () => {
    expect(toHex(cancelArgs(0x0102030405060708n, 254))).toBe("0807060504030201fe");
  });
  it("refuse a viewing key that is not 33 bytes", () => {
    expect(() => makeArgs(new Uint8Array(32), 1)).toThrow();
  });
  it("write the marker as order hash then maker", () => {
    const m = markerData(new Uint8Array(32).fill(7), address("11111111111111111111111111111111"));
    expect(toHex(m)).toBe("07".repeat(32) + "00".repeat(32));
  });
});
