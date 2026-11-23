// Keeler Worker: serves web/dist and is the page's only network peer.
// Holds HELIUS_API_KEY and DEALER_KEY; neither ever reaches a response.

export interface Env {
  STATIC: Fetcher;
  HELIUS_API_KEY?: string;
  DEALER_KEY?: string;
  NETWORK: string;
  DEALERS?: unknown;
  UPSTREAM_RPC?: string;
  UPSTREAM_SPR?: string;
  UPSTREAM_INDEXER?: string;
  UPSTREAM_PROVER?: string;
  PROGRAM_ID?: string;
  ASSETS?: unknown;
  BAND_BPS?: string | number;
  MIN_TTL_SECS?: string | number;
  MAX_TTL_SECS?: string | number;
  REFERENCE?: unknown;
  FAUCET?: string;
  RPC_LIMIT?: RateLimit;
  QUOTE_LIMIT?: RateLimit;
  FAUCET_LIMIT?: RateLimit;
}

const RPC_BODY_CAP = 64 * 1024;
// 1 MiB plus the TEE envelope (nonce 12 + status 2 + GCM tag 16) the SDK allows on top.
const SPR_CAP = 1024 * 1024 + 64;
const SMALL_BODY_CAP = 4 * 1024;
const DEALER_TIMEOUT_MS = 3000;
const FAUCET_TIMEOUT_MS = 15000;
const U64_MAX = (1n << 64n) - 1n;
const BASE58 = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;

// @heliuslabs/spr 0.9.0-alpha: dist/client/client.js and dist/wallet/{sync,registry}.js call
// getAccountInfo, getProgramAccounts, getMultipleAccounts, getBalance, getLatestBlockhash,
// getSlot, getSignatureStatuses; the rest is what a wallet flow and chain time need.
const RPC_METHODS = new Set([
  "getAccountInfo", "getMultipleAccounts", "getProgramAccounts", "getLatestBlockhash",
  "getBalance", "getTokenAccountBalance", "getTokenAccountsByOwner", "getSignatureStatuses",
  "getTransaction", "sendTransaction", "simulateTransaction", "getSlot",
  "getMinimumBalanceForRentExemption", "getEpochInfo", "getBlockHeight", "getBlockTime",
]);

// dist/indexer/names.js: SprApi POSTs JSON-RPC to <indexer>/<method> (dist/api/index.js).
const INDEXER = /^\/(getEncryptedUtxosByTags|getShieldedTransactionsByTags|getShieldedTransactionsByNullifiers|getShieldedTransactionsBySignature|getMerkleProofs|getNonInclusionProofs|getUserRecords)$/;
// dist/client/prover/client.js: GET /attestation /health /proving-keys /prove/<key>/status,
// POST /prove/<key> and /prove/<key>/indexed; <key> is a key file name without ".key".
const PROVER: [string, RegExp][] = [
  ["GET", /^\/(attestation|health|proving-keys)$/],
  ["GET", /^\/prove\/[a-z0-9_-]{1,64}\/status$/],
  ["POST", /^\/prove\/[a-z0-9_-]{1,64}(\/indexed)?$/],
];
// dist/client/prover/tee/session.js sends these; X-Sync / X-Async pick sync or queued proving.
const SPR_REQUEST_HEADERS = ["content-type", "spr-tee", "spr-tee-enc", "spr-tee-ciphertext", "x-sync", "x-async"];
// Spr-Tee marks an encrypted reply; retry-after paces attestation retries.
const SPR_RESPONSE_HEADERS = ["content-type", "spr-tee", "retry-after"];

// dist/client/prover/tee/signed.js NETWORKS.
const TEE_POLICY: Record<string, string> = {
  devnet: "https://d3gbdb0egjwcw9.cloudfront.net/tee-policy/devnet.json",
  mainnet: "https://d3gbdb0egjwcw9.cloudfront.net/tee-policy/mainnet.json",
};
// dist/endpoint.js LOCALNET_* defaults.
const LOCAL = { rpc: "http://127.0.0.1:8899", indexer: "http://127.0.0.1:8784", prover: "http://127.0.0.1:3001" };

const CSP =
  "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; worker-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'";

class Fail extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
  }
}

const json = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const path = url.pathname;
    if (!path.startsWith("/api/") && !path.startsWith("/v1/")) return serveStatic(request, env);
    try {
      const origin = request.headers.get("origin");
      if (origin !== null && origin !== url.origin) throw new Fail(403, "Cross-origin requests are not allowed.");
      if (path === "/api/config") return only(request, "GET") ?? json(config(env));
      if (path === "/api/rpc") return only(request, "POST") ?? (await limit(env.RPC_LIMIT, request), await rpc(request, env));
      if (path === "/api/tee-policy") return only(request, "GET") ?? (await limit(env.RPC_LIMIT, request), await teePolicy(env));
      if (path === "/api/quotes") return only(request, "POST") ?? (await limit(env.QUOTE_LIMIT, request), await quotes(request, env));
      if (path === "/api/faucet") {
        if (env.FAUCET !== "1") throw new Fail(404, "Not found.");
        return only(request, "POST") ?? (await limit(env.FAUCET_LIMIT, request), await faucet(request, env));
      }
      if (path.startsWith("/v1/spr/")) return await spr(request, env, url);
      throw new Fail(404, "Not found.");
    } catch (error) {
      if (error instanceof Fail) return json({ error: error.message }, error.status);
      return json({ error: "Something went wrong in the server." }, 500);
    }
  },
};

function only(request: Request, method: string): Response | undefined {
  return request.method === method ? undefined : json({ error: `Use ${method} for this route.` }, 405);
}

// No binding (plain `wrangler dev` without ratelimits support) means no limit.
async function limit(limiter: RateLimit | undefined, request: Request): Promise<void> {
  if (limiter === undefined) return;
  const key = request.headers.get("cf-connecting-ip") ?? "unknown";
  const { success } = await limiter.limit({ key });
  if (!success) throw new Fail(429, "Too many requests. Wait a minute and try again.");
}

async function serveStatic(request: Request, env: Env): Promise<Response> {
  const upstream = await env.STATIC.fetch(request);
  const response = new Response(upstream.body, upstream);
  const h = response.headers;
  const path = new URL(request.url).pathname;
  h.set("x-content-type-options", "nosniff");
  h.set("referrer-policy", "no-referrer");
  if ((h.get("content-type") ?? "").startsWith("text/html")) h.set("content-security-policy", CSP);
  if (path.endsWith(".wasm")) h.set("content-type", "application/wasm");
  // Vite writes hashed files to /assets/name-<hash>.ext.
  if (response.ok && /^\/assets\/.+-[A-Za-z0-9_-]{8,}\.[a-z0-9]+$/.test(path)) {
    h.set("cache-control", "public, max-age=31536000, immutable");
  }
  return response;
}

// ---- config ----

function parsed(value: unknown, name: string): unknown {
  if (typeof value !== "string") return value;
  try {
    return JSON.parse(value);
  } catch {
    throw new Fail(500, `The server setting ${name} is not valid JSON.`);
  }
}

function dealers(env: Env): { url: string; pubkey: string }[] {
  const list = parsed(env.DEALERS ?? [], "DEALERS");
  if (!Array.isArray(list) || !list.every((d) => typeof d?.url === "string" && typeof d?.pubkey === "string")) {
    throw new Fail(500, "The server setting DEALERS is malformed.");
  }
  return list;
}

function config(env: Env) {
  return {
    network: env.NETWORK,
    rpcPath: "/api/rpc",
    sprPath: "/v1/spr",
    programId: env.PROGRAM_ID ?? null,
    assets: parsed(env.ASSETS ?? [], "ASSETS"),
    dealers: dealers(env).map((d) => ({ pubkey: d.pubkey })),
    bandBps: Number(env.BAND_BPS ?? 0),
    minTtlSecs: Number(env.MIN_TTL_SECS ?? 0),
    maxTtlSecs: Number(env.MAX_TTL_SECS ?? 0),
    reference: parsed(env.REFERENCE ?? {}, "REFERENCE"),
    faucet: env.FAUCET === "1",
  };
}

// ---- upstreams ----

function helius(env: Env, path: string): string {
  if (env.NETWORK !== "devnet" && env.NETWORK !== "mainnet") throw new Fail(503, "This network has no upstream configured.");
  return `https://${env.NETWORK}.helius-rpc.com${path}`;
}

function withKey(base: string, env: Env, query = ""): string {
  const pairs = query.split("&").filter((p) => p !== "" && !new URLSearchParams(p).has("api-key"));
  if (env.HELIUS_API_KEY) pairs.push(`api-key=${encodeURIComponent(env.HELIUS_API_KEY)}`);
  return pairs.length === 0 ? base : `${base}${base.includes("?") ? "&" : "?"}${pairs.join("&")}`;
}

async function readCapped(body: ReadableStream<Uint8Array> | null, cap: number): Promise<Uint8Array | null> {
  if (body === null) return new Uint8Array();
  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > cap) {
      await reader.cancel();
      return null;
    }
    chunks.push(value);
  }
  const out = new Uint8Array(size);
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.byteLength;
  }
  return out;
}

// Never surface fetch errors: their text can carry the upstream URL and key.
async function upstream(url: string, init: RequestInit): Promise<Response> {
  try {
    return await fetch(url, { ...init, redirect: "manual" });
  } catch {
    throw new Fail(502, "The upstream service could not be reached.");
  }
}

// ---- /api/rpc ----

function rpcError(id: unknown, code: number, message: string) {
  return { jsonrpc: "2.0", id: id ?? null, error: { code, message } };
}

const validCall = (c: unknown): c is { id?: unknown; method: string } =>
  typeof c === "object" && c !== null && !Array.isArray(c) && (c as { jsonrpc?: unknown }).jsonrpc === "2.0" &&
  typeof (c as { method?: unknown }).method === "string";

async function rpc(request: Request, env: Env): Promise<Response> {
  const body = await readCapped(request.body, RPC_BODY_CAP);
  if (body === null) throw new Fail(413, "The request is larger than 64 KiB.");
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder().decode(body));
  } catch {
    return json(rpcError(null, -32700, "Parse error"));
  }
  const calls = Array.isArray(value) ? value : [value];
  if (calls.length === 0) return json(rpcError(null, -32600, "Invalid request"));
  const refused = calls.map((c) =>
    !validCall(c) ? rpcError(null, -32600, "Invalid request")
      : !RPC_METHODS.has(c.method) ? rpcError(c.id, -32601, "Method not allowed")
      : null,
  );
  if (refused.some((r) => r !== null)) {
    const replies = refused.map((r, i) => r ?? rpcError((calls[i] as { id?: unknown }).id, -32600, "Batch refused: another request in it is not allowed"));
    return json(Array.isArray(value) ? replies : replies[0]);
  }
  const base = env.UPSTREAM_RPC ?? (env.NETWORK === "local" ? LOCAL.rpc : helius(env, "/"));
  // Forward the original bytes so u64 integers keep their precision.
  const response = await upstream(withKey(base, env), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body,
  });
  if (!response.ok) {
    await response.body?.cancel();
    throw new Fail(response.status === 429 ? 429 : 502, `The RPC upstream answered with status ${response.status}.`);
  }
  return new Response(response.body, {
    headers: { "content-type": response.headers.get("content-type") ?? "application/json", "cache-control": "no-store" },
  });
}

// ---- /v1/spr/* ----

async function spr(request: Request, env: Env, url: URL): Promise<Response> {
  const sub = url.pathname.slice("/v1/spr".length);
  const isIndexer = INDEXER.test(sub);
  const isProver = PROVER.some(([, re]) => re.test(sub));
  if (!isIndexer && !isProver) throw new Fail(404, "Not found.");
  const allowed = isIndexer ? request.method === "POST" : PROVER.some(([m, re]) => m === request.method && re.test(sub));
  if (!allowed) throw new Fail(405, "This method is not allowed for this route.");

  const headers = new Headers();
  for (const name of SPR_REQUEST_HEADERS) {
    const v = request.headers.get(name);
    if (v !== null) headers.set(name, v);
  }
  let body: Uint8Array | undefined;
  if (request.method === "POST") {
    const read = await readCapped(request.body, SPR_CAP);
    if (read === null) throw new Fail(413, "The request is larger than 1 MiB.");
    body = read;
  }
  const local = env.NETWORK === "local";
  const base = isIndexer
    ? env.UPSTREAM_INDEXER ?? env.UPSTREAM_SPR ?? (local ? LOCAL.indexer : helius(env, "/v1/spr"))
    : env.UPSTREAM_PROVER ?? env.UPSTREAM_SPR ?? (local ? LOCAL.prover : helius(env, "/v1/spr"));
  const target = withKey(base.replace(/\/+$/, "") + sub, env, url.search.slice(1));
  const response = await upstream(target, { method: request.method, headers, body });

  const length = Number(response.headers.get("content-length") ?? 0);
  const bytes = length > SPR_CAP ? null : await readCapped(response.body, SPR_CAP);
  if (bytes === null) throw new Fail(502, "The upstream reply is larger than 1 MiB.");
  if (env.HELIUS_API_KEY && new TextDecoder().decode(bytes).includes(env.HELIUS_API_KEY)) {
    throw new Fail(502, "The upstream service returned an error.");
  }
  const out = new Headers({ "cache-control": "no-store" });
  for (const name of SPR_RESPONSE_HEADERS) {
    const v = response.headers.get(name);
    if (v !== null) out.set(name, v);
  }
  // 3xx would reveal upstream locations; the SDK never expects one.
  const status = response.status >= 300 && response.status < 400 ? 502 : response.status;
  return new Response(bytes, { status, headers: out });
}

// ---- /api/tee-policy ----

async function teePolicy(env: Env): Promise<Response> {
  const url = TEE_POLICY[env.NETWORK];
  if (url === undefined) throw new Fail(404, "This network has no TEE policy.");
  const response = await upstream(url, { method: "GET" });
  const bytes = response.ok ? await readCapped(response.body, 64 * 1024) : null;
  if (bytes === null) throw new Fail(502, "The TEE policy could not be fetched.");
  return new Response(bytes, { headers: { "content-type": "application/json", "cache-control": "no-store" } });
}

// ---- dealers ----

async function smallJson(request: Request): Promise<Record<string, unknown>> {
  const bytes = await readCapped(request.body, SMALL_BODY_CAP);
  if (bytes === null) throw new Fail(413, "The request is too large.");
  try {
    const v: unknown = JSON.parse(new TextDecoder().decode(bytes));
    if (typeof v === "object" && v !== null && !Array.isArray(v)) return v as Record<string, unknown>;
  } catch {}
  throw new Fail(400, "The request body must be a JSON object.");
}

const isAddress = (v: unknown): v is string => typeof v === "string" && BASE58.test(v);
const isAmount = (v: unknown): v is string => typeof v === "string" && /^(0|[1-9][0-9]{0,19})$/.test(v) && BigInt(v) <= U64_MAX;

function dealerKey(env: Env): string {
  if (!env.DEALER_KEY) throw new Fail(503, "Dealers are not configured.");
  return env.DEALER_KEY;
}

async function askDealer(dealerUrl: string, route: string, key: string, body: string, timeoutMs: number): Promise<Response | null> {
  try {
    return await fetch(`${dealerUrl.replace(/\/+$/, "")}${route}`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-keeler-key": key },
      body,
      redirect: "manual",
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch {
    return null;
  }
}

async function quotes(request: Request, env: Env): Promise<Response> {
  const b = await smallJson(request);
  if (!isAddress(b.sell_mint) || !isAddress(b.buy_mint) || !isAddress(b.trader)) {
    throw new Fail(400, "sell_mint, buy_mint and trader must be base58 addresses.");
  }
  if (!isAmount(b.sell_amount)) throw new Fail(400, "sell_amount must be a whole number string that fits in a u64.");
  const list = dealers(env);
  const key = dealerKey(env);
  const body = JSON.stringify({ sell_mint: b.sell_mint, buy_mint: b.buy_mint, sell_amount: b.sell_amount, trader: b.trader });
  const answers = await Promise.all(
    list.map(async (d) => {
      const response = await askDealer(d.url, "/quote", key, body, DEALER_TIMEOUT_MS);
      let quote = null;
      try {
        const bytes = response?.ok ? await readCapped(response.body, SMALL_BODY_CAP) : null;
        const q = bytes && JSON.parse(new TextDecoder().decode(bytes))?.quote;
        if (isAmount(q?.buy_amount) && Number.isSafeInteger(q.expiry) && q.expiry >= 0) {
          quote = { buy_amount: q.buy_amount, expiry: q.expiry };
        }
      } catch {}
      return { dealer: d.pubkey, quote };
    }),
  );
  return json(answers);
}

async function faucet(request: Request, env: Env): Promise<Response> {
  const b = await smallJson(request);
  if (!isAddress(b.wallet)) throw new Fail(400, "wallet must be a base58 address.");
  const first = dealers(env)[0];
  if (first === undefined) throw new Fail(503, "No dealer is configured.");
  const response = await askDealer(first.url, "/faucet", dealerKey(env), JSON.stringify({ wallet: b.wallet }), FAUCET_TIMEOUT_MS);
  if (response === null) throw new Fail(502, "The faucet did not answer.");
  let reply: { ok?: unknown; error?: unknown } | null = null;
  try {
    const bytes = await readCapped(response.body, SMALL_BODY_CAP);
    reply = bytes && JSON.parse(new TextDecoder().decode(bytes));
  } catch {}
  if (response.ok && reply?.ok === true) return json({ ok: true });
  const reason = typeof reply?.error === "string" ? reply.error.slice(0, 200) : "The faucet refused the request.";
  return json({ error: reason }, response.status >= 400 && response.status < 500 ? response.status : 502);
}
