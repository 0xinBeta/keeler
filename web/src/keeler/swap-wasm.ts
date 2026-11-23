// The page side of prover.worker.ts. Served layout under /swap/ (built by
// scripts/setup.sh, git-ignored):
//   swap_program.js, swap_program_bg.wasm      the swap program's WASM package
//   make/{circuit.xbc,pk.bin}, cancel/{circuit.xbc,pk.bin}
export type Circuit = "make" | "cancel";

const hex = (s: string) => Uint8Array.from(s.match(/../g)!, (b) => parseInt(b, 16));

/** SHA-256 of each proving key, as `swap-keys.CHECKSUM` pins them. */
export const PINNED_KEYS: Record<Circuit, { sha256: Uint8Array; insecureTestSetup: boolean }> = {
  make: { sha256: hex("f8c9feb479316980af604735f98df4ec89ce421d75c95fa4b4862dd957d6492e"), insecureTestSetup: true },
  cancel: { sha256: hex("866146bc05017f3c8adf3ea3923e4b9dcf20a5003b8bc4d6383020c8d18181f2"), insecureTestSetup: true },
};

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
