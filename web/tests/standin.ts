// Test-only stand-in for the Worker's /api routes on the local network
// (the real Worker is worker/). Reads tests/.localnet/localnet.json, written by
// tests/localnet. Speaks the dealer's TCP line protocol for /api/quotes.
import { readFileSync } from "node:fs";
import { connect } from "node:net";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Plugin } from "vite";
import {
  address,
  appendTransactionMessageInstructions,
  createKeyPairSignerFromBytes,
  createSolanaRpc,
  createTransactionMessage,
  getSignatureFromTransaction,
  pipe,
  sendTransactionWithoutConfirmingFactory,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
} from "@solana/kit";
import { getTransferSolInstruction } from "@solana-program/system";
import { findAssociatedTokenPda, getCreateAssociatedTokenIdempotentInstructionAsync, getTransferInstruction, TOKEN_PROGRAM_ADDRESS } from "@solana-program/token";

export const PORTS = { rpc: 10899, photon: 10784, prover: 3101 };
export const SWAP_PROGRAM_ID = "F4hjig5RJbXW6vrgYaMcuWzR55kZF6tbKyf8c36nrtvU";
export const SOL = "11111111111111111111111111111111";
const FAUCET_SOL = 2_000_000_000n;
const FAUCET_TOKENS = 300_000_000n;

type Localnet = {
  rpcUrl: string;
  splMint: string;
  prices: Record<string, number>;
  payerSecret: number[];
  dealer: { pubkey: string; listen: string };
  idleDealer: { pubkey: string };
};
const info = (): Localnet =>
  JSON.parse(readFileSync(new URL("./.localnet/localnet.json", import.meta.url), "utf8")) as Localnet;

/** `POST /__test/idle {"on": bool}`: the idle dealer outbids the real one by one unit and never settles. */
let idleQuotes = false;

async function body(req: IncomingMessage): Promise<string> {
  let text = "";
  for await (const chunk of req) text += chunk;
  return text;
}

function send(res: ServerResponse, status: number, value: unknown) {
  res.statusCode = status;
  res.setHeader("content-type", "application/json");
  res.end(JSON.stringify(value));
}

/** One request line to the dealer, one reply line back, as `core/src/rfq.rs` does. */
function askDealer(listen: string, rfq: { sell_mint: string; buy_mint: string; sell_amount: string; trader: string }) {
  if (!/^\d+$/.test(rfq.sell_amount)) return Promise.resolve(null);
  // Amounts are u64 JSON numbers on the TCP protocol and strings over HTTP.
  const line = JSON.stringify({ ...rfq, sell_amount: "@@" }).replace('"@@"', rfq.sell_amount) + "\n";
  const [host, port] = listen.split(":");
  return new Promise<{ buy_amount: string; expiry: number } | null>((resolve) => {
    const socket = connect(Number(port), host!);
    let got = "";
    const done = (v: { buy_amount: string; expiry: number } | null) => {
      socket.destroy();
      resolve(v);
    };
    socket.setTimeout(3000, () => done(null));
    socket.on("error", () => done(null));
    socket.on("connect", () => socket.write(line));
    socket.on("data", (d) => {
      got += d;
      if (!got.includes("\n")) return;
      const m = /"buy_amount":(\d+),"expiry":(\d+)/.exec(got);
      done(m ? { buy_amount: m[1]!, expiry: Number(m[2]) } : null);
    });
  });
}

async function faucet(wallet: string) {
  const net = info();
  const rpc = createSolanaRpc(net.rpcUrl);
  const payer = await createKeyPairSignerFromBytes(Uint8Array.from(net.payerSecret));
  const mint = address(net.splMint);
  const owner = address(wallet);
  const { value: accounts } = await rpc.getTokenAccountsByOwner(payer.address, { mint }, { encoding: "base64" }).send();
  const source = accounts[0]?.pubkey;
  if (!source) throw new Error("the fixture payer has no token account");
  const [destination] = await findAssociatedTokenPda({ owner, mint, tokenProgram: TOKEN_PROGRAM_ADDRESS });
  const { value: blockhash } = await rpc.getLatestBlockhash().send();
  const instructions = [
    getTransferSolInstruction({ source: payer, destination: owner, amount: FAUCET_SOL }),
    await getCreateAssociatedTokenIdempotentInstructionAsync({ payer, owner, mint }),
    getTransferInstruction({ source, destination, authority: payer, amount: FAUCET_TOKENS }),
  ];
  const tx = await signTransactionMessageWithSigners(
    pipe(
      createTransactionMessage({ version: 0 }),
      (m) => setTransactionMessageFeePayerSigner(payer, m),
      (m) => setTransactionMessageLifetimeUsingBlockhash(blockhash, m),
      (m) => appendTransactionMessageInstructions(instructions, m),
    ),
  );
  await sendTransactionWithoutConfirmingFactory({ rpc })(tx as never, { commitment: "confirmed" });
  const signature = getSignatureFromTransaction(tx as never);
  for (let i = 0; i < 100; i++) {
    const { value } = await rpc.getSignatureStatuses([signature]).send();
    const s = value[0];
    if (s?.err) throw new Error(`faucet transaction failed: ${JSON.stringify(s.err)}`);
    if (s?.confirmationStatus === "confirmed" || s?.confirmationStatus === "finalized") return;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("faucet transaction not confirmed");
}

export function standin(): Plugin {
  return {
    name: "keeler-test-standin",
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        try {
          const url = req.url ?? "";
          if (url === "/api/config" && req.method === "GET") {
            const net = info();
            return send(res, 200, {
              network: "local",
              rpcPath: "/api/rpc",
              sprPath: "/v1/spr",
              programId: SWAP_PROGRAM_ID,
              assets: [
                { mint: net.splMint, symbol: "tSPL", decimals: 9 },
                { mint: SOL, symbol: "SOL", decimals: 9 },
              ],
              dealers: [{ pubkey: net.dealer.pubkey }, { pubkey: net.idleDealer.pubkey }],
              bandBps: 50,
              minTtlSecs: 10,
              maxTtlSecs: 120,
              reference: Object.fromEntries(Object.entries(net.prices).map(([m, p]) => [m, { price: String(p), expo: 0 }])),
              faucet: true,
            });
          }
          if (url === "/api/quotes" && req.method === "POST") {
            const net = info();
            const rfq = JSON.parse(await body(req));
            const real = await askDealer(net.dealer.listen, rfq);
            const idle = idleQuotes && real ? { buy_amount: String(BigInt(real.buy_amount) + 1n), expiry: real.expiry } : null;
            return send(res, 200, [
              { dealer: net.dealer.pubkey, quote: real },
              { dealer: net.idleDealer.pubkey, quote: idle },
            ]);
          }
          if (url === "/api/faucet" && req.method === "POST") {
            const { wallet } = JSON.parse(await body(req));
            await faucet(wallet);
            return send(res, 200, { ok: true });
          }
          if (url === "/__test/idle" && req.method === "POST") {
            idleQuotes = Boolean(JSON.parse(await body(req)).on);
            return send(res, 200, { ok: true });
          }
        } catch (e) {
          return send(res, 500, { error: e instanceof Error ? e.message : String(e) });
        }
        next();
      });
    },
  };
}

/** Proxies to the local RPC, Photon and prover, as the Worker's /api/rpc and /v1/spr do. */
export const proxy = {
  "/api/rpc": { target: `http://127.0.0.1:${PORTS.rpc}`, rewrite: () => "/" },
  "^/v1/spr/(prove|health|attestation|proving-keys)": {
    target: `http://127.0.0.1:${PORTS.prover}`,
    rewrite: (p: string) => p.replace(/^\/v1\/spr/, ""),
  },
  "/v1/spr": { target: `http://127.0.0.1:${PORTS.photon}`, rewrite: (p: string) => p.replace(/^\/v1\/spr/, "") || "/" },
};
