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
  PAUSED?: string;
  PYTH_ACCOUNTS?: unknown;
  PYTH_MAX_AGE_SECS?: string | number;
  PYTH_MAX_CONF_BPS?: string | number;
  RPC_LIMIT?: RateLimit;
  RPC_GLOBAL_LIMIT?: RateLimit;
  SPR_LIMIT?: RateLimit;
  SPR_GLOBAL_LIMIT?: RateLimit;
  PROVE_LIMIT?: RateLimit;
  PROVE_GLOBAL_LIMIT?: RateLimit;
  QUOTE_LIMIT?: RateLimit;
  FAUCET_LIMIT?: RateLimit;
  LEND_PROGRAM_ID?: string;
  LEND_POOLS?: unknown;
  LEND_VAULTS?: unknown;
  LEND_READ_LIMIT?: RateLimit;
  LEND_READ_GLOBAL_LIMIT?: RateLimit;
  LEND_TICKET_LIMIT?: RateLimit;
  LEND_TICKET_GLOBAL_LIMIT?: RateLimit;
  LEND_BUILD_LIMIT?: RateLimit;
  LEND_BUILD_GLOBAL_LIMIT?: RateLimit;
}

const RPC_BODY_CAP = 64 * 1024;
const RPC_BATCH_CAP = 10;
// getProgramAccounts only for the SDK's two programs (dist/interface/program.js) plus PROGRAM_ID.
const GPA_PROGRAMS = ["regy3WAeRXMYuMYxr5iK3Bf9Arr3PcxGxyzAddbiqDQ", "ring1GaLCuJnkUMVkX2JeZLTTX17wKsV4fZmPVzXANW"];
const HELIUS_HOSTS = new Set(["devnet.helius-rpc.com", "mainnet.helius-rpc.com"]);
// 1 MiB plus the TEE envelope (nonce 12 + status 2 + GCM tag 16) the SDK allows on top.
const SPR_CAP = 1024 * 1024 + 64;
// The SDK's own indexer limit (dist/api/index.js MAX_BODY_BYTES).
const INDEXER_RESPONSE_CAP = 10 * 1024 * 1024;
const PRICE_CACHE_MS = 5000;
const PRICE_TIMEOUT_MS = 5000;
const SMALL_BODY_CAP = 4 * 1024;
const DEALER_TIMEOUT_MS = 3000;
const FAUCET_TIMEOUT_MS = 15000;
const LEND_BODY_CAP = 16 * 1024;
const LEND_TIMEOUT_MS = 10000;
// A build makes two proofs.
const LEND_BUILD_TIMEOUT_MS = 30000;
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
    const response = await api(request, env, url, path);
    // Proxied bytes are never rendered: no sniffing, no active content.
    const hardened = new Response(response.body, response);
    hardened.headers.set("x-content-type-options", "nosniff");
    hardened.headers.set("content-security-policy", "default-src 'none'");
    return hardened;
  },
};

async function api(request: Request, env: Env, url: URL, path: string): Promise<Response> {
    try {
      const origin = request.headers.get("origin");
      if (origin !== null && origin !== url.origin) throw new Fail(403, "Cross-origin requests are not allowed.");
      if (path === "/api/config") return only(request, "GET") ?? json(config(env));
      if (path === "/api/rpc") return only(request, "POST") ?? (await rpc(request, env));
      if (path === "/api/tee-policy") return only(request, "GET") ?? (await limit(env, "RPC_LIMIT", request), await teePolicy(env));
      if (path === "/api/prices") return only(request, "GET") ?? (await limit(env, "RPC_LIMIT", request), json(await prices(env)));
      if (path === "/api/quotes") {
        if (env.PAUSED === "1") throw new Fail(503, "Swaps are paused.");
        return only(request, "POST") ?? (await limit(env, "QUOTE_LIMIT", request), await quotes(request, env));
      }
      if (path === "/api/faucet") {
        if (env.FAUCET !== "1") throw new Fail(404, "Not found.");
        if (env.PAUSED === "1") throw new Fail(503, "Swaps are paused.");
        return only(request, "POST") ?? (await limit(env, "FAUCET_LIMIT", request), await faucet(request, env));
      }
      if (path.startsWith("/api/lend/")) return await lend(request, env, url, path);
      if (path.startsWith("/v1/spr/")) return await spr(request, env, url);
      throw new Fail(404, "Not found.");
    } catch (error) {
      if (error instanceof Fail) return json({ error: error.message }, error.status);
      return json({ error: "Something went wrong in the server." }, 500);
    }
}

function only(request: Request, method: string): Response | undefined {
  return request.method === method ? undefined : json({ error: `Use ${method} for this route.` }, 405);
}

/** The rate-limit key: an IPv6 client by its /64 (one subscriber's prefix), IPv4 as is. */
export function clientKey(ip: string | null): string {
  if (ip === null || ip === "") return "unknown";
  if (!ip.includes(":") || ip.includes(".")) return ip.slice(ip.lastIndexOf(":") + 1); // IPv4, or IPv4-mapped IPv6
  const [head = "", tail] = ip.toLowerCase().split("::");
  const left = head === "" ? [] : head.split(":");
  const right = tail === undefined || tail === "" ? [] : tail.split(":");
  const full = [...left, ...Array(Math.max(0, 8 - left.length - right.length)).fill("0"), ...right];
  return full.slice(0, 4).map((h) => h.replace(/^0+(?=.)/, "")).join(":") + "::/64";
}

type LimitName = "RPC_LIMIT" | "RPC_GLOBAL_LIMIT" | "SPR_LIMIT" | "SPR_GLOBAL_LIMIT" | "PROVE_LIMIT" | "PROVE_GLOBAL_LIMIT" | "QUOTE_LIMIT" | "FAUCET_LIMIT"
  | "LEND_READ_LIMIT" | "LEND_READ_GLOBAL_LIMIT" | "LEND_TICKET_LIMIT" | "LEND_TICKET_GLOBAL_LIMIT" | "LEND_BUILD_LIMIT" | "LEND_BUILD_GLOBAL_LIMIT";

/**
 * Charges `times` units to a limiter, keyed per client unless `global`. A missing binding is no
 * limit only on `local` (plain `wrangler dev`); anywhere else it fails closed.
 */
async function limit(env: Env, name: LimitName, request: Request, times = 1, global = false): Promise<void> {
  const limiter = env[name];
  if (limiter === undefined) {
    if (env.NETWORK === "local") return;
    throw new Fail(503, "Rate limiting is not configured on this server, so it refuses this request.");
  }
  const key = global ? "global" : clientKey(request.headers.get("cf-connecting-ip"));
  for (let i = 0; i < times; i++) {
    const { success } = await limiter.limit({ key });
    if (!success) throw new Fail(429, global ? "The service is busy. Wait a minute and try again." : "Too many requests. Wait a minute and try again.");
  }
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
    assets: assets(env),
    dealers: dealers(env).map((d) => ({ pubkey: d.pubkey })),
    bandBps: Number(env.BAND_BPS ?? 0),
    minTtlSecs: Number(env.MIN_TTL_SECS ?? 0),
    maxTtlSecs: Number(env.MAX_TTL_SECS ?? 0),
    faucet: env.FAUCET === "1",
    paused: env.PAUSED === "1",
    ...(lendConfig(env) === undefined ? {} : { lend: lendConfig(env) }),
  };
}

// ---- shapes ----

type Check = (v: unknown) => boolean;
/** Field name → [check, required]. */
type Spec = Record<string, readonly [Check, boolean]>;

const uint: Check = (v) => Number.isSafeInteger(v) && (v as number) >= 0;
const bps: Check = (v) => uint(v) && (v as number) <= 10_000;
const hex = (n: number): Check => (v) => typeof v === "string" && new RegExp(`^[0-9a-f]{${n}}$`).test(v);
const base64 = (max: number): Check => (v) => typeof v === "string" && v.length > 0 && v.length <= max && v.length % 4 === 0 && /^[A-Za-z0-9+/]*={0,2}$/.test(v);
const oneOf = (...values: string[]): Check => (v) => typeof v === "string" && values.includes(v);
const list = (item: Check, min: number, max: number): Check => (v) => Array.isArray(v) && v.length >= min && v.length <= max && v.every(item);
const object = (spec: Spec): Check => (v) => shaped(v, spec) !== null;
const address: Check = (v) => isAddress(v);
const amount: Check = (v) => isAmount(v);

/** The object with exactly the spec's fields, or null if a field is missing, wrong, or unknown. */
function shaped(v: unknown, spec: Spec): Record<string, unknown> | null {
  if (typeof v !== "object" || v === null || Array.isArray(v)) return null;
  const o = v as Record<string, unknown>;
  if (Object.keys(o).some((k) => !Object.hasOwn(spec, k))) return null;
  const out: Record<string, unknown> = {};
  for (const [k, [check, required]] of Object.entries(spec)) {
    if (o[k] === undefined) {
      if (required) return null;
    } else if (!check(o[k])) return null;
    else out[k] = o[k];
  }
  return out;
}

// ---- lend config ----

const CURVE_POINT: Spec = { utilisationBps: [bps, true], rateBps: [uint, true] };
// Piecewise linear from 0 % to 100 % utilisation, utilisation strictly rising.
const curve: Check = (v) => {
  if (!list(object(CURVE_POINT), 2, 8)(v)) return false;
  const u = (v as { utilisationBps: number }[]).map((p) => p.utilisationBps);
  return u[0] === 0 && u.at(-1) === 10_000 && u.every((x, i) => i === 0 || x > u[i - 1]!);
};
const POOL: Spec = {
  mint: [address, true],
  curve: [curve, true],
  feeBps: [bps, true],
  maxUtilisationBps: [bps, true],
  minSupply: [amount, true],
};
const VAULT: Spec = {
  id: [(v) => uint(v) && (v as number) <= 0xffff, true],
  collateralMint: [address, true],
  debtMint: [address, true],
  cfBps: [bps, true],
  ltBps: [bps, true],
  lmlBps: [bps, true],
  penaltyBps: [bps, true],
  minCollateral: [amount, true],
  minDebt: [amount, true],
};

type LendConfig = { programId: string; pools: Record<string, unknown>[]; vaults: Record<string, unknown>[] };

/** Lend is off (undefined) when none of its variables is set; set but malformed is a config error. */
function lendConfig(env: Env): LendConfig | undefined {
  if (env.LEND_PROGRAM_ID === undefined && env.LEND_POOLS === undefined && env.LEND_VAULTS === undefined) return undefined;
  if (!isAddress(env.LEND_PROGRAM_ID)) throw new Fail(500, "The server setting LEND_PROGRAM_ID is malformed.");
  const all = (name: "LEND_POOLS" | "LEND_VAULTS", spec: Spec) => {
    const raw = parsed(env[name], name);
    const items = Array.isArray(raw) && raw.length > 0 ? raw.map((x) => shaped(x, spec)) : [null];
    if (items.some((x) => x === null)) throw new Fail(500, `The server setting ${name} is malformed.`);
    return items as Record<string, unknown>[];
  };
  const pools = all("LEND_POOLS", POOL);
  const vaults = all("LEND_VAULTS", VAULT);
  const mints = new Set(pools.map((p) => p.mint));
  const ok = vaults.every((v, i) =>
    mints.has(v.debtMint) && (v.cfBps as number) <= (v.ltBps as number) && (v.ltBps as number) <= (v.lmlBps as number) &&
    vaults.findIndex((w) => w.id === v.id) === i);
  if (!ok || mints.size !== pools.length) throw new Fail(500, "The server settings LEND_POOLS and LEND_VAULTS do not fit together.");
  return { programId: env.LEND_PROGRAM_ID, pools, vaults };
}

type Asset = { mint: string; symbol: string; decimals: number; maxSell?: string };

function assets(env: Env): Asset[] {
  const list = parsed(env.ASSETS ?? [], "ASSETS");
  const ok = (a: Asset) =>
    isAddress(a?.mint) && typeof a.symbol === "string" && Number.isInteger(a.decimals) && (a.maxSell === undefined || isAmount(a.maxSell));
  if (!Array.isArray(list) || !list.every(ok)) throw new Fail(500, "The server setting ASSETS is malformed.");
  return list;
}

// ---- /api/prices ----

type Quoted = { price: string; expo: number; publishTime: number; fixed?: true };
let priceCache: { key: string; at: number; prices: Record<string, Quoted> } | undefined;

// Pyth's Solana receiver program; it owns every PriceUpdateV2 account.
const PYTH_RECEIVER = "rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ";
// Anchor discriminator: the first 8 bytes of sha256("account:PriceUpdateV2"); the test recomputes it.
export const PRICE_UPDATE_V2 = [34, 241, 35, 99, 157, 126, 244, 205];

function limitVar(value: string | number | undefined, name: string): number {
  const n = Number(value);
  if (value === undefined || value === "" || !Number.isSafeInteger(n) || n < 0) throw new Fail(500, `The server setting ${name} is missing or not a whole number.`);
  return n;
}

type PythAccount = { account: string; feedId: string };

async function prices(env: Env): Promise<Record<string, Quoted>> {
  const now = Math.floor(Date.now() / 1000);
  const raw = parsed(env.PYTH_ACCOUNTS ?? {}, "PYTH_ACCOUNTS");
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) throw new Fail(500, "The server setting PYTH_ACCOUNTS is malformed.");
  const feeds: Record<string, PythAccount> = {};
  for (const [mint, v] of Object.entries(raw as Record<string, Partial<PythAccount>>)) {
    const feedId = typeof v?.feedId === "string" ? v.feedId.replace(/^0x/, "").toLowerCase() : "";
    if (!isAddress(mint) || !isAddress(v?.account) || !/^[0-9a-f]{64}$/.test(feedId)) {
      throw new Fail(500, "The server setting PYTH_ACCOUNTS is malformed.");
    }
    feeds[mint] = { account: v.account, feedId };
  }
  const list = assets(env);
  if (env.NETWORK === "mainnet") {
    const missing = list.find((a) => feeds[a.mint] === undefined);
    if (missing) throw new Fail(500, `No Pyth price account is configured for ${missing.symbol} (${missing.mint}); mainnet needs one for every asset.`);
  }
  const reference = parsed(env.REFERENCE ?? {}, "REFERENCE") as Record<string, { price: unknown; expo: unknown }>;
  const out: Record<string, Quoted> = {};
  for (const [mint, r] of Object.entries(reference ?? {})) {
    if (feeds[mint] !== undefined || env.NETWORK === "mainnet") continue;
    // "fixed" tells the page this is a configured price, not one published `now`.
    out[mint] = { price: String(r?.price), expo: Number(r?.expo), publishTime: now, fixed: true };
  }
  if (Object.keys(feeds).length === 0) return out;
  const symbol = (mint: string) => list.find((a) => a.mint === mint)?.symbol ?? mint;
  return { ...out, ...(await pyth(env, feeds, symbol, now)) };
}

async function pyth(env: Env, feeds: Record<string, PythAccount>, symbol: (mint: string) => string, now: number): Promise<Record<string, Quoted>> {
  const maxAge = limitVar(env.PYTH_MAX_AGE_SECS, "PYTH_MAX_AGE_SECS");
  const maxConf = limitVar(env.PYTH_MAX_CONF_BPS, "PYTH_MAX_CONF_BPS");
  const base = rpcBase(env);
  const key = JSON.stringify([base, feeds]);
  // Age is re-checked on every read; a cached price that went stale is read again.
  const fresh = (p: Record<string, Quoted>) => Object.values(p).every((q) => now - q.publishTime <= maxAge);
  if (priceCache?.key === key && Date.now() - priceCache.at < PRICE_CACHE_MS && fresh(priceCache.prices)) return priceCache.prices;

  const mints = Object.keys(feeds);
  const body = JSON.stringify({
    jsonrpc: "2.0", id: 1, method: "getMultipleAccounts",
    params: [mints.map((m) => feeds[m]!.account), { encoding: "base64", commitment: "confirmed" }],
  });
  let value: unknown;
  try {
    const response = await fetch(withKey(base, env), {
      method: "POST", headers: { "content-type": "application/json" }, body, redirect: "manual", signal: AbortSignal.timeout(PRICE_TIMEOUT_MS),
    });
    const bytes = response.ok ? await readCapped(response.body, 64 * 1024) : null;
    if (bytes === null) throw new Error();
    value = JSON.parse(new TextDecoder().decode(bytes))?.result?.value;
  } catch {
    throw new Fail(503, "Live prices are unavailable right now. Try again shortly.");
  }
  if (!Array.isArray(value) || value.length !== mints.length) throw new Fail(503, "Live prices are unavailable right now. Try again shortly.");
  const result: Record<string, Quoted> = {};
  mints.forEach((mint, i) => {
    result[mint] = checkPriceAccount(value[i], feeds[mint]!.feedId, symbol(mint), now, maxAge, maxConf);
  });
  priceCache = { key, at: Date.now(), prices: result };
  return result;
}

// PriceUpdateV2, little-endian: discriminator 8 | write authority 32 | verification level at 40
// (1 = Full, one byte; 0 = Partial, two bytes) | feed id 32 | price i64 | conf u64 | expo i32 | publish time i64 | ...
export function checkPriceAccount(account: unknown, feedId: string, symbol: string, now: number, maxAge: number, maxConfBps: number): Quoted {
  const fail = (why: string): never => {
    throw new Fail(503, `The ${symbol} price failed a safety check: ${why}.`);
  };
  const a = account as { owner?: unknown; data?: unknown } | null;
  if (a === null || typeof a !== "object") return fail("its Pyth account does not exist");
  if (a.owner !== PYTH_RECEIVER) return fail("its account is not owned by Pyth's receiver program");
  const b64 = Array.isArray(a.data) && a.data[1] === "base64" && typeof a.data[0] === "string" ? a.data[0] : fail("its account data is not base64");
  let data: Uint8Array;
  try {
    data = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
  } catch {
    return fail("its account data is not base64");
  }
  if (data.length < 101) return fail("its account is too short");
  if (PRICE_UPDATE_V2.some((x, i) => data[i] !== x)) return fail("its account is not a PriceUpdateV2");
  if (data[40] !== 1) return fail("the update is not fully verified");
  const id = [...data.subarray(41, 73)].map((x) => x.toString(16).padStart(2, "0")).join("");
  if (id !== feedId) return fail("the account holds a different feed");
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const price = view.getBigInt64(73, true);
  const conf = view.getBigUint64(81, true);
  const expo = view.getInt32(89, true);
  const published = view.getBigInt64(93, true);
  const t = BigInt(now);
  if (published > t + BigInt(maxAge)) return fail("it is published in the future");
  if (t - published > BigInt(maxAge)) return fail(`it is stale (published ${t - published} s ago, max ${maxAge} s)`);
  if (price <= 0n) return fail("the price is not positive");
  if (conf * 10_000n > price * BigInt(maxConfBps)) return fail(`its confidence is wider than ${maxConfBps} bps of the price`);
  return { price: price.toString(), expo, publishTime: Number(published) };
}

// ---- upstreams ----

const rpcBase = (env: Env) => env.UPSTREAM_RPC ?? (env.NETWORK === "local" ? LOCAL.rpc : helius(env, "/"));

function helius(env: Env, path: string): string {
  if (env.NETWORK !== "devnet" && env.NETWORK !== "mainnet") throw new Fail(503, "This network has no upstream configured.");
  return `https://${env.NETWORK}.helius-rpc.com${path}`;
}

function withKey(base: string, env: Env, query = ""): string {
  const pairs = query.split("&").filter((p) => p !== "" && !new URLSearchParams(p).has("api-key"));
  // The key goes only to Helius itself, never to an UPSTREAM_* override host.
  if (env.HELIUS_API_KEY && HELIUS_HOSTS.has(new URL(base).hostname)) pairs.push(`api-key=${encodeURIComponent(env.HELIUS_API_KEY)}`);
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

const validCall = (c: unknown): c is { id?: unknown; method: string; params?: unknown } =>
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
  if (calls.length > RPC_BATCH_CAP) return json(rpcError(null, -32600, `Batch refused: more than ${RPC_BATCH_CAP} requests`));
  const programs = new Set([...GPA_PROGRAMS, ...(env.PROGRAM_ID ? [env.PROGRAM_ID] : [])]);
  const refused = calls.map((c) =>
    !validCall(c) ? rpcError(null, -32600, "Invalid request")
      : !RPC_METHODS.has(c.method) ? rpcError(c.id, -32601, "Method not allowed")
      : c.method === "getProgramAccounts" && !programs.has((c.params as unknown[] | undefined)?.[0] as string) ? rpcError(c.id, -32602, "getProgramAccounts is allowed only for Keeler's programs")
      : hasUnsafeInteger(c.params) ? rpcError(c.id, -32602, "Invalid params: an integer is too large")
      : null,
  );
  if (refused.some((r) => r !== null)) {
    const replies = refused.map((r, i) => r ?? rpcError((calls[i] as { id?: unknown }).id, -32600, "Batch refused: another request in it is not allowed"));
    return json(Array.isArray(value) ? replies : replies[0]);
  }
  // One unit per call, per client and for everyone, so a batch costs what its calls cost upstream.
  await limit(env, "RPC_LIMIT", request, calls.length);
  await limit(env, "RPC_GLOBAL_LIMIT", request, calls.length, true);
  // Forward exactly what was checked: duplicate or extra keys cannot read differently upstream.
  const clean = (calls as { id?: unknown; method: string; params?: unknown }[]).map((c) => ({
    jsonrpc: "2.0", id: c.id ?? null, method: c.method, ...(c.params === undefined ? {} : { params: c.params }),
  }));
  const response = await upstream(withKey(rpcBase(env), env), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(Array.isArray(value) ? clean : clean[0]),
  });
  if (!response.ok) {
    await response.body?.cancel();
    throw new Fail(response.status === 429 ? 429 : 502, `The RPC upstream answered with status ${response.status}.`);
  }
  return new Response(response.body, { headers: { "content-type": "application/json", "cache-control": "no-store" } });
}

// Re-serialising would round an integer past 2^53, so such a call is refused instead.
function hasUnsafeInteger(v: unknown): boolean {
  if (typeof v === "number") return Number.isInteger(v) && !Number.isSafeInteger(v);
  if (typeof v === "object" && v !== null) return Object.values(v).some(hasUnsafeInteger);
  return false;
}

// ---- /v1/spr/* ----

async function spr(request: Request, env: Env, url: URL): Promise<Response> {
  const sub = url.pathname.slice("/v1/spr".length);
  const isIndexer = INDEXER.test(sub);
  const isProver = PROVER.some(([, re]) => re.test(sub));
  if (!isIndexer && !isProver) throw new Fail(404, "Not found.");
  const allowed = isIndexer ? request.method === "POST" : PROVER.some(([m, re]) => m === request.method && re.test(sub));
  if (!allowed) throw new Fail(405, "This method is not allowed for this route.");
  // A proof POST costs Helius credits; status polls and the rest use the indexer limit.
  const proving = request.method === "POST" && sub.startsWith("/prove/");
  await limit(env, proving ? "PROVE_LIMIT" : "SPR_LIMIT", request);

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
  if (proving) checkProofRequest(headers, body!);
  // Global spend is charged only for a request that passed every check, so junk cannot use it up.
  await limit(env, proving ? "PROVE_GLOBAL_LIMIT" : "SPR_GLOBAL_LIMIT", request, 1, true);
  const local = env.NETWORK === "local";
  const base = isIndexer
    ? env.UPSTREAM_INDEXER ?? env.UPSTREAM_SPR ?? (local ? LOCAL.indexer : helius(env, "/v1/spr"))
    : env.UPSTREAM_PROVER ?? env.UPSTREAM_SPR ?? (local ? LOCAL.prover : helius(env, "/v1/spr"));
  const target = withKey(base.replace(/\/+$/, "") + sub, env, url.search.slice(1));
  const response = await upstream(target, { method: request.method, headers, body });

  const cap = isIndexer ? INDEXER_RESPONSE_CAP : SPR_CAP;
  const length = Number(response.headers.get("content-length") ?? 0);
  const bytes = length > cap ? null : await readCapped(response.body, cap);
  if (bytes === null) throw new Fail(502, `The upstream reply is larger than ${isIndexer ? 10 : 1} MiB.`);
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

// dist/client/prover: a plain proof POST is a JSON object; through the TEE it is
// application/octet-stream with Spr-Tee and Spr-Tee-Enc and at least a 16-byte GCM tag.
function checkProofRequest(headers: Headers, body: Uint8Array) {
  const type = (headers.get("content-type") ?? "").split(";")[0]!.trim().toLowerCase();
  if (type === "application/octet-stream") {
    if (headers.get("spr-tee") === null || headers.get("spr-tee-enc") === null || body.byteLength < 16) {
      throw new Fail(400, "An encrypted proof request needs Spr-Tee, Spr-Tee-Enc and a body.");
    }
    return;
  }
  if (type !== "application/json") throw new Fail(415, "A proof request must be application/json or application/octet-stream.");
  let v: unknown;
  try {
    v = JSON.parse(new TextDecoder().decode(body));
  } catch {}
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Fail(400, "The proof request must be a JSON object.");
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

async function smallJson(request: Request, cap = SMALL_BODY_CAP): Promise<Record<string, unknown>> {
  const bytes = await readCapped(request.body, cap);
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

/** POSTs `body`, or GETs when there is none. */
async function askDealer(dealerUrl: string, route: string, key: string, body: string | undefined, timeoutMs: number): Promise<Response | null> {
  try {
    return await fetch(`${dealerUrl.replace(/\/+$/, "")}${route}`, {
      method: body === undefined ? "GET" : "POST",
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
        // The signature is checked for shape only; the browser verifies it against the dealer key.
        if (isAmount(q?.buy_amount) && Number.isSafeInteger(q.expiry) && q.expiry >= 0 && typeof q.signature === "string" && /^[0-9a-f]{128}$/.test(q.signature)) {
          quote = { buy_amount: q.buy_amount, expiry: q.expiry, signature: q.signature };
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

// ---- /api/lend/* ----

const vaultId: Check = (v) => uint(v) && (v as number) <= 0xffff;
// A request signed by the browser session key (`sessionKey`), which the wallet authorised once (`sessionAuth`, over
// the text in docs/lend-api.md) until `sessionExpiry`; with a nonce and a timestamp. The keeper verifies all of it.
const SIGNED: Spec = { wallet: [address, true], nonce: [hex(32), true], timestamp: [uint, true], sessionKey: [address, true], sessionExpiry: [uint, true], sessionAuth: [hex(128), true], signature: [hex(128), true] };
const PURPOSE = oneOf("supply", "deposit", "repay");
// The owner message, encrypted to the owner's viewing key.
const message = base64(512);

const POOLS_REPLY: Spec = {
  pools: [list(object({
    mint: [address, true], at: [uint, true], size: [amount, true], utilisationBps: [bps, true], supplyRateBps: [uint, true], borrowRateBps: [uint, true],
  }), 0, 8), true],
};
const TICKET_REQUEST: Spec = {
  ...SIGNED, purpose: [PURPOSE, true], amount: [amount, true], pool: [address, false], vault: [vaultId, false], position: [amount, false],
};
const TICKET_REPLY: Spec = { id: [hex(32), true], expiry: [uint, true], signature: [hex(128), true] };
const BUILD_REQUEST: Spec = {
  ...SIGNED, op: [oneOf("redeem", "draw"), true], pool: [address, false], vault: [vaultId, false], position: [amount, false],
  amount: [amount, false], borrow: [amount, false], withdraw: [amount, false],
};
// A v1 transaction is at most 4,096 bytes: 5,464 base64 characters.
const BUILD_REPLY: Spec = { transaction: [base64(5464), true], outputs: [list(hex(64), 1, 8), true], now: [uint, true], bound: [amount, false], index: [amount, false] };
const ACCOUNT_REPLY: Spec = {
  receipts: [list(object({ pool: [address, true], shares: [amount, true], message: [message, true] }), 0, 8), true],
  positions: [list(object({
    vault: [vaultId, true], position: [amount, true], collateral: [amount, true], debtShares: [amount, true], message: [message, true],
  }), 0, 8), true],
  intents: [list(object({
    id: [hex(32), true], purpose: [PURPOSE, true], amount: [amount, true], expiry: [uint, true], status: [oneOf("waiting", "filled", "refundable"), true],
  }), 0, 8), true],
};

type LendRoute = {
  method: "GET" | "POST"; keeper: string; limits: [LimitName, LimitName]; timeoutMs: number;
  /** The query for a GET, the body for a POST. */
  request: Spec; reply: Spec;
};
const LEND_ROUTES: Record<string, LendRoute> = {
  "/api/lend/pools": { method: "GET", keeper: "/lend/pools", limits: ["LEND_READ_LIMIT", "LEND_READ_GLOBAL_LIMIT"], timeoutMs: LEND_TIMEOUT_MS, request: {}, reply: POOLS_REPLY },
  "/api/lend/account": { method: "GET", keeper: "/lend/account", limits: ["LEND_READ_LIMIT", "LEND_READ_GLOBAL_LIMIT"], timeoutMs: LEND_TIMEOUT_MS, request: SIGNED, reply: ACCOUNT_REPLY },
  "/api/lend/ticket": { method: "POST", keeper: "/lend/ticket", limits: ["LEND_TICKET_LIMIT", "LEND_TICKET_GLOBAL_LIMIT"], timeoutMs: LEND_TIMEOUT_MS, request: TICKET_REQUEST, reply: TICKET_REPLY },
  "/api/lend/build": { method: "POST", keeper: "/lend/build", limits: ["LEND_BUILD_LIMIT", "LEND_BUILD_GLOBAL_LIMIT"], timeoutMs: LEND_BUILD_TIMEOUT_MS, request: BUILD_REQUEST, reply: BUILD_REPLY },
};

// shortcut: one keeper, the first DEALERS entry; return a list here when there is more than one.
function keeper(env: Env): { url: string } {
  const first = dealers(env)[0];
  if (first === undefined) throw new Fail(503, "No lending keeper is configured.");
  return first;
}

async function lend(request: Request, env: Env, url: URL, path: string): Promise<Response> {
  const route = Object.hasOwn(LEND_ROUTES, path) ? LEND_ROUTES[path]! : undefined;
  if (lendConfig(env) === undefined || route === undefined) throw new Fail(404, "Not found.");
  // PAUSED is the swap's switch: lending ignores it, so repayments and withdrawals keep working while liquidations run.
  // The keeper has its own pause, which stops only new supply, new positions and borrowing (a 403 the page shows).
  const wrong = only(request, route.method);
  if (wrong) return wrong;
  await limit(env, route.limits[0], request);

  let body: string | undefined;
  let query = "";
  if (route.method === "POST") {
    const clean = shaped(await smallJson(request, LEND_BODY_CAP), route.request);
    if (clean === null) throw new Fail(400, `The request body does not have the fields ${route.keeper} takes.`);
    body = JSON.stringify(clean);
  } else {
    const params = [...url.searchParams];
    const given = Object.fromEntries(params.map(([k, v]) => [k, (k === "timestamp" || k === "sessionExpiry") && /^(0|[1-9][0-9]{0,15})$/.test(v) ? Number(v) : v]));
    const clean = params.length === Object.keys(given).length ? shaped(given, route.request) : null;
    if (clean === null) throw new Fail(400, `The query does not have the fields ${route.keeper} takes.`);
    if (params.length > 0) query = "?" + new URLSearchParams(Object.entries(clean).map(([k, v]) => [k, String(v)]));
  }
  const key = dealerKey(env);
  const target = keeper(env);
  // Charged only once the request passed every check.
  await limit(env, route.limits[1], request, 1, true);

  const response = await askDealer(target.url, route.keeper + query, key, body, route.timeoutMs);
  if (response === null) throw new Fail(502, "The lending keeper did not answer.");
  let reply: unknown = null;
  try {
    const bytes = await readCapped(response.body, LEND_BODY_CAP);
    reply = bytes && JSON.parse(new TextDecoder().decode(bytes));
  } catch {}
  if (response.ok) {
    const clean = shaped(reply, route.reply);
    if (clean === null) throw new Fail(502, "The lending keeper sent a malformed reply.");
    return json(clean);
  }
  const error = (reply as { error?: unknown } | null)?.error;
  if (response.status >= 400 && response.status < 500 && typeof error === "string") return json({ error: error.slice(0, 200) }, response.status);
  throw new Fail(502, "The lending keeper could not do this right now.");
}
