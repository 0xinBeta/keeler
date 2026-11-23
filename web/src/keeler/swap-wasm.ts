// The page side of prover.worker.ts. Served layout under /swap/ (built by
// scripts/setup.sh, git-ignored):
//   swap_program.js, swap_program_bg.wasm      the swap program's WASM package
//   make/{circuit.xbc,pk.bin,vk.bin}, cancel/{circuit.xbc,pk.bin,vk.bin}
//   proving-keys.json                          also bundled: the pins below
export type Circuit = "make" | "cancel";

// Bundled at build time from the same setup that produced the keys, so a key
// swapped on the server later fails `checkProvingKey`.
import provingKeys from "../../public/swap/proving-keys.json";

/** Lowercase hex SHA-256 of `circuit.xbc` and `vk.bin` per circuit, hashed by vite.config.ts at build time. */
declare const __KEELER_SWAP_ARTIFACTS__: Record<string, { xbc: string; vk: string }> | undefined;
const bundledArtifacts = () => (typeof __KEELER_SWAP_ARTIFACTS__ === "undefined" ? {} : __KEELER_SWAP_ARTIFACTS__);

type Pin = { sha256: Uint8Array; insecureTestSetup?: boolean };
type KeyPins = Record<string, { provingKeySha256: number[]; insecureTestSetup?: boolean }>;

export function pinnedKey(c: Circuit, keys: KeyPins = provingKeys): Pin {
  const k = keys[c];
  if (!k || k.provingKeySha256.length !== 32) throw new Error(`the build has no proving-key pin for ${c}`);
  return { sha256: Uint8Array.from(k.provingKeySha256), ...(k.insecureTestSetup === undefined ? {} : { insecureTestSetup: k.insecureTestSetup }) };
}

/** The circuit and verifying-key digests this build pinned for `c`. */
export function pinnedArtifacts(c: Circuit, artifacts: Record<string, { xbc: string; vk: string }> = bundledArtifacts()) {
  const a = artifacts[c];
  const hex64 = (s: unknown) => typeof s === "string" && /^[0-9a-f]{64}$/.test(s);
  if (!a || !hex64(a.xbc) || !hex64(a.vk)) throw new Error(`the build has no circuit and verifying-key pins for ${c}`);
  return a;
}

/**
 * Proving refuses a build without a network, and on mainnet any key not marked
 * `insecureTestSetup: false` (a missing flag fails): only a real ceremony's key may prove there.
 */
export function checkPinForNetwork(c: Circuit, pin: Pin, network: string | undefined) {
  if (!network) throw new Error("this site was built without VITE_NETWORK, so it will not prove");
  if (network === "mainnet" && pin.insecureTestSetup !== false) {
    throw new Error(`the ${c} proving key is not marked as coming from a real setup ceremony; it may not be used on mainnet`);
  }
}

export type WorkerRequest =
  | { op: "make" | "cancel"; inputs: unknown; sender: Uint8Array; payer: string }
  | { op: "orderTerms"; utxo: unknown }
  | { op: "prove"; circuit: Circuit; proofInputs: unknown; publicHash: Uint8Array };

export type WorkerReply = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: string };

/** What the WASM `<program>Transaction` functions return. */
export type RawProgramTransaction = {
  finalizedTx: { outputUtxos: RawOutput[]; outputHashes: Uint8Array[] } & Record<string, unknown>;
  proofInputs: Record<string, string>;
  publicHash: Uint8Array;
};
export type RawOutput = {
  asset: { asset: string; assetId: bigint };
  amount: bigint;
  blinding: Uint8Array;
  dataHash?: Uint8Array;
  data?: { records: { kind: string; bytes: Uint8Array }[] };
} & Record<string, unknown>;

let worker: Worker | undefined;
let next = 0;
const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();

/** One Web Worker for the page's lifetime; requests run one after another in it. */
export function callWorker<T>(req: WorkerRequest): Promise<T> {
  if (!worker) {
    worker = new Worker(new URL("./prover.worker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (e: MessageEvent<WorkerReply>) => {
      const p = pending.get(e.data.id);
      pending.delete(e.data.id);
      if (e.data.ok) p?.resolve(e.data.value);
      else p?.reject(new Error(e.data.error));
    };
    worker.onerror = (e) => {
      for (const p of pending.values()) p.reject(new Error(`prover worker failed: ${e.message}`));
      pending.clear();
      worker = undefined;
    };
  }
  const id = next++;
  return new Promise<T>((resolve, reject) => {
    pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
    worker!.postMessage({ id, req });
  });
}
