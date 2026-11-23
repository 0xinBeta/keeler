import { afterEach, describe, expect, it, vi } from "vitest";
import worker, { type Env } from "../src/index";

const KEY = "HELIUSSECRET123";
const DKEY = "DEALERSECRET456";
const PKEY = "PYTHSECRET789";
const SIG = "ab".repeat(64);
const MINT_A = "So11111111111111111111111111111111111111112";
const MINT_B = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const TRADER = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";

type Call = { url: string; init: RequestInit };
let calls: Call[] = [];
function mockFetch(handler: (url: string, init: RequestInit) => Response | Promise<Response>) {
  calls = [];
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init = {}) => {
    const url = String(input);
    calls.push({ url, init });
    return handler(url, init);
  });
}
afterEach(() => vi.restoreAllMocks());

const allow = { limit: async () => ({ success: true }) } as RateLimit;
function env(over: Partial<Env> = {}): Env {
  return {
    STATIC: { fetch: async () => new Response("<!doctype html>", { headers: { "content-type": "text/html" } }) } as unknown as Fetcher,
    HELIUS_API_KEY: KEY,
    DEALER_KEY: DKEY,
    NETWORK: "devnet",
    DEALERS: JSON.stringify([{ url: "https://d1.example", pubkey: "D1" }, { url: "https://d2.example", pubkey: "D2" }, { url: "https://d3.example", pubkey: "D3" }]),
    PROGRAM_ID: "F4hjig",
    ASSETS: JSON.stringify([{ mint: MINT_A, symbol: "tSOL", decimals: 9 }]),
    BAND_BPS: "100",
    MIN_TTL_SECS: "120",
    MAX_TTL_SECS: "600",
    REFERENCE: JSON.stringify({ [MINT_A]: { price: "150", expo: -2 } }),
    FAUCET: "1",
    RPC_LIMIT: allow,
    QUOTE_LIMIT: allow,
    FAUCET_LIMIT: allow,
    SPR_LIMIT: allow,
    PROVE_LIMIT: allow,
    ...over,
  };
}

const ORIGIN = "https://keeler.test";
function req(path: string, init: RequestInit & { headers?: Record<string, string> } = {}) {
  return new Request(ORIGIN + path, init);
}
const post = (path: string, body: unknown, headers: Record<string, string> = {}) =>
  req(path, { method: "POST", body: typeof body === "string" ? body : JSON.stringify(body), headers: { "content-type": "application/json", ...headers } });

async function call(r: Request, e = env()) {
  const res = await worker.fetch(r, e);
  const text = await res.text();
  expect(text).not.toContain(KEY);
  expect(text).not.toContain(DKEY);
  expect(text).not.toContain(PKEY);
  expect(text).not.toContain("helius-rpc.com");
  for (const [, v] of res.headers) expect(v).not.toContain(KEY);
  return { res, text, body: text ? JSON.parse(text) : null };
}

describe("/api/rpc", () => {
  it("forwards an allowed method with the key added and the bytes unchanged", async () => {
    mockFetch(() => Response.json({ jsonrpc: "2.0", id: 1, result: 12345678901234567890 }));
    const raw = '{"jsonrpc":"2.0","id":1,"method":"getBalance","params":["x",{"minContextSlot":18446744073709551615}]}';
    const { res, text } = await call(post("/api/rpc", raw));
    expect(res.status).toBe(200);
    expect(text).toContain("result");
    expect(calls[0]!.url).toBe(`https://devnet.helius-rpc.com/?api-key=${KEY}`);
    expect(new TextDecoder().decode(calls[0]!.init.body as Uint8Array)).toBe(raw);
  });

  it("refuses a disallowed method without calling upstream", async () => {
    mockFetch(() => new Response("x"));
    const { body } = await call(post("/api/rpc", { jsonrpc: "2.0", id: 7, method: "requestAirdrop", params: [] }));
    expect(body).toEqual({ jsonrpc: "2.0", id: 7, error: { code: -32601, message: "Method not allowed" } });
    expect(calls).toHaveLength(0);
  });

  it("checks a batch element by element", async () => {
    mockFetch(() => Response.json([]));
    const { body } = await call(post("/api/rpc", [
      { jsonrpc: "2.0", id: 1, method: "getSlot" },
      { jsonrpc: "2.0", id: 2, method: "getProgramAccounts" },
      { jsonrpc: "2.0", id: 3, method: "getLargestAccounts" },
    ]));
    expect(body.map((r: { id: number; error: { code: number } }) => [r.id, r.error.code])).toEqual([[1, -32600], [2, -32600], [3, -32601]]);
    expect(calls).toHaveLength(0);
    await call(post("/api/rpc", [{ jsonrpc: "2.0", id: 1, method: "getSlot" }, { jsonrpc: "2.0", id: 2, method: "sendTransaction" }]));
    expect(calls).toHaveLength(1);
  });

  it("refuses empty batches, bad JSON and bodies over 64 KiB", async () => {
    mockFetch(() => new Response("x"));
    expect((await call(post("/api/rpc", []))).body.error.code).toBe(-32600);
    expect((await call(post("/api/rpc", "{nope"))).body.error.code).toBe(-32700);
    const big = JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getSlot", params: ["a".repeat(65 * 1024)] });
    expect((await call(post("/api/rpc", big))).res.status).toBe(413);
    expect((await call(req("/api/rpc"))).res.status).toBe(405);
    expect(calls).toHaveLength(0);
  });

  it("hides upstream failures and errors", async () => {
    mockFetch((url) => new Response(`bad key at ${url}`, { status: 401 }));
    const a = await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }));
    expect(a.res.status).toBe(502);
    mockFetch((url) => { throw new Error(`connect failed ${url}`); });
    const b = await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }));
    expect(b.res.status).toBe(502);
    expect(b.body.error).toBeTypeOf("string");
  });

  it("uses UPSTREAM_RPC and the local default", async () => {
    mockFetch(() => Response.json({}));
    await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }), env({ NETWORK: "local", HELIUS_API_KEY: undefined }));
    expect(calls[0]!.url).toBe("http://127.0.0.1:8899");
    await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }), env({ UPSTREAM_RPC: "http://10.0.0.1:8899" }));
    expect(calls[1]!.url).toBe(`http://10.0.0.1:8899?api-key=${KEY}`);
  });

  it("rate limits per client IP", async () => {
    mockFetch(() => Response.json({}));
    const keys: string[] = [];
    const deny = { limit: async ({ key }: { key: string }) => (keys.push(key), { success: false }) } as RateLimit;
    const { res } = await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }, { "cf-connecting-ip": "1.2.3.4" }), env({ RPC_LIMIT: deny }));
    expect(res.status).toBe(429);
    expect(keys).toEqual(["1.2.3.4"]);
    expect(calls).toHaveLength(0);
  });
});

describe("/v1/spr/*", () => {
  it("forwards Spr-Tee headers, content type, sync header and query; strips the rest", async () => {
    mockFetch(() => new Response(new Uint8Array([1, 2, 3]), { headers: { "content-type": "application/octet-stream", "spr-tee": "v1", "set-cookie": "a=b", "x-upstream": "1" } }));
    const r = req("/v1/spr/prove/transfer_confidential_2_2/status?jobId=abc&api-key=evil", {
      headers: { "spr-tee": "v1", "spr-tee-enc": "ee", "spr-tee-ciphertext": "cc", cookie: "s=1", authorization: "x", "x-sync": "true" },
    });
    const res = await worker.fetch(r, env());
    expect(res.status).toBe(200);
    expect(new Uint8Array(await res.arrayBuffer())).toEqual(new Uint8Array([1, 2, 3]));
    expect(res.headers.get("spr-tee")).toBe("v1");
    expect(res.headers.get("set-cookie")).toBeNull();
    expect(res.headers.get("x-upstream")).toBeNull();
    const { url, init } = calls[0]!;
    expect(url).toBe(`https://devnet.helius-rpc.com/v1/spr/prove/transfer_confidential_2_2/status?jobId=abc&api-key=${KEY}`);
    const h = init.headers as Headers;
    expect([...h.keys()].sort()).toEqual(["spr-tee", "spr-tee-ciphertext", "spr-tee-enc", "x-sync"]);
  });

  it("sends indexer and prover paths to their own upstreams", async () => {
    mockFetch(() => Response.json({}));
    const e = env({ NETWORK: "local", HELIUS_API_KEY: undefined, UPSTREAM_INDEXER: "http://127.0.0.1:8784/", UPSTREAM_PROVER: "http://127.0.0.1:3001" });
    await call(post("/v1/spr/getUserRecords", { jsonrpc: "2.0" }), e);
    await call(req("/v1/spr/attestation?nonce=ab"), e);
    await call(post("/v1/spr/prove/merge_8_1/indexed", {}, { "spr-tee": "v1" }), e);
    expect(calls.map((c) => [c.init.method, c.url])).toEqual([
      ["POST", "http://127.0.0.1:8784/getUserRecords"],
      ["GET", "http://127.0.0.1:3001/attestation?nonce=ab"],
      ["POST", "http://127.0.0.1:3001/prove/merge_8_1/indexed"],
    ]);
    expect((calls[2]!.init.headers as Headers).get("content-type")).toBe("application/json");
  });

  it("allows only the SDK's paths and methods", async () => {
    mockFetch(() => Response.json({}));
    for (const p of ["/v1/spr/", "/v1/spr/getProgramAccounts", "/v1/spr/prove", "/v1/spr/prove/a%2Fb/status","/v1/spr/prove/A/status", "/v1/spr/admin", "/v1/other"]) {
      expect((await call(req(p))).res.status, p).toBe(404);
    }
    expect((await call(req("/v1/spr/getUserRecords"))).res.status).toBe(405);
    expect((await call(post("/v1/spr/health", {}))).res.status).toBe(405);
    expect((await call(req("/v1/spr/prove/transfer_confidential_1_2"))).res.status).toBe(405);
    expect(calls).toHaveLength(0);
    for (const p of ["/v1/spr/health", "/v1/spr/proving-keys", "/v1/spr/attestation"]) expect((await call(req(p))).res.status).toBe(200);
  });

  it("caps request and response at 1 MiB", async () => {
    mockFetch(() => new Response(new Uint8Array(1024 * 1024 + 65)));
    expect((await call(req("/v1/spr/health"))).res.status).toBe(502);
    expect((await call(post("/v1/spr/prove/merge_8_1", "x".repeat(1024 * 1024 + 65)))).res.status).toBe(413);
    mockFetch(() => new Response(new Uint8Array(1024 * 1024)));
    expect((await worker.fetch(req("/v1/spr/health"), env())).status).toBe(200);
  });

  it("passes prover error statuses through but never a body that holds the key", async () => {
    mockFetch(() => Response.json({ code: "busy" }, { status: 429, headers: { "retry-after": "2" } }));
    const a = await call(req("/v1/spr/attestation?nonce=1"));
    expect(a.res.status).toBe(429);
    expect(a.res.headers.get("retry-after")).toBe("2");
    mockFetch((url) => new Response(`denied ${url}`, { status: 403 }));
    expect((await call(req("/v1/spr/health"))).res.status).toBe(502);
    mockFetch(() => new Response(null, { status: 302, headers: { location: "https://elsewhere" } }));
    const c = await call(req("/v1/spr/health"));
    expect(c.res.status).toBe(502);
    expect(c.res.headers.get("location")).toBeNull();
  });
});

describe("origin and static", () => {
  it("refuses cross-origin requests to the API", async () => {
    mockFetch(() => Response.json({}));
    const { res } = await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }, { origin: "https://evil.example" }));
    expect(res.status).toBe(403);
    expect(res.headers.get("access-control-allow-origin")).toBeNull();
    expect((await call(post("/api/quotes", {}, { origin: "https://evil.example" }))).res.status).toBe(403);
    expect(calls).toHaveLength(0);
    expect((await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }, { origin: ORIGIN }))).res.status).toBe(200);
  });

  it("adds security headers to HTML and caches hashed assets", async () => {
    const html = await worker.fetch(req("/"), env());
    expect(html.headers.get("content-security-policy")).toContain("connect-src 'self'");
    expect(html.headers.get("content-security-policy")).toContain("'wasm-unsafe-eval'");
    expect(html.headers.get("x-content-type-options")).toBe("nosniff");
    expect(html.headers.get("referrer-policy")).toBe("no-referrer");
    expect(html.headers.get("cross-origin-opener-policy")).toBeNull();
    const STATIC = { fetch: async () => new Response("js", { headers: { "content-type": "text/javascript" } }) } as unknown as Fetcher;
    const js = await worker.fetch(req("/assets/index-AbC123xY.js"), env({ STATIC }));
    expect(js.headers.get("cache-control")).toBe("public, max-age=31536000, immutable");
    expect(js.headers.get("content-security-policy")).toBeNull();
    const wasm = await worker.fetch(req("/swap/swap_bg.wasm"), env({ STATIC }));
    expect(wasm.headers.get("content-type")).toBe("application/wasm");
    expect(wasm.headers.get("cache-control")).toBeNull();
  });
});

describe("/api/config and /api/tee-policy", () => {
  it("returns the public config only", async () => {
    const { body } = await call(req("/api/config"));
    expect(body).toEqual({
      network: "devnet", rpcPath: "/api/rpc", sprPath: "/v1/spr", programId: "F4hjig",
      assets: [{ mint: MINT_A, symbol: "tSOL", decimals: 9 }],
      dealers: [{ pubkey: "D1" }, { pubkey: "D2" }, { pubkey: "D3" }],
      bandBps: 100, minTtlSecs: 120, maxTtlSecs: 600,
      faucet: true, paused: false,
    });
    expect((await call(req("/api/config"), env({ FAUCET: undefined }))).body.faucet).toBe(false);
    expect((await call(req("/api/config"), env({ DEALERS: "[{" }))).res.status).toBe(500);
  });

  it("serves the network's signed TEE policy", async () => {
    mockFetch(() => Response.json({ release: {}, freshness: {} }));
    const { body } = await call(req("/api/tee-policy"));
    expect(body).toEqual({ release: {}, freshness: {} });
    expect(calls[0]!.url).toBe("https://d3gbdb0egjwcw9.cloudfront.net/tee-policy/devnet.json");
    expect((await call(req("/api/tee-policy"), env({ NETWORK: "local" }))).res.status).toBe(404);
  });
});

describe("/api/quotes", () => {
  const good = { sell_mint: MINT_A, buy_mint: MINT_B, sell_amount: "1000000", trader: TRADER };

  it("validates the body", async () => {
    mockFetch(() => Response.json({}));
    for (const bad of [
      { ...good, sell_mint: "0OIl" + MINT_A.slice(4) },
      { ...good, trader: "short" },
      { ...good, sell_amount: 1000 },
      { ...good, sell_amount: "18446744073709551616" },
      { ...good, sell_amount: "-1" },
      { ...good, sell_amount: "01" },
    ]) {
      expect((await call(post("/api/quotes", bad))).res.status).toBe(400);
    }
    expect((await call(post("/api/quotes", "[]"))).res.status).toBe(400);
    expect(calls).toHaveLength(0);
  });

  it("fans out in parallel with a timeout, in dealer order; bad replies are null", async () => {
    mockFetch((url, init) => {
      if (url.startsWith("https://d1")) return Response.json({ quote: { buy_amount: "18446744073709551615", expiry: 1700000000, signature: SIG, extra: 1 } });
      if (url.startsWith("https://d2")) {
        return new Promise((_, reject) => init.signal!.addEventListener("abort", () => reject(new Error("timeout"))));
      }
      throw new Error("refused");
    });
    const started = Date.now();
    const { body } = await call(post("/api/quotes", { ...good, extra: "dropped" }));
    expect(Date.now() - started).toBeLessThan(3900);
    expect(body).toEqual([
      { dealer: "D1", quote: { buy_amount: "18446744073709551615", expiry: 1700000000, signature: SIG } },
      { dealer: "D2", quote: null },
      { dealer: "D3", quote: null },
    ]);
    expect(calls.map((c) => c.url)).toEqual(["https://d1.example/quote", "https://d2.example/quote", "https://d3.example/quote"]);
    const h = calls[0]!.init.headers as Record<string, string>;
    expect(h["x-keeler-key"]).toBe(DKEY);
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual(good);
  });

  it("treats malformed and refusing replies as null", async () => {
    const replies = [
      Response.json({ error: "no quote" }),
      new Response("not json"),
      Response.json({ quote: { buy_amount: 5, expiry: 1 } }),
    ];
    mockFetch(() => replies.shift()!);
    const { body } = await call(post("/api/quotes", good));
    expect(body.map((q: { quote: unknown }) => q.quote)).toEqual([null, null, null]);
  });
});

describe("/api/faucet", () => {
  it("is 404 unless FAUCET is 1", async () => {
    mockFetch(() => Response.json({ ok: true }));
    expect((await call(post("/api/faucet", { wallet: TRADER }), env({ FAUCET: "0" }))).res.status).toBe(404);
    expect(calls).toHaveLength(0);
  });

  it("forwards the wallet to the first dealer", async () => {
    mockFetch(() => Response.json({ ok: true }));
    const { body } = await call(post("/api/faucet", { wallet: TRADER, other: 1 }));
    expect(body).toEqual({ ok: true });
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("https://d1.example/faucet");
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({ wallet: TRADER });
    expect((calls[0]!.init.headers as Record<string, string>)["x-keeler-key"]).toBe(DKEY);
    expect((await call(post("/api/faucet", { wallet: "nope" }))).res.status).toBe(400);
  });

  it("passes the dealer's refusal", async () => {
    mockFetch(() => Response.json({ error: "daily limit reached" }, { status: 429 }));
    const { res, body } = await call(post("/api/faucet", { wallet: TRADER }));
    expect(res.status).toBe(429);
    expect(body).toEqual({ error: "daily limit reached" });
  });
});

describe("round 2", () => {
  const good = { sell_mint: MINT_A, buy_mint: MINT_B, sell_amount: "1000000", trader: TRADER };

  it("passes a well-formed quote signature and nulls any other", async () => {
    const sigs: unknown[] = [SIG, SIG.toUpperCase(), SIG.slice(2), undefined];
    const list = sigs.map((_, i) => ({ url: `https://d${i}.example`, pubkey: `D${i}` }));
    mockFetch((url) => Response.json({ quote: { buy_amount: "5", expiry: 9, signature: sigs[Number(url[9])] } }));
    const { body } = await call(post("/api/quotes", good), env({ DEALERS: JSON.stringify(list) }));
    expect(body.map((q: { quote: unknown }) => q.quote)).toEqual([{ buy_amount: "5", expiry: 9, signature: SIG }, null, null, null]);
  });

  it("PAUSED stops quotes and faucet only", async () => {
    mockFetch(() => Response.json({ jsonrpc: "2.0", id: 1, result: 1 }));
    const e = env({ PAUSED: "1" });
    expect((await call(req("/api/config"), e)).body.paused).toBe(true);
    for (const r of [post("/api/quotes", good), post("/api/faucet", { wallet: TRADER })]) {
      const { res, body } = await call(r, e);
      expect(res.status).toBe(503);
      expect(body).toEqual({ error: "Swaps are paused." });
    }
    expect(calls).toHaveLength(0);
    expect((await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }), e)).res.status).toBe(200);
    expect((await call(post("/v1/spr/getUserRecords", {}), e)).res.status).toBe(200);
  });

  it("config assets carry maxSell; a bad maxSell is a config error", async () => {
    const a = [{ mint: MINT_A, symbol: "tSOL", decimals: 9, maxSell: "5000000000" }, { mint: MINT_B, symbol: "tUSD", decimals: 6 }];
    expect((await call(req("/api/config"), env({ ASSETS: JSON.stringify(a) }))).body.assets).toEqual(a);
    const bad = [{ mint: MINT_A, symbol: "tSOL", decimals: 9, maxSell: 5 }];
    expect((await call(req("/api/config"), env({ ASSETS: JSON.stringify(bad) }))).res.status).toBe(500);
  });

  it("limits proofs separately from indexer and status calls", async () => {
    mockFetch(() => Response.json({}));
    const seen: string[] = [];
    const named = (name: string, ok: boolean) => ({ limit: async () => (seen.push(name), { success: ok }) }) as RateLimit;
    const e = env({ SPR_LIMIT: named("spr", true), PROVE_LIMIT: named("prove", false), RPC_LIMIT: named("rpc", true) });
    expect((await call(post("/v1/spr/prove/merge_8_1", {}), e)).res.status).toBe(429);
    expect((await call(post("/v1/spr/prove/merge_8_1/indexed", {}), e)).res.status).toBe(429);
    expect((await call(req("/v1/spr/prove/merge_8_1/status?jobId=a"), e)).res.status).toBe(200);
    expect((await call(post("/v1/spr/getMerkleProofs", {}), e)).res.status).toBe(200);
    expect(seen).toEqual(["prove", "prove", "spr", "spr"]);
    expect(calls).toHaveLength(2);
    expect((await call(post("/v1/spr/getMerkleProofs", {}), env({ SPR_LIMIT: named("spr", false) }))).res.status).toBe(429);
  });

  it("allows indexer replies up to 10 MiB, prover replies up to 1 MiB", async () => {
    mockFetch(() => new Response(new Uint8Array(5 * 1024 * 1024)));
    expect((await worker.fetch(post("/v1/spr/getEncryptedUtxosByTags", {}), env())).status).toBe(200);
    expect((await worker.fetch(post("/v1/spr/prove/merge_8_1", {}), env())).status).toBe(502);
    mockFetch(() => new Response(new Uint8Array(10 * 1024 * 1024 + 1)));
    expect((await worker.fetch(post("/v1/spr/getEncryptedUtxosByTags", {}), env())).status).toBe(502);
  });
});

describe("/api/prices", () => {
  const FEED_A = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
  const FEED_B = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";
  let n = 0;
  const NOW = 1_800_000_000;
  // A fresh PYTH_URL per test keeps the module's 5 s cache from leaking between tests.
  const pythEnv = (over: Partial<Env> = {}) => env({
    PYTH_API_KEY: PKEY, PYTH_URL: `https://hermes${++n}.example/`,
    PYTH_FEEDS: JSON.stringify({ [MINT_A]: "0x" + FEED_A.toUpperCase(), [MINT_B]: FEED_B }),
    PYTH_MAX_AGE_SECS: "60", PYTH_MAX_CONF_BPS: "100", ...over,
  });
  const feed = (id: string, price: Record<string, unknown> = {}) =>
    ({ id, price: { price: "15000000000", conf: "1000000", expo: -8, publish_time: NOW - 5, ...price } });
  const hermes = (...parsed: unknown[]) => () => Response.json({ parsed });
  const at = (secs: number) => vi.spyOn(Date, "now").mockReturnValue(secs * 1000);

  it("returns REFERENCE values stamped now when no feeds are set", async () => {
    at(NOW);
    const { body } = await call(req("/api/prices"));
    expect(body).toEqual({ [MINT_A]: { price: "150", expo: -2, publishTime: NOW } });
  });

  it("refuses mainnet without a feed for every asset", async () => {
    at(NOW);
    const a = await call(req("/api/prices"), env({ NETWORK: "mainnet" }));
    expect(a.res.status).toBe(500);
    expect(a.body.error).toContain(MINT_A);
    const assets = JSON.stringify([{ mint: MINT_A, symbol: "tSOL", decimals: 9 }, { mint: TRADER, symbol: "X", decimals: 6 }]);
    expect((await call(req("/api/prices"), pythEnv({ NETWORK: "mainnet", ASSETS: assets }))).res.status).toBe(500);
  });

  it("reads Hermes with the bearer key and checks every feed", async () => {
    at(NOW);
    mockFetch(hermes(feed("0x" + FEED_A), feed(FEED_B, { price: "100000000", conf: "10000" })));
    const { res, body } = await call(req("/api/prices"), pythEnv());
    expect(res.status).toBe(200);
    expect(body).toEqual({
      [MINT_A]: { price: "15000000000", expo: -8, publishTime: NOW - 5 },
      [MINT_B]: { price: "100000000", expo: -8, publishTime: NOW - 5 },
    });
    expect(calls[0]!.url).toBe(`https://hermes${n}.example/v2/updates/price/latest?ids[]=${FEED_A}&ids[]=${FEED_B}&parsed=true`);
    expect((calls[0]!.init.headers as Record<string, string>).authorization).toBe(`Bearer ${PKEY}`);
  });

  it("is a 503 for each failed check, never a fixed fallback", async () => {
    at(NOW);
    const cases: (() => Response)[] = [
      () => new Response("down", { status: 500 }),
      () => { throw new Error(`fetch failed ${PKEY}`); },
      () => new Response("not json"),
      () => Response.json({}),
      hermes(feed(FEED_A)), // FEED_B missing
      hermes(feed(FEED_A), feed(FEED_B, { publish_time: NOW - 61 })), // stale
      hermes(feed(FEED_A), feed(FEED_B, { publish_time: NOW + 61 })), // future
      hermes(feed(FEED_A), feed(FEED_B, { price: "0" })),
      hermes(feed(FEED_A), feed(FEED_B, { price: "-5" })),
      hermes(feed(FEED_A), feed(FEED_B, { conf: "150000001" })), // over 100 bps of 150e8
      hermes(feed(FEED_A), feed(FEED_B, { price: 15 })),
      hermes(feed(FEED_A), feed(FEED_B), feed("ff".repeat(32), { publish_time: NOW - 999 })), // any bad feed fails all
    ];
    for (const [i, c] of cases.entries()) {
      mockFetch(c);
      const { res, body } = await call(req("/api/prices"), pythEnv());
      expect(res.status, `case ${i}`).toBe(503);
      expect(body.error).toBeTypeOf("string");
      expect(body[MINT_A]).toBeUndefined();
    }
    mockFetch(hermes(feed(FEED_A), feed(FEED_B, { conf: "150000000", publish_time: NOW + 60 }))); // edges pass
    expect((await call(req("/api/prices"), pythEnv())).res.status).toBe(200);
  });

  it("caches for 5 s and re-checks age on every read", async () => {
    const e = pythEnv();
    const clock = at(NOW);
    mockFetch(hermes(feed(FEED_A, { publish_time: NOW - 58 }), feed(FEED_B, { publish_time: NOW - 58 })));
    await call(req("/api/prices"), e);
    clock.mockReturnValue((NOW + 1) * 1000);
    await call(req("/api/prices"), e);
    expect(calls).toHaveLength(1);
    clock.mockReturnValue((NOW + 3) * 1000); // still inside 5 s but now 61 s old: fetched again
    await call(req("/api/prices"), e);
    expect(calls).toHaveLength(2);
  });
});
