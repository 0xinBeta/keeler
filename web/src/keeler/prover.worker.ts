// The swap WASM runs here, single-threaded, so proving never blocks the page.
// Files come from /swap/ (see swap-wasm.ts); the proving-key digests are pinned
// in the bundle, never read from the server that serves the keys.
import { BUILD_NETWORK } from "./network";
import { checkPinForNetwork, pinnedArtifacts, pinnedKey, type Circuit, type WorkerRequest, type WorkerReply } from "./swap-wasm";

type Wasm = {
  default: (opts: { module_or_path: string }) => Promise<unknown>;
  makeTransaction(inputs: unknown, sender: Uint8Array, payer: string): unknown;
  cancelTransaction(inputs: unknown, sender: Uint8Array, payer: string): unknown;
  orderTermsDataUtxo(utxo: unknown): unknown;
  checkProvingKey(pk: Uint8Array, sha256: Uint8Array): void;
  sha256Hex(bytes: Uint8Array): string;
  verify(vk: Uint8Array, proof: Uint8Array, publicInputs: Uint8Array): boolean;
  prove(xbc: Uint8Array, pk: Uint8Array, inputs: unknown): { proof: Uint8Array; publicInputs: Uint8Array; free(): void };
  programProof(proof: Uint8Array, publicInputs: Uint8Array, expectedHash: Uint8Array): {
    compressedProof: { a: Uint8Array; b: Uint8Array; c: Uint8Array };
  };
};

const base = new URL("/swap/", self.location.origin).href;
let wasm: Promise<Wasm> | undefined;
const artifacts = new Map<Circuit, Promise<{ xbc: Uint8Array; pk: Uint8Array; vk: Uint8Array }>>();

function load(): Promise<Wasm> {
  wasm ??= (async () => {
    const mod = (await import(/* @vite-ignore */ `${base}swap_program.js`)) as Wasm;
    await mod.default({ module_or_path: `${base}swap_program_bg.wasm` });
    return mod;
  })();
  return wasm;
}

async function bytes(path: string): Promise<Uint8Array> {
  const res = await fetch(base + path);
  if (!res.ok) throw new Error(`${path}: HTTP ${res.status}`);
  return new Uint8Array(await res.arrayBuffer());
}

function circuitFiles(m: Wasm, c: Circuit) {
  const pin = pinnedKey(c);
  checkPinForNetwork(c, pin, BUILD_NETWORK);
  const digests = pinnedArtifacts(c);
  let got = artifacts.get(c);
  if (!got) {
    got = Promise.all([bytes(`${c}/circuit.xbc`), bytes(`${c}/pk.bin`), bytes(`${c}/vk.bin`)]).then(([xbc, pk, vk]) => {
      if (m.sha256Hex(xbc) !== digests.xbc) throw new Error(`${c}/circuit.xbc differs from the circuit this site was built with`);
      if (m.sha256Hex(vk) !== digests.vk) throw new Error(`${c}/vk.bin differs from the verifying key this site was built with`);
      m.checkProvingKey(pk, pin.sha256);
      return { xbc, pk, vk };
    });
    got.catch(() => artifacts.delete(c));
    artifacts.set(c, got);
  }
  return got;
}

async function handle(req: WorkerRequest): Promise<unknown> {
  const m = await load();
  switch (req.op) {
    case "make":
      return m.makeTransaction(req.inputs, req.sender, req.payer);
    case "cancel":
      return m.cancelTransaction(req.inputs, req.sender, req.payer);
    case "orderTerms":
      return m.orderTermsDataUtxo(req.utxo);
    case "prove": {
      const { xbc, pk, vk } = await circuitFiles(m, req.circuit);
      const started = performance.now();
      const bundle = m.prove(xbc, pk, req.proofInputs);
      try {
        // As the Rust prover's proof.rs: nothing is sent unless the pinned verifying key accepts it.
        if (!m.verify(vk, bundle.proof, bundle.publicInputs)) throw new Error(`the ${req.circuit} proof does not verify against the pinned verifying key`);
        // Binds the proof's single public input to the transaction's public hash.
        const { a, b, c } = m.programProof(bundle.proof, bundle.publicInputs, req.publicHash).compressedProof;
        const proof = new Uint8Array(a.length + b.length + c.length);
        proof.set(a);
        proof.set(b, a.length);
        proof.set(c, a.length + b.length);
        return { proof, ms: performance.now() - started };
      } finally {
        bundle.free();
      }
    }
  }
}

self.onmessage = async (e: MessageEvent<{ id: number; req: WorkerRequest }>) => {
  const { id, req } = e.data;
  let reply: WorkerReply;
  try {
    reply = { id, ok: true, value: await handle(req) };
  } catch (err) {
    reply = { id, ok: false, error: err instanceof Error ? err.message : String(err) };
  }
  self.postMessage(reply);
};
