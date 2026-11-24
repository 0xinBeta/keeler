// The swap and lend WASM packages run here, single-threaded, so proving never blocks the page.
// Files come from /swap/ and /lend/ (see swap-wasm.ts); the proving-key digests are pinned
// in the bundle, never read from the server that serves the keys.
import { BUILD_NETWORK } from "./network";
import { checkPinForNetwork, isLendCircuit, pinnedArtifacts, pinnedKey, type Circuit, type WorkerRequest, type WorkerReply } from "./swap-wasm";

type Common = {
  default: (opts: { module_or_path: string }) => Promise<unknown>;
  checkProvingKey(pk: Uint8Array, sha256: Uint8Array): void;
  sha256Hex(bytes: Uint8Array): string;
  verify(vk: Uint8Array, proof: Uint8Array, publicInputs: Uint8Array): boolean;
  prove(xbc: Uint8Array, pk: Uint8Array, inputs: unknown): { proof: Uint8Array; publicInputs: Uint8Array; free(): void };
  programProof(proof: Uint8Array, publicInputs: Uint8Array, expectedHash: Uint8Array): {
    compressedProof: { a: Uint8Array; b: Uint8Array; c: Uint8Array };
  };
};
type Build = (inputs: unknown, sender: Uint8Array, payer: string) => unknown;
type SwapWasm = Common & { makeTransaction: Build; cancelTransaction: Build; orderTermsDataUtxo(utxo: unknown): unknown };
type LendWasm = Common & {
  intentTransaction: Build;
  cancelIntentTransaction: Build;
  /** Not in every build of the package: the close circuit comes later. */
  closeTransaction?: Build;
  receiptStateDataUtxo(utxo: unknown): unknown;
  positionStateDataUtxo(utxo: unknown): unknown;
  intentStateDataUtxo(utxo: unknown): unknown;
  noteOutputHash(owner: Uint8Array, mint: Uint8Array, amount: bigint, kind: number, state: Uint8Array, reserve: Uint8Array | undefined, blinding: Uint8Array, treeId: number): Uint8Array;
  outputBlinding(firstNullifier: Uint8Array, seed: Uint8Array, slot: number): Uint8Array;
};

const packages = { swap: { dir: "/swap/", name: "swap_program" }, lend: { dir: "/lend/", name: "keeler_lend" } } as const;
type Pkg = keyof typeof packages;
const baseOf = (p: Pkg) => new URL(packages[p].dir, self.location.origin).href;
const loaded = new Map<Pkg, Promise<Common>>();
const artifacts = new Map<Circuit, Promise<{ xbc: Uint8Array; pk: Uint8Array; vk: Uint8Array }>>();

function load<T extends Common>(p: Pkg): Promise<T> {
  let got = loaded.get(p);
  if (!got) {
    got = (async () => {
      const base = baseOf(p);
      const mod = (await import(/* @vite-ignore */ `${base}${packages[p].name}.js`)) as Common;
      await mod.default({ module_or_path: `${base}${packages[p].name}_bg.wasm` });
      return mod;
    })();
    got.catch(() => loaded.delete(p));
    loaded.set(p, got);
  }
  return got as Promise<T>;
}

async function bytes(p: Pkg, path: string): Promise<Uint8Array> {
  const res = await fetch(baseOf(p) + path);
  if (!res.ok) throw new Error(`${packages[p].dir}${path}: HTTP ${res.status}`);
  return new Uint8Array(await res.arrayBuffer());
}

function circuitFiles(m: Common, c: Circuit) {
  const pin = pinnedKey(c);
  checkPinForNetwork(c, pin, BUILD_NETWORK);
  const digests = pinnedArtifacts(c);
  const p: Pkg = isLendCircuit(c) ? "lend" : "swap";
  let got = artifacts.get(c);
  if (!got) {
    got = Promise.all([bytes(p, `${c}/circuit.xbc`), bytes(p, `${c}/pk.bin`), bytes(p, `${c}/vk.bin`)]).then(([xbc, pk, vk]) => {
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
  switch (req.op) {
    case "make":
      return (await load<SwapWasm>("swap")).makeTransaction(req.inputs, req.sender, req.payer);
    case "cancel":
      return (await load<SwapWasm>("swap")).cancelTransaction(req.inputs, req.sender, req.payer);
    case "orderTerms":
      return (await load<SwapWasm>("swap")).orderTermsDataUtxo(req.utxo);
    case "lendIntent":
      return (await load<LendWasm>("lend")).intentTransaction(req.inputs, req.sender, req.payer);
    case "lendCancel":
      return (await load<LendWasm>("lend")).cancelIntentTransaction(req.inputs, req.sender, req.payer);
    case "lendClose": {
      const m = await load<LendWasm>("lend");
      if (typeof m.closeTransaction !== "function") {
        throw new Error("this site's lending package cannot build a close yet (it has no closeTransaction); closing a position is not available");
      }
      return m.closeTransaction(req.inputs, req.sender, req.payer);
    }
    case "lendState": {
      const m = await load<LendWasm>("lend");
      const decode = { receipt: m.receiptStateDataUtxo, position: m.positionStateDataUtxo, intent: m.intentStateDataUtxo }[req.kind];
      return decode(req.utxo);
    }
    case "lendNoteHash":
      return (await load<LendWasm>("lend")).noteOutputHash(req.owner, req.mint, req.amount, req.kind, req.state, undefined, req.blinding, req.treeId);
    case "lendBlinding":
      return (await load<LendWasm>("lend")).outputBlinding(req.firstNullifier, req.seed, req.slot);
    case "prove": {
      const m = await load<Common>(isLendCircuit(req.circuit) ? "lend" : "swap");
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
