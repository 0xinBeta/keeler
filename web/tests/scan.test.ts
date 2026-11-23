// Which on-chain transactions count as this maker's: the order scan's writer check and maker
// check, and the cancel check that decides "refunded".
import {
  address,
  appendTransactionMessageInstruction,
  compileTransaction,
  createKeyPairSignerFromPrivateKeyBytes,
  createTransactionMessage,
  generateKeyPairSigner,
  getAddressEncoder,
  getTransactionEncoder,
  pipe,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
  signBytes,
  signTransaction,
  type Address,
  type Blockhash,
  type KeyPairSigner,
} from "@solana/kit";
import { LocalShieldedKeys, initializePoseidon } from "@heliuslabs/spr";
import { ed25519DerivationMessage } from "@heliuslabs/spr/keypair";
import { beforeAll, describe, expect, it } from "vitest";
import { cancelAccounts, cancelArgs, isMakersCancel, ixData, TAG, termsNameMaker, writerKey } from "../src/keeler/swap";

const PROGRAM = address("F4hjig5RJbXW6vrgYaMcuWzR55kZF6tbKyf8c36nrtvU");
const TREE = address("SysvarC1ock11111111111111111111111111111111");
const AUTHORITY = address("SysvarRent111111111111111111111111111111111");

async function keysFor(seedByte: number) {
  const signer = await createKeyPairSignerFromPrivateKeyBytes(new Uint8Array(32).fill(seedByte));
  const pub = new Uint8Array(await crypto.subtle.exportKey("raw", signer.keyPair.publicKey));
  const seed = new Uint8Array(await signBytes(signer.keyPair.privateKey, ed25519DerivationMessage(pub as never)));
  // The same keys the client derives (LocalKeys adds only a proof service, which these checks never use).
  return { signer, keys: LocalShieldedKeys.fromDerivationSeed({ solanaPublicKey: signer.address, derivationSeed: seed }) };
}

beforeAll(async () => {
  await initializePoseidon();
});

describe("the order scan", () => {
  it("takes a transaction as this maker's only if its viewing key re-derives from this maker's keys", async () => {
    const a = await keysFor(1);
    const b = await keysFor(2);
    const firstNullifier = new Uint8Array(32).fill(7) as never;
    const [bKey] = await b.keys.transactionKeys([{ viewingPublicKey: b.keys.address().viewingPublicKey, firstNullifier }]);
    const written = { txViewingPublicKey: bKey!.publicKey(), nullifiers: [firstNullifier] };
    expect(await writerKey(a.keys, written)).toBeUndefined();
    const own = await writerKey(b.keys, written);
    expect(own?.publicKey().toBytes()).toEqual(bKey!.publicKey().toBytes());
    expect(await writerKey(b.keys, { ...written, txViewingPublicKey: undefined })).toBeUndefined();
  });

  it("takes order terms only when they name this maker's Solana key and nullifier key", async () => {
    const a = await keysFor(1);
    const b = await keysFor(2);
    const terms = (who: Awaited<ReturnType<typeof keysFor>>) => ({
      key: { bytes: Object.fromEntries([...getAddressEncoder().encode(who.signer.address)].map((x, i) => [i, x])) },
      nullifierPk: BigInt(`0x${Buffer.from(who.keys.address().nullifierPublicKey).toString("hex")}`).toString(),
    });
    expect(termsNameMaker(terms(a), a.signer.address, a.keys.address().nullifierPublicKey)).toBe(true);
    expect(termsNameMaker(terms(b), a.signer.address, a.keys.address().nullifierPublicKey)).toBe(false);
    // Another maker's nullifier key under this maker's Solana key is still not this maker's order.
    expect(termsNameMaker({ ...terms(a), nullifierPk: terms(b).nullifierPk }, a.signer.address, a.keys.address().nullifierPublicKey)).toBe(false);
  });
});

/** A wire transaction with one swap-program instruction of `tag` whose maker account is `maker`, signed by `payer`. */
async function wire(payer: KeyPairSigner, maker: Address, tag: number) {
  const ix = {
    programAddress: PROGRAM,
    accounts: cancelAccounts(payer.address, maker, TREE, [], AUTHORITY),
    data: ixData(tag, new Uint8Array(128), cancelArgs(1n, 255), new Uint8Array()),
  };
  const message = pipe(
    createTransactionMessage({ version: 0 }),
    (m) => setTransactionMessageFeePayer(payer.address, m),
    (m) => setTransactionMessageLifetimeUsingBlockhash({ blockhash: "11111111111111111111111111111111" as Blockhash, lastValidBlockHeight: 0n }, m),
    (m) => appendTransactionMessageInstruction(ix, m),
  );
  const compiled = compileTransaction(message);
  const signed = maker === payer.address ? await signTransaction([payer.keyPair], compiled) : compiled;
  return new Uint8Array(getTransactionEncoder().encode(signed));
}

describe("a spend counts as a refund", () => {
  it("only when it is a cancel naming and signed by this maker", async () => {
    const me = await generateKeyPairSigner();
    const other = await generateKeyPairSigner();
    expect(isMakersCancel(await wire(me, me.address, TAG.cancel), PROGRAM, me.address)).toBe(true);
    expect(isMakersCancel(await wire(other, other.address, TAG.cancel), PROGRAM, me.address)).toBe(false);
    expect(isMakersCancel(await wire(me, me.address, 3), PROGRAM, me.address)).toBe(false);
    expect(isMakersCancel(await wire(me, me.address, TAG.cancel), address("11111111111111111111111111111111"), me.address)).toBe(false);
  });
});
