// The page side of prover.worker.ts. Served layout under /swap/ (built by
// scripts/setup.sh, git-ignored):
//   swap_program.js, swap_program_bg.wasm      the swap program's WASM package
//   make/{circuit.xbc,pk.bin}, cancel/{circuit.xbc,pk.bin}
//   proving-keys.json                          also bundled: the pins below
export type Circuit = "make" | "cancel";

// Bundled at build time from the same setup that produced the keys, so a key
// swapped on the server later fails `checkProvingKey`.
import provingKeys from "../../public/swap/proving-keys.json";

type Pin = { sha256: Uint8Array; insecureTestSetup: boolean };

export function pinnedKey(c: Circuit, keys: Record<string, { provingKeySha256: number[]; insecureTestSetup: boolean }> = provingKeys): Pin {
  const k = keys[c];
  if (!k || k.provingKeySha256.length !== 32) throw new Error(`the build has no proving-key pin for ${c}`);
  return { sha256: Uint8Array.from(k.provingKeySha256), insecureTestSetup: k.insecureTestSetup };
}

/** On mainnet only a key from a real setup ceremony may prove. */
export function checkPinForNetwork(c: Circuit, pin: Pin, network: string) {
  if (network === "mainnet" && pin.insecureTestSetup) {
    throw new Error(`the ${c} proving key comes from an insecure test setup; it may not be used on mainnet`);
  }
}

export type WorkerRequest =
  | { op: "make" | "cancel"; inputs: unknown; sender: Uint8Array; payer: string }
  | { op: "orderTerms"; utxo: unknown }
  | { op: "prove"; circuit: Circuit; network: string; proofInputs: unknown; publicHash: Uint8Array };

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
