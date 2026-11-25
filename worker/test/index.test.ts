import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import worker, { PRICE_UPDATE_V2, base58Key, type Env } from "../src/index";
// Signed requests the page's own signer made from fixed keys (web/src/keeler/lend-auth.ts); the keeper's tests use it too.
import fixture from "../../web/tests/fixtures/lend-requests.json";

const KEY = "HELIUSSECRET123";
const DKEY = "DEALERSECRET456";
const SIG = "ab".repeat(64);
const hexBytes = (h: string) => Uint8Array.from(h.match(/../g) ?? [], (b) => parseInt(b, 16));
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
    RPC_GLOBAL_LIMIT: allow,
    SPR_GLOBAL_LIMIT: allow,
    PROVE_GLOBAL_LIMIT: allow,
    ...over,
  };
}

const ORIGIN = "https://keeler.test";
function req(path: string, init: RequestInit & { headers?: Record<string, string> } = {}) {
  return new Request(ORIGIN + path, init);
}
// A browser POST from the page carries its Origin.
// A header set to undefined is left out.
const post = (path: string, body: unknown, headers: Record<string, string | undefined> = {}) =>
  req(path, {
    method: "POST",
    body: typeof body === "string" ? body : JSON.stringify(body),
    headers: Object.fromEntries(Object.entries({ "content-type": "application/json", origin: ORIGIN, ...headers }).filter((e): e is [string, string] => e[1] !== undefined)),
  });

async function call(r: Request, e = env()) {
  const res = await worker.fetch(r, e);
  const text = await res.text();
  expect(text).not.toContain(KEY);
  expect(text).not.toContain(DKEY);
  expect(text).not.toContain("helius-rpc.com");
  for (const [, v] of res.headers) expect(v).not.toContain(KEY);
  return { res, text, body: text ? JSON.parse(text) : null };
}

describe("/api/rpc", () => {
  it("forwards the validated call re-serialised, with the key added and hardened JSON headers", async () => {
    mockFetch(() => new Response('{"jsonrpc":"2.0","id":1,"result":12345678901234567890}', { headers: { "content-type": "text/html" } }));
    // Duplicate "method" (JSON.parse keeps the last) and extra keys must not reach upstream as sent.
    const raw = '{"jsonrpc":"2.0","id":1,"method":"requestAirdrop","method":"getBalance","params":["x",{"minContextSlot":5}],"extra":1}';
    const { res, text } = await call(post("/api/rpc", raw));
    expect(res.status).toBe(200);
    expect(text).toContain("12345678901234567890");
    expect(res.headers.get("content-type")).toBe("application/json");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    expect(res.headers.get("content-security-policy")).toBe("default-src 'none'");
    expect(calls[0]!.url).toBe(`https://devnet.helius-rpc.com/?api-key=${KEY}`);
    expect(calls[0]!.init.body).toBe('{"jsonrpc":"2.0","id":1,"method":"getBalance","params":["x",{"minContextSlot":5}]}');
  });

  it("refuses an integer that would lose precision when re-serialised", async () => {
    mockFetch(() => Response.json({}));
    const raw = '{"jsonrpc":"2.0","id":1,"method":"getBalance","params":["x",{"minContextSlot":18446744073709551615}]}';
    expect((await call(post("/api/rpc", raw))).body.error.code).toBe(-32602);
    expect(calls).toHaveLength(0);
  });

  it("caps batches at 10 and charges both limiters once per call", async () => {
    mockFetch(() => Response.json([]));
    const batch = (n: number) => Array.from({ length: n }, (_, i) => ({ jsonrpc: "2.0", id: i, method: "getSlot" }));
    const big = await call(post("/api/rpc", batch(11)));
    expect(big.body).toEqual({ jsonrpc: "2.0", id: null, error: { code: -32600, message: "Batch refused: more than 10 requests" } });
    expect(calls).toHaveLength(0);
    const charged: string[] = [];
    const count = (name: string) => ({ limit: async ({ key }: { key: string }) => (charged.push(`${name}:${key}`), { success: true }) }) as RateLimit;
    await call(post("/api/rpc", batch(10), { "cf-connecting-ip": "1.2.3.4" }), env({ RPC_LIMIT: count("ip"), RPC_GLOBAL_LIMIT: count("all") }));
    expect(charged).toEqual([...Array(10).fill("ip:1.2.3.4"), ...Array(10).fill("all:global")]);
    expect(calls).toHaveLength(1);
  });

  it("allows getProgramAccounts only for the registry, the pool and PROGRAM_ID", async () => {
    mockFetch(() => Response.json({}));
    const gpa = (program: string) => post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getProgramAccounts", params: [program, { encoding: "base64" }] });
    const PROGRAM = "F4hjigFfc2G6ubbR2ZsVLL4y5WmXTTXP9nLV4f4pLx1F";
    for (const p of ["regy3WAeRXMYuMYxr5iK3Bf9Arr3PcxGxyzAddbiqDQ", "ring1GaLCuJnkUMVkX2JeZLTTX17wKsV4fZmPVzXANW", PROGRAM]) {
      expect((await call(gpa(p), env({ PROGRAM_ID: PROGRAM }))).body.error).toBeUndefined();
    }
    expect(calls).toHaveLength(3);
    expect((await call(gpa("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"), env({ PROGRAM_ID: PROGRAM }))).body.error.code).toBe(-32602);
    expect((await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getProgramAccounts" }))).body.error.code).toBe(-32602);
    expect(calls).toHaveLength(3);
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
    expect(body.map((r: { id: number; error: { code: number } }) => [r.id, r.error.code])).toEqual([[1, -32600], [2, -32602], [3, -32601]]);
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
    // An override host never gets the Helius key, on any route.
    const e = env({ UPSTREAM_RPC: "http://10.0.0.1:8899", UPSTREAM_SPR: "https://spr.example/v1/spr" });
    await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }), e);
    await call(post("/v1/spr/getUserRecords", {}), e);
    await call(req("/v1/spr/attestation?nonce=1"), e);
    expect(calls.slice(1).map((c) => c.url)).toEqual(["http://10.0.0.1:8899", "https://spr.example/v1/spr/getUserRecords", "https://spr.example/v1/spr/attestation?nonce=1"]);
    for (const c of calls) expect(c.url).not.toContain(KEY);
  });

  it("rate limits per client: IPv4 as is, IPv6 by /64", async () => {
    mockFetch(() => Response.json({}));
    const keys: string[] = [];
    const deny = { limit: async ({ key }: { key: string }) => (keys.push(key), { success: false }) } as RateLimit;
    for (const ip of ["1.2.3.4", "2001:db8:a:b:1:2:3:4", "2001:DB8:a:b::99", "2001:db8::1", "::ffff:5.6.7.8"]) {
      const { res } = await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }, { "cf-connecting-ip": ip }), env({ RPC_LIMIT: deny }));
      expect(res.status).toBe(429);
    }
    expect(keys).toEqual(["1.2.3.4", "2001:db8:a:b::/64", "2001:db8:a:b::/64", "2001:db8:0:0::/64", "5.6.7.8"]);
    expect(calls).toHaveLength(0);
  });

  it("the global limiter stops everyone", async () => {
    mockFetch(() => Response.json({}));
    const full = { limit: async () => ({ success: false }) } as RateLimit;
    const { res, body } = await call(post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" }), env({ RPC_GLOBAL_LIMIT: full }));
    expect(res.status).toBe(429);
    expect(body.error).toContain("busy");
    expect(calls).toHaveLength(0);
  });

  it("fails closed without a limiter binding off local, and allows it on local", async () => {
    mockFetch(() => Response.json({}));
    const rpcCall = () => post("/api/rpc", { jsonrpc: "2.0", id: 1, method: "getSlot" });
    for (const missing of ["RPC_LIMIT", "RPC_GLOBAL_LIMIT"] as const) {
      const { res, body } = await call(rpcCall(), env({ [missing]: undefined }));
      expect(res.status).toBe(503);
      expect(body.error).toContain("Rate limiting");
    }
    expect((await call(post("/v1/spr/prove/merge_8_1", { circuitType: "merge" }), env({ PROVE_GLOBAL_LIMIT: undefined }))).res.status).toBe(503);
    expect((await call(post("/api/quotes", {}), env({ QUOTE_LIMIT: undefined }))).res.status).toBe(503);
    expect(calls).toHaveLength(0);
    const local = env({ NETWORK: "local", RPC_LIMIT: undefined, RPC_GLOBAL_LIMIT: undefined });
    expect((await call(rpcCall(), local)).res.status).toBe(200);
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
    expect(res.headers.get("content-type")).toBe("application/octet-stream");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    expect(res.headers.get("content-security-policy")).toBe("default-src 'none'");
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
    await call(post("/v1/spr/prove/merge_8_1/indexed", { circuitType: "merge" }, { "spr-tee": "v1" }), e);
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

  it("never passes on a body that holds a credential of PRICE_RPC_URL", async () => {
    const PATH_TOKEN = "pathtoken0123456789";
    const QUERY_TOKEN = "querytoken0123456789";
    const e = env({ PRICE_RPC_URL: `https://prices.example/${PATH_TOKEN}/?token=${QUERY_TOKEN}` });
    for (const token of [PATH_TOKEN, QUERY_TOKEN]) {
      mockFetch(() => Response.json({ echo: token }));
      expect((await call(req("/v1/spr/health"), e)).res.status).toBe(502);
    }
    mockFetch(() => Response.json({ ok: true }));
    expect((await call(req("/v1/spr/health"), e)).res.status).toBe(200);
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
    expect((await call(post("/v1/spr/prove/merge_8_1", { circuitType: "merge" }), e)).res.status).toBe(429);
    expect((await call(post("/v1/spr/prove/merge_8_1/indexed", { circuitType: "merge" }), e)).res.status).toBe(429);
    expect((await call(req("/v1/spr/prove/merge_8_1/status?jobId=a"), e)).res.status).toBe(200);
    expect((await call(post("/v1/spr/getMerkleProofs", {}), e)).res.status).toBe(200);
    expect(seen).toEqual(["prove", "prove", "spr", "spr"]);
    expect(calls).toHaveLength(2);
    expect((await call(post("/v1/spr/getMerkleProofs", {}), env({ SPR_LIMIT: named("spr", false) }))).res.status).toBe(429);
  });

  it("allows indexer replies up to 10 MiB, prover replies up to 1 MiB", async () => {
    mockFetch(() => new Response(new Uint8Array(5 * 1024 * 1024)));
    expect((await worker.fetch(post("/v1/spr/getEncryptedUtxosByTags", {}), env())).status).toBe(200);
    expect((await worker.fetch(post("/v1/spr/prove/merge_8_1", { circuitType: "merge" }), env())).status).toBe(502);
    mockFetch(() => new Response(new Uint8Array(10 * 1024 * 1024 + 1)));
    expect((await worker.fetch(post("/v1/spr/getEncryptedUtxosByTags", {}), env())).status).toBe(502);
  });
});

describe("proof requests and the global prover limit", () => {
  const ENC = "e1".repeat(32);
  it("charges the global limit only for a well-formed proof request", async () => {
    mockFetch(() => Response.json({}));
    const seen: string[] = [];
    const count = (name: string) => ({ limit: async () => (seen.push(name), { success: true }) }) as RateLimit;
    const e = env({ PROVE_LIMIT: count("client"), PROVE_GLOBAL_LIMIT: count("global") });
    const raw = (body: BodyInit, headers: Record<string, string>) => req("/v1/spr/prove/merge_8_1", { method: "POST", body, headers });
    for (const [r, status] of [
      [post("/v1/spr/prove/merge_8_1", "{nope"), 400],
      [post("/v1/spr/prove/merge_8_1", "[]"), 400],
      [post("/v1/spr/prove/merge_8_1", ""), 400],
      [raw("{}", { "content-type": "text/plain" }), 415],
      [raw("{}", {}), 415],
      [raw(new Uint8Array(64), { "content-type": "application/octet-stream", "spr-tee": "v1" }), 400],
      [raw(new Uint8Array(8), { "content-type": "application/octet-stream", "spr-tee": "v1", "spr-tee-enc": ENC }), 400],
      [raw(new Uint8Array(64), { "content-type": "application/octet-stream", "spr-tee": "v2", "spr-tee-enc": ENC }), 400],
      [raw(new Uint8Array(64), { "content-type": "application/octet-stream", "spr-tee": "v1", "spr-tee-enc": "ee" }), 400],
      [post("/v1/spr/prove/merge_8_1", {}), 400],
      [post("/v1/spr/prove/merge_8_1", { a: 1 }), 400],
      [post("/v1/spr/prove/merge_8_1", { circuitType: "a b" }), 400],
      [post("/v1/spr/prove/merge_8_1", "x".repeat(1024 * 1024 + 65)), 413],
    ] as const) {
      expect((await call(r, e)).res.status).toBe(status);
    }
    expect(seen).toEqual(Array(13).fill("client"));
    expect(calls).toHaveLength(0);
    seen.length = 0;
    expect((await call(post("/v1/spr/prove/merge_8_1", { circuitType: "merge", a: 1 }, { "content-type": "application/json; charset=utf-8" }), e)).res.status).toBe(200);
    expect((await call(raw(new Uint8Array(64), { "content-type": "application/octet-stream", "spr-tee": "v1", "spr-tee-enc": ENC }), e)).res.status).toBe(200);
    expect(seen).toEqual(["client", "global", "client", "global"]);
    expect(calls).toHaveLength(2);
  });

  it("refuses over the client limit before reading the request", async () => {
    mockFetch(() => Response.json({}));
    const seen: string[] = [];
    const named = (name: string, ok: boolean) => ({ limit: async () => (seen.push(name), { success: ok }) }) as RateLimit;
    const e = env({ PROVE_LIMIT: named("client", false), PROVE_GLOBAL_LIMIT: named("global", true) });
    expect((await call(post("/v1/spr/prove/merge_8_1", "{nope"), e)).res.status).toBe(429);
    expect(seen).toEqual(["client"]);
  });
});

describe("/api/lend/*", () => {
  const PROGRAM = fixture.program_id;
  const POOLS = [{
    mint: MINT_B,
    curve: [{ utilisationBps: 0, rateBps: 0 }, { utilisationBps: 8800, rateBps: 500 }, { utilisationBps: 9300, rateBps: 700 }, { utilisationBps: 10000, rateBps: 1500 }],
    feeBps: 1000, maxUtilisationBps: 9500, minSupply: "1000000",
  }];
  const VAULTS = [{
    id: 1, collateralMint: MINT_A, debtMint: MINT_B, cfBps: 8000, ltBps: 8500, lmlBps: 9000, penaltyBps: 100, minCollateral: "10000000", minDebt: "1000000",
  }];
  const LIMITS = ["LEND_READ_LIMIT", "LEND_READ_GLOBAL_LIMIT", "LEND_TICKET_LIMIT", "LEND_TICKET_GLOBAL_LIMIT", "LEND_BUILD_LIMIT"] as const;
  // Two keepers in failover order; the replies name the one that made them.
  const K1 = "GgBaCs3NCBuZN12kCJgAW63ydqohFkHEdfdEXBPzLHq";
  const K2 = "AKnL4NNf3DGWZJS6cPknBuEGnVsV4A4m5tgebLHaRSZ9";
  const KEEPERS = [{ url: "https://k1.example", pubkey: K1 }, { url: "https://k2.example/", pubkey: K2 }];
  const lendEnv = (over: Partial<Env> = {}) => env({
    LEND_PROGRAM_ID: PROGRAM, LEND_POOLS: JSON.stringify(POOLS), LEND_VAULTS: JSON.stringify(VAULTS), LEND_KEEPERS: JSON.stringify(KEEPERS),
    ASSETS: JSON.stringify([{ mint: MINT_A, symbol: "tSOL", decimals: 9 }, { mint: MINT_B, symbol: "tUSD", decimals: 6 }]),
    ...Object.fromEntries(LIMITS.map((n) => [n, allow])), ...over,
  });
  const [ACCOUNT_REQ, TICKET_REQ, BUILD_REQ] = fixture.requests as unknown as { method: string; path: string; fields: Record<string, string | number> }[];
  const ACCOUNT = ACCOUNT_REQ!.fields as Record<string, string | number> & { tickets: Record<string, unknown>[] };
  const TICKET = TICKET_REQ!.fields;
  const BUILD = BUILD_REQ!.fields;
  /** `body` signed by the fixture's session as the page signs it (docs/lend-api.md, "Request bytes"). */
  const sessionKey = crypto.subtle.importKey("pkcs8", Uint8Array.from([...hexBytes("302e020100300506032b657004220420"), ...hexBytes(fixture.session.seed_hex)]), { name: "Ed25519" }, false, ["sign"]);
  async function signed(path: string, body: Record<string, string | number>, nonce = "0e".repeat(16)) {
    const f: Record<string, string | number> = { ...body, wallet: fixture.wallet.address, origin: fixture.origin, sessionKey: fixture.session.key, sessionExpiry: fixture.session.expiry, sessionAuth: fixture.session.auth_hex, nonce, timestamp: fixture.now };
    const canonical = Object.keys(f).filter((k) => !["signature", "nonce", "timestamp"].includes(k)).sort().map((k) => `${k}=${f[k]}\n`).join("");
    const time = new Uint8Array(8);
    new DataView(time.buffer).setBigUint64(0, BigInt(fixture.now), true);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonical)));
    const message = Uint8Array.from([...new TextEncoder().encode(`keeler-lend-request-v1\nPOST\n${path}\n`), ...hexBytes(nonce), ...time, ...digest]);
    const sig = new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, await sessionKey, message));
    return { ...f, signature: [...sig].map((b) => b.toString(16).padStart(2, "0")).join("") };
  }
  let REDEEM: Record<string, string | number>;
  let REDEEM_AMOUNT: Record<string, string | number>;
  let REDEEM_RECEIPT: Record<string, string | number>;
  beforeAll(async () => {
    REDEEM = await signed("/lend/build", { op: "redeem", pool: MINT_B, shares: "5000000" });
    REDEEM_AMOUNT = await signed("/lend/build", { op: "redeem", pool: MINT_B, amount: "1000000" });
    REDEEM_RECEIPT = await signed("/lend/build", { op: "redeem", pool: MINT_B, shares: "5000000", receipt: "cd".repeat(32) });
  });
  const TX = "A".repeat(5464); // 4,096 bytes
  const OUT = "cd".repeat(32);
  const POOLS_REPLY = { pools: [{ mint: MINT_B, at: 1_800_000_000, size: "1000000000", utilisationBps: 5000, supplyRateBps: 120, borrowRateBps: 280, surplus: "0", badDebt: "0" }] };
  const ACCOUNT_REPLY = {
    receipts: [{ pool: MINT_B, shares: "5000000", message: "AAAA" }],
    positions: [{ vault: 0, position: "7", collateral: "10000000", debtShares: "0", message: "AAAA" }],
    intents: [{ id: "ab".repeat(16), purpose: "deposit", amount: "10000000", expiry: 1_800_000_600, status: "waiting" }],
    index: { supply: "1000000000000", borrow: "1000000000000", at: 1_800_000_000 },
  };
  const TICKET_REPLY = { id: "ab".repeat(16), expiry: 1_800_000_600, signature: SIG, keeper: K1 };
  const BUILD_REPLY = { transaction: TX, outputs: [OUT, OUT], now: 1_800_000_000, bound: "150000000000000", index: "1000000000000", keeper: K1 };
  const routes = () => [
    req("/api/lend/pools"),
    post("/api/lend/account", ACCOUNT),
    post("/api/lend/ticket", TICKET),
    post("/api/lend/build", BUILD),
  ];

  it("is absent from config and every route is 404 without the lend settings", async () => {
    mockFetch(() => Response.json({}));
    expect((await call(req("/api/config"))).body.lend).toBeUndefined();
    for (const r of routes()) expect((await call(r)).res.status).toBe(404);
    expect(calls).toHaveLength(0);
  });

  it("puts lend in config, and refuses malformed settings", async () => {
    expect((await call(req("/api/config"), lendEnv())).body.lend).toEqual({ programId: PROGRAM, pools: POOLS, vaults: VAULTS, keepers: [K1, K2] });
    // Without LEND_KEEPERS, today's single keeper: the first dealer's key.
    expect((await call(req("/api/config"), lendEnv({ LEND_KEEPERS: undefined }))).body.lend.keepers).toEqual(["D1"]);
    expect((await call(req("/api/config"), lendEnv({ LEND_KEEPERS: undefined, DEALERS: "[]" }))).body.lend.keepers).toEqual([]);
    const p = POOLS[0]!;
    const v = VAULTS[0]!;
    for (const bad of [
      { LEND_KEEPERS: "[]" },
      { LEND_KEEPERS: "[{" },
      { LEND_KEEPERS: JSON.stringify([KEEPERS[0], KEEPERS[0]]) }, // a duplicate key
      { LEND_KEEPERS: JSON.stringify([{ ...KEEPERS[0], pubkey: "D1" }]) },
      { LEND_KEEPERS: JSON.stringify([{ ...KEEPERS[0], url: "k1.example" }]) },
      { LEND_KEEPERS: JSON.stringify([{ ...KEEPERS[0], extra: 1 }]) },
      { LEND_KEEPERS: JSON.stringify([...KEEPERS, { url: "https://k3.example", pubkey: TRADER }, { url: "https://k4.example", pubkey: MINT_A }, { url: "https://k5.example", pubkey: MINT_B }]) }, // more than 4
      { LEND_PROGRAM_ID: "nope" },
      { LEND_POOLS: undefined },
      { LEND_VAULTS: "[]" },
      { LEND_POOLS: "[{" },
      { LEND_POOLS: JSON.stringify([{ ...p, extra: 1 }]) },
      { LEND_POOLS: JSON.stringify([{ ...p, minSupply: 1 }]) },
      { LEND_POOLS: JSON.stringify([{ ...p, curve: p.curve.slice(0, 3) }]) }, // does not reach 100 %
      { LEND_POOLS: JSON.stringify([{ ...p, curve: [p.curve[0], p.curve[2], p.curve[1], p.curve[3]] }]) }, // not rising
      { LEND_POOLS: JSON.stringify([{ ...p, feeBps: 10001 }]) },
      { LEND_POOLS: JSON.stringify([p, p]) },
      { LEND_VAULTS: JSON.stringify([{ ...v, debtMint: TRADER }]) }, // no such pool
      { LEND_VAULTS: JSON.stringify([{ ...v, cfBps: 8600 }]) }, // above the threshold
      { LEND_VAULTS: JSON.stringify([v, v]) },
      { LEND_VAULTS: JSON.stringify([{ ...v, minDebt: undefined }]) },
    ] as Partial<Env>[]) {
      expect((await call(req("/api/config"), lendEnv(bad))).res.status, JSON.stringify(bad)).toBe(500);
    }
  });

  it("forwards each route to the first keeper's path with the key, and returns the checked reply", async () => {
    const replies: Record<string, unknown> = {
      "/lend/pools": POOLS_REPLY,
      "/lend/account": ACCOUNT_REPLY,
      "/lend/ticket": TICKET_REPLY,
      "/lend/build": BUILD_REPLY,
    };
    mockFetch((url) => Response.json(replies[new URL(url).pathname]));
    const timeouts = vi.spyOn(AbortSignal, "timeout");
    const results = [];
    for (const r of routes()) results.push(await call(r, lendEnv()));
    expect(results.map((r) => r.res.status)).toEqual([200, 200, 200, 200]);
    expect(results.map((r) => r.body)).toEqual(Object.values(replies));
    expect(results[0]!.res.headers.get("content-security-policy")).toBe("default-src 'none'");
    expect(calls.map((c) => [c.init.method, c.url])).toEqual([
      ["GET", "https://k1.example/lend/pools"],
      ["POST", "https://k1.example/lend/account"],
      ["POST", "https://k1.example/lend/ticket"],
      ["POST", "https://k1.example/lend/build"],
    ]);
    for (const c of calls) expect((c.init.headers as Record<string, string>)["x-keeler-key"]).toBe(DKEY);
    expect(calls[0]!.init.body).toBeUndefined();
    // The account read carries the signed fields and the open tickets, exactly as the page sent them.
    expect(JSON.parse(calls[1]!.init.body as string)).toEqual(ACCOUNT);
    expect(ACCOUNT.tickets).toEqual([fixture.ticket.held]);
    expect(JSON.parse(calls[2]!.init.body as string)).toEqual(TICKET);
    expect(JSON.parse(calls[3]!.init.body as string)).toEqual(BUILD);
    expect(timeouts.mock.calls.map(([ms]) => ms)).toEqual([10000, 10000, 10000, 30000]);
    // A redeem of everything names its shares, and a partial one its amount.
    for (const body of [REDEEM, REDEEM_AMOUNT, REDEEM_RECEIPT]) {
      expect((await call(post("/api/lend/build", body), lendEnv())).res.status).toBe(200);
      expect(JSON.parse(calls.at(-1)!.init.body as string)).toEqual(body);
    }
  });

  it("refuses requests of the wrong shape without asking the keeper", async () => {
    mockFetch(() => Response.json({}));
    const e = lendEnv();
    const bad: Request[] = [
      req("/api/lend/pools?x=1"),
      post("/api/lend/account", {}),
      post("/api/lend/account", { ...ACCOUNT, extra: 1 }),
      post("/api/lend/account", { ...ACCOUNT, timestamp: "soon" }),
      post("/api/lend/account", { ...ACCOUNT, wallet: "nope" }),
      post("/api/lend/account", { ...ACCOUNT, tickets: undefined }),
      post("/api/lend/account", { ...ACCOUNT, tickets: [{ ...ACCOUNT.tickets[0], extra: 1 }] }),
      post("/api/lend/account", { ...ACCOUNT, tickets: [{ ...ACCOUNT.tickets[0], keeper: "nope" }] }),
      post("/api/lend/account", { ...ACCOUNT, tickets: Array(9).fill(ACCOUNT.tickets[0]) }),
      post("/api/lend/account", { ...ACCOUNT, origin: "javascript:alert(1)" }),
      post("/api/lend/ticket", { ...TICKET, origin: undefined }),
      post("/api/lend/ticket", { ...TICKET, extra: 1 }),
      post("/api/lend/ticket", { ...TICKET, amount: 5000000 }),
      post("/api/lend/ticket", { ...TICKET, amount: "18446744073709551616" }),
      post("/api/lend/ticket", { ...TICKET, purpose: "borrow" }),
      post("/api/lend/ticket", { ...TICKET, signature: SIG.toUpperCase() }),
      post("/api/lend/ticket", { ...TICKET, nonce: "0f" }),
      post("/api/lend/ticket", { ...TICKET, timestamp: -1 }),
      post("/api/lend/ticket", { ...TICKET, sessionKey: undefined }),
      post("/api/lend/ticket", { ...TICKET, sessionKey: "nope" }),
      post("/api/lend/ticket", { ...TICKET, sessionExpiry: "1800086340" }),
      post("/api/lend/ticket", { ...TICKET, sessionAuth: "ab" }),
      post("/api/lend/account", { ...ACCOUNT, sessionExpiry: "later" }),
      post("/api/lend/ticket", { ...TICKET, wallet: undefined }),
      post("/api/lend/ticket", "[]"),
      post("/api/lend/build", { ...BUILD, op: "liquidate" }),
      post("/api/lend/build", { ...BUILD, vault: 70000 }),
      post("/api/lend/build", { ...REDEEM, amount: "1000000", shares: "5000000" }),
      post("/api/lend/build", { ...ACCOUNT, tickets: undefined, op: "redeem", pool: MINT_B }),
      post("/api/lend/build", { ...REDEEM, shares: "5" + "0".repeat(20) }),
      post("/api/lend/build", { ...BUILD, shares: "5000000" }),
      post("/api/lend/build", { ...BUILD, receipt: OUT }), // a receipt names what a redeem spends
      post("/api/lend/build", { ...REDEEM, receipt: "cd" }),
      post("/api/lend/build", { ...BUILD, pad: "x".repeat(5 * 1024) }), // over the old 4 KiB cap: refused for shape, not size
    ];
    for (const r of bad) expect((await call(r, e)).res.status, r.url).toBe(400);
    expect((await call(post("/api/lend/build", { ...BUILD, pad: "x".repeat(16 * 1024) }), e)).res.status).toBe(413);
    expect((await call(req("/api/lend/ticket"), e)).res.status).toBe(405);
    expect((await call(req("/api/lend/account"), e)).res.status).toBe(405);
    expect((await call(post("/api/lend/pools", {}), e)).res.status).toBe(405);
    expect((await call(req("/api/lend/other"), e)).res.status).toBe(404);
    expect((await call(req("/api/lend/constructor"), e)).res.status).toBe(404);
    expect(calls).toHaveLength(0);
    // The existing dealer routes keep their 4 KiB cap.
    expect((await call(post("/api/quotes", { pad: "x".repeat(5 * 1024) }), e)).res.status).toBe(413);
  });

  it("refuses malformed and oversized keeper replies, and passes its refusals", async () => {
    const b = { transaction: TX, outputs: [OUT], now: 1, keeper: K1 };
    const cases: [Response | (() => never), number][] = [
      [Response.json({ ...b, transaction: "A".repeat(5468) }), 502], // over 4,096 bytes
      [Response.json({ ...b, outputs: [] }), 502],
      [Response.json({ ...b, extra: 1 }), 502],
      [Response.json({ ...b, transaction: "not base64!" }), 502],
      [Response.json({ ...b, keeper: undefined }), 502],
      [Response.json({ ...b, keeper: K2 }), 502], // the reply names a keeper other than the one asked
      [new Response("not json"), 502],
      [new Response(JSON.stringify({ ...b, bound: "1".padEnd(17 * 1024, " ") })), 502], // over 16 KiB
      [Response.json({ ...b, index: 5 }), 502],
      [Response.json({ error: "x" }, { status: 500 }), 502],
      [() => { throw new Error(`refused ${DKEY}`); }, 502],
      [Response.json({ error: "The reserve is busy for 12 s." }, { status: 409 }), 409],
    ];
    for (const [reply, status] of cases) {
      mockFetch(() => (typeof reply === "function" ? reply() : reply));
      const { res, body } = await call(post("/api/lend/build", BUILD), lendEnv());
      expect(res.status).toBe(status);
      expect(body.error).toBeTypeOf("string");
    }
    mockFetch(() => Response.json({ ...ACCOUNT_REPLY, receipts: [{ ...ACCOUNT_REPLY.receipts[0], shares: 5 }] }));
    expect((await call(post("/api/lend/account", ACCOUNT), lendEnv())).res.status).toBe(502);
  });

  it("charges the client limit first and the global limit only after validation and the signatures", async () => {
    mockFetch(() => Response.json(TICKET_REPLY));
    const seen: string[] = [];
    const named = (name: string, ok = true) => ({ limit: async ({ key }: { key: string }) => (seen.push(`${name}:${key}`), { success: ok }) }) as RateLimit;
    const e = lendEnv(Object.fromEntries(LIMITS.map((n) => [n, named(n)])));
    const ip = { "cf-connecting-ip": "2001:db8:a:b:1:2:3:4" };
    await call(post("/api/lend/ticket", { ...TICKET, extra: 1 }, ip), e);
    await call(post("/api/lend/ticket", { ...TICKET, signature: SIG }, ip), e);
    await call(post("/api/lend/ticket", TICKET, ip), e);
    await call(post("/api/lend/build", BUILD, ip), e);
    await call(req("/api/lend/pools", { headers: ip }), e);
    await call(post("/api/lend/account", {}, ip), e);
    expect(seen).toEqual([
      "LEND_TICKET_LIMIT:2001:db8:a:b::/64",
      "LEND_TICKET_LIMIT:2001:db8:a:b::/64",
      "LEND_TICKET_LIMIT:2001:db8:a:b::/64", "LEND_TICKET_GLOBAL_LIMIT:global",
      // A build has no global limit: the keeper's lane paces builds.
      "LEND_BUILD_LIMIT:2001:db8:a:b::/64",
      "LEND_READ_LIMIT:2001:db8:a:b::/64", "LEND_READ_GLOBAL_LIMIT:global",
      "LEND_READ_LIMIT:2001:db8:a:b::/64",
    ]);
    const route: Record<(typeof LIMITS)[number], number> = { LEND_READ_LIMIT: 0, LEND_READ_GLOBAL_LIMIT: 0, LEND_TICKET_LIMIT: 2, LEND_TICKET_GLOBAL_LIMIT: 2, LEND_BUILD_LIMIT: 3 };
    for (const n of LIMITS) {
      mockFetch(() => Response.json({}));
      const { res } = await call(routes()[route[n]]!, lendEnv({ [n]: named(n, false) }));
      expect(res.status, n).toBe(429);
      const missing = await call(routes()[route[n]]!, lendEnv({ [n]: undefined }));
      expect(missing.res.status, n).toBe(503);
      expect(missing.body.error).toContain("Rate limiting");
      expect(calls).toHaveLength(0);
    }
    mockFetch(() => Response.json(POOLS_REPLY));
    const local = lendEnv({ NETWORK: "local", ...Object.fromEntries(LIMITS.map((n) => [n, undefined])) });
    expect((await call(req("/api/lend/pools"), local)).res.status).toBe(200);
  });

  it("checks the wallet's authorisation and the session's signature before anything global or the keeper", async () => {
    mockFetch(() => Response.json(TICKET_REPLY));
    const seen: string[] = [];
    const named = (name: string) => ({ limit: async () => (seen.push(name), { success: true }) }) as RateLimit;
    const e = lendEnv(Object.fromEntries(LIMITS.map((n) => [n, named(n)])));
    const flip = (h: string) => (h[0] === "0" ? "1" : "0") + h.slice(1);
    const forged: Request[] = [
      post("/api/lend/ticket", { ...TICKET, signature: flip(String(TICKET.signature)) }),
      post("/api/lend/ticket", { ...TICKET, sessionAuth: flip(String(TICKET.sessionAuth)) }),
      post("/api/lend/ticket", { ...TICKET, amount: "250000001" }), // a field the signature covers
      post("/api/lend/ticket", { ...TICKET, nonce: "09".repeat(16) }), // and the nonce, by position
      post("/api/lend/ticket", { ...TICKET, sessionExpiry: Number(TICKET.sessionExpiry) + 1 }), // the authorised expiry
      post("/api/lend/ticket", { ...TICKET, wallet: TRADER }), // someone else's wallet
      post("/api/lend/build", { ...BUILD, timestamp: Number(BUILD.timestamp) + 1 }),
      post("/api/lend/build", { ...BUILD, signature: TICKET.signature }), // another request's signature
      post("/api/lend/account", { ...ACCOUNT, signature: SIG }),
    ];
    for (const r of forged) expect((await call(r, e)).res.status, r.url).toBe(401);
    // Authorised for another program or network.
    expect((await call(post("/api/lend/ticket", TICKET), lendEnv({ LEND_PROGRAM_ID: TRADER, ASSETS: lendEnv().ASSETS }))).res.status).toBe(401);
    expect((await call(post("/api/lend/ticket", TICKET), lendEnv({ NETWORK: "mainnet" }))).res.status).toBe(401);
    expect(calls).toHaveLength(0);
    expect(seen.every((n) => !n.includes("GLOBAL"))).toBe(true);
    // The page's own signed requests pass, byte for byte.
    for (const r of [post("/api/lend/account", ACCOUNT), post("/api/lend/ticket", TICKET), post("/api/lend/build", BUILD)]) {
      expect((await call(r, lendEnv())).res.status).not.toBe(401);
    }
  });

  it("never holds a repayment ticket back on the global ticket limit", async () => {
    mockFetch(() => Response.json(TICKET_REPLY));
    const full ={ limit: async () => ({ success: false }) } as RateLimit;
    const e = lendEnv({ LEND_TICKET_GLOBAL_LIMIT: full });
    const repay = await signed("/lend/ticket", { purpose: "repay", amount: "1000000", vault: 1, position: "7" });
    expect((await call(post("/api/lend/ticket", repay), e)).res.status).toBe(200);
    const deposit = await signed("/lend/ticket", { purpose: "deposit", amount: "1000000", vault: 1, position: "7" });
    expect((await call(post("/api/lend/ticket", deposit), e)).res.status).toBe(429);
    // Its own client limit still applies.
    expect((await call(post("/api/lend/ticket", repay), lendEnv({ LEND_TICKET_LIMIT: full }))).res.status).toBe(429);
  });

  it("without LEND_KEEPERS, sends lending to LEND_KEEPER_URL, else the first dealer", async () => {
    mockFetch(() => Response.json(POOLS_REPLY));
    expect((await call(req("/api/lend/pools"), lendEnv({ LEND_KEEPERS: undefined, LEND_KEEPER_URL: "https://keeper.example/" }))).res.status).toBe(200);
    expect(calls[0]!.url).toBe("https://keeper.example/lend/pools");
    expect((await call(req("/api/lend/pools"), lendEnv({ LEND_KEEPERS: undefined }))).res.status).toBe(200);
    expect(calls[1]!.url).toBe("https://d1.example/lend/pools");
    expect((await call(req("/api/lend/pools"), lendEnv({ LEND_KEEPERS: undefined, LEND_KEEPER_URL: "keeper.example" }))).res.status).toBe(500);
    // LEND_KEEPERS wins over LEND_KEEPER_URL.
    expect((await call(req("/api/lend/pools"), lendEnv({ LEND_KEEPER_URL: "https://keeper.example/" }))).res.status).toBe(200);
    expect(calls[2]!.url).toBe("https://k1.example/lend/pools");
  });

  it("asks the next keeper only when the first gave no reply or a reply that is not the keeper's", async () => {
    const k2 = (url: string) => new URL(url).host === "k2.example";
    const fine = (url: string) => Response.json({ ...TICKET_REPLY, keeper: k2(url) ? K2 : K1 });
    const firstFails: [string, () => Response][] = [
      ["no reply", () => { throw new TypeError("connection refused"); }],
      ["not JSON", () => new Response("<html>bad gateway</html>", { status: 502 })],
      ["an ok reply of the wrong shape", () => Response.json({ hello: 1 })],
      ["a reply naming another keeper", () => Response.json({ ...TICKET_REPLY, keeper: K2 })],
      ["an error without the keeper's JSON", () => Response.json({ message: "x" }, { status: 503 })],
    ];
    for (const [name, first] of firstFails) {
      mockFetch((url) => (k2(url) ? fine(url) : first()));
      const { res, body } = await call(post("/api/lend/ticket", TICKET), lendEnv());
      expect(res.status, name).toBe(200);
      expect(body.keeper, name).toBe(K2);
      expect(calls.map((c) => new URL(c.url).host), name).toEqual(["k1.example", "k2.example"]);
      expect(calls[1]!.init.body, name).toBe(calls[0]!.init.body);
    }
    // The keeper's own JSON answer stands, whatever its status: no second keeper.
    for (const [reply, status] of [
      [Response.json({ error: "Lending is paused for new deposits." }, { status: 403 }), 403],
      [Response.json({ error: "The reserve is busy for 12 s." }, { status: 409 }), 409],
      [Response.json({ error: "The keeper failed." }, { status: 500 }), 502],
    ] as const) {
      mockFetch((url) => (k2(url) ? fine(url) : reply.clone()));
      expect((await call(post("/api/lend/ticket", TICKET), lendEnv())).res.status).toBe(status);
      expect(calls).toHaveLength(1);
    }
    // Both fail: the last failure, once each.
    mockFetch(() => { throw new TypeError("down"); });
    expect((await call(post("/api/lend/ticket", TICKET), lendEnv())).res.status).toBe(502);
    expect(calls).toHaveLength(2);
    // One keeper only: no retry.
    mockFetch(() => { throw new TypeError("down"); });
    expect((await call(post("/api/lend/ticket", TICKET), lendEnv({ LEND_KEEPERS: JSON.stringify([KEEPERS[0]]) }))).res.status).toBe(502);
    expect(calls).toHaveLength(1);
  });

  it("tries every listed keeper in order, up to four", async () => {
    const four = [...KEEPERS, { url: "https://k3.example", pubkey: TRADER }, { url: "https://k4.example", pubkey: PROGRAM }];
    mockFetch((url) => (new URL(url).host === "k4.example" ? Response.json({ ...TICKET_REPLY, keeper: PROGRAM }) : (() => { throw new TypeError("down"); })()));
    const { res, body } = await call(post("/api/lend/ticket", TICKET), lendEnv({ LEND_KEEPERS: JSON.stringify(four) }));
    expect(res.status).toBe(200);
    expect(body.keeper).toBe(PROGRAM);
    expect(calls.map((c) => new URL(c.url).host)).toEqual(["k1.example", "k2.example", "k3.example", "k4.example"]);
  });

  it("passes over a keeper still starting inside its dealer, and says lending is starting when none is left", async () => {
    // The dealer's answer while its keeper replays history or cannot start (cli/src/keeper.rs `Slot`).
    const starting = () => Response.json({ starting: "the keeper is starting" }, { status: 503 });
    mockFetch((url) => (new URL(url).host === "k2.example" ? Response.json({ ...TICKET_REPLY, keeper: K2 }) : starting()));
    const ok = await call(post("/api/lend/ticket", TICKET), lendEnv());
    expect(ok.res.status).toBe(200);
    expect(ok.body.keeper).toBe(K2);
    mockFetch(starting);
    const { res, body } = await call(post("/api/lend/ticket", TICKET), lendEnv());
    expect(res.status).toBe(503);
    expect(body).toEqual({ error: "Lending is starting up. Try again in a minute." });
    expect(calls).toHaveLength(2);
  });

  it("refuses a NETWORK it does not know and a plain-http keeper or dealer off a local network", async () => {
    expect((await call(req("/api/config"), env({ NETWORK: "mainnet-beta" }))).res.status).toBe(500);
    mockFetch(() => Response.json(POOLS_REPLY));
    const plain = JSON.stringify([{ url: "http://k1.example", pubkey: K1 }]);
    expect((await call(req("/api/lend/pools"), lendEnv({ LEND_KEEPERS: plain }))).res.status).toBe(500);
    expect((await call(req("/api/lend/pools"), lendEnv({ LEND_KEEPERS: undefined, LEND_KEEPER_URL: "http://keeper.example" }))).res.status).toBe(500);
    expect((await call(req("/api/config"), env({ DEALERS: JSON.stringify([{ url: "http://d1.example", pubkey: K1 }]) }))).res.status).toBe(500);
    expect(calls).toHaveLength(0);
    expect((await call(req("/api/lend/pools"), lendEnv({ NETWORK: "local", LEND_KEEPERS: plain }))).res.status).toBe(200);
  });

  it("charges each limit once for a request that fails over", async () => {
    mockFetch((url) => (new URL(url).host === "k2.example" ? Response.json({ ...TICKET_REPLY, keeper: K2 }) : (() => { throw new TypeError("down"); })()));
    const seen: string[] = [];
    const named = (name: string) => ({ limit: async () => (seen.push(name), { success: true }) }) as RateLimit;
    const e = lendEnv(Object.fromEntries(LIMITS.map((n) => [n, named(n)])));
    expect((await call(post("/api/lend/ticket", TICKET), e)).res.status).toBe(200);
    expect(calls).toHaveLength(2);
    expect(seen).toEqual(["LEND_TICKET_LIMIT", "LEND_TICKET_GLOBAL_LIMIT"]);
  });

  it("LEND_PAUSED refuses new Earn deposits itself and forwards everything else marked paused", async () => {
    const replies: Record<string, unknown> = { "/lend/pools": POOLS_REPLY, "/lend/account": ACCOUNT_REPLY, "/lend/ticket": TICKET_REPLY, "/lend/build": BUILD_REPLY };
    mockFetch((url) => Response.json(replies[new URL(url).pathname]));
    const e = lendEnv({ LEND_PAUSED: "1" });
    expect(TICKET.purpose).toBe("supply");
    const { res, body } = await call(post("/api/lend/ticket", TICKET), e);
    expect(res.status).toBe(503);
    expect(body.error).toMatch(/paused/);
    expect(calls).toHaveLength(0);
    const repay = await signed("/lend/ticket", { purpose: "repay", amount: "1000000", vault: 1, position: "7" });
    const deposit = await signed("/lend/ticket", { purpose: "deposit", amount: "1000000", vault: 1, position: "7" });
    // Builds pass too: the keeper refuses only those that borrow (cli/src/keeper.rs, `x-keeler-paused`), so a
    // withdrawal from Earn or of collateral still works.
    const passed = [req("/api/lend/pools"), post("/api/lend/account", ACCOUNT), post("/api/lend/ticket", repay), post("/api/lend/ticket", deposit), post("/api/lend/build", BUILD), post("/api/lend/build", REDEEM)];
    for (const r of passed) {
      expect((await call(r, e)).res.status, r.url).toBe(200);
    }
    expect(calls.map((c) => (c.init.headers as Record<string, string>)["x-keeler-paused"])).toEqual(passed.map(() => "1"));
    // Not paused: no header.
    await call(req("/api/lend/pools"), lendEnv());
    expect((calls[passed.length]!.init.headers as Record<string, string>)["x-keeler-paused"]).toBeUndefined();
  });

  it("refuses a session authorised for another site", async () => {
    mockFetch(() => Response.json(TICKET_REPLY));
    // A session signed on another site, relayed with this site's Origin (the origin check runs before the signature's).
    const relayed = await call(post("/api/lend/ticket", { ...TICKET, origin: "https://evil.example" }), lendEnv());
    expect(relayed.res.status).toBe(401);
    expect(relayed.body.error).toContain("another site");
    // No Origin header at all, off a local build.
    const bare = await call(post("/api/lend/account", ACCOUNT, { origin: undefined }), lendEnv());
    expect(bare.res.status).toBe(401);
    expect(bare.body.error).toContain("another site");
    // A browser on another site never gets this far: the cross-origin guard answers first.
    expect((await call(post("/api/lend/ticket", TICKET, { origin: "https://evil.example" }), lendEnv())).res.status).toBe(403);
    expect(calls).toHaveLength(0);
    // A local build behind the dev proxy sends no Origin header.
    const local = await call(post("/api/lend/ticket", TICKET, { origin: undefined }), lendEnv({ NETWORK: "local" }));
    expect(local.body.error ?? "").not.toContain("another site"); // it gets on to the signature (signed for devnet)
  });

  it("refuses lend settings whose mints are not in ASSETS", async () => {
    expect((await call(req("/api/config"), lendEnv({ ASSETS: JSON.stringify([{ mint: MINT_A, symbol: "tSOL", decimals: 9 }]) }))).res.status).toBe(500);
    expect((await call(req("/api/config"), lendEnv({ ASSETS: JSON.stringify([{ mint: MINT_B, symbol: "tUSD", decimals: 6 }]) }))).res.status).toBe(500);
  });

  it("reads base58 keys of 32 bytes only", () => {
    expect(base58Key("11111111111111111111111111111111")).toEqual(new Uint8Array(32));
    expect(base58Key(fixture.wallet.address)?.length).toBe(32);
    expect(base58Key("1111111111111111111111111111111")).toBeNull(); // 31 bytes
    expect(base58Key("0OIl")).toBeNull();
  });

  it("PAUSED does not touch lending: every route still reaches the keeper, whose own pause answers", async () => {
    const replies: Record<string, unknown> = {
      "/lend/pools": POOLS_REPLY,
      "/lend/account": ACCOUNT_REPLY,
      "/lend/ticket": TICKET_REPLY,
      "/lend/build": BUILD_REPLY,
    };
    mockFetch((url) => Response.json(replies[new URL(url).pathname]));
    const e = lendEnv({ PAUSED: "1" });
    for (const r of routes()) expect((await call(r, e)).res.status).toBe(200);
    expect(calls).toHaveLength(4);
    mockFetch(() => Response.json({ error: "Lending is paused for new deposits and borrowing." }, { status: 403 }));
    const { res, body } = await call(post("/api/lend/ticket", TICKET), e);
    expect(res.status).toBe(403);
    expect(body.error).toMatch(/paused/);
  });

  it("leaves the page CSP unchanged", async () => {
    const html = await worker.fetch(req("/"), lendEnv());
    expect(html.headers.get("content-security-policy")).toBe(
      "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; worker-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'",
    );
  });
});

describe("/api/prices", () => {
  const FEED_A = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
  const FEED_B = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";
  const ACC_A = "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE";
  const ACC_B = "Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX";
  const OWNER = "rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ";
  const NOW = 1_800_000_000;
  const TWO = JSON.stringify([{ mint: MINT_A, symbol: "SOL", decimals: 9 }, { mint: MINT_B, symbol: "USDC", decimals: 6 }]);
  let n = 0;
  // A fresh UPSTREAM_RPC per test keeps the module's 5 s cache from leaking between tests.
  const pythEnv = (over: Partial<Env> = {}) => env({
    UPSTREAM_RPC: `https://rpc${++n}.example/`, ASSETS: TWO,
    PYTH_ACCOUNTS: JSON.stringify({ [MINT_A]: { account: ACC_A, feedId: "0x" + FEED_A.toUpperCase() }, [MINT_B]: { account: ACC_B, feedId: FEED_B } }),
    PYTH_MAX_AGE_SECS: "60", PYTH_MAX_CONF_BPS: "100", ...over,
  });

  // A 134-byte PriceUpdateV2 account, Full verification.
  type Fields = { disc?: number[]; feed?: string; level?: number; price?: bigint; conf?: bigint; expo?: number; published?: bigint; owner?: string };
  function account(f: Fields = {}) {
    const data = new Uint8Array(134);
    data.set(f.disc ?? PRICE_UPDATE_V2);
    const v = new DataView(data.buffer);
    data[40] = f.level ?? 1;
    const feed = f.feed ?? FEED_A;
    for (let i = 0; i < 32; i++) data[41 + i] = parseInt(feed.slice(2 * i, 2 * i + 2), 16);
    v.setBigInt64(73, f.price ?? 15_000_000_000n, true);
    v.setBigUint64(81, f.conf ?? 1_000_000n, true);
    v.setInt32(89, f.expo ?? -8, true);
    v.setBigInt64(93, f.published ?? BigInt(NOW - 25), true);
    return { owner: f.owner ?? OWNER, lamports: 1, executable: false, data: [btoa(String.fromCharCode(...data)), "base64"] };
  }
  const rpcReply = (...value: unknown[]) => () => Response.json({ jsonrpc: "2.0", id: 1, result: { context: { slot: 1 }, value } });
  const goodB = (f: Fields = {}) => account({ feed: FEED_B, price: 100_000_000n, conf: 10_000n, ...f });
  const at = (secs: number) => vi.spyOn(Date, "now").mockReturnValue(secs * 1000);

  it("returns REFERENCE values stamped now when no accounts are set", async () => {
    at(NOW);
    const { body } = await call(req("/api/prices"));
    expect(body).toEqual({ [MINT_A]: { price: "150", expo: -2, publishTime: NOW, fixed: true } });
  });

  it("hard-codes the right PriceUpdateV2 discriminator", async () => {
    const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode("account:PriceUpdateV2")));
    expect([...hash.subarray(0, 8)]).toEqual(PRICE_UPDATE_V2);
  });

  it("refuses mainnet without an account for every asset", async () => {
    at(NOW);
    const a = await call(req("/api/prices"), env({ NETWORK: "mainnet" }));
    expect(a.res.status).toBe(500);
    expect(a.body.error).toContain(MINT_A);
    const three = JSON.stringify([...JSON.parse(TWO), { mint: TRADER, symbol: "X", decimals: 6 }]);
    expect((await call(req("/api/prices"), pythEnv({ NETWORK: "mainnet", ASSETS: three }))).res.status).toBe(500);
  });

  it("reads mainnet prices only through PRICE_RPC_URL, which must be https and not a Helius host", async () => {
    at(NOW);
    const main = (over: Partial<Env> = {}) => pythEnv({ NETWORK: "mainnet", UPSTREAM_RPC: undefined, ...over });
    const missing = await call(req("/api/prices"), main());
    expect(missing.res.status).toBe(500);
    expect(missing.body.error).toMatch(/PRICE_RPC_URL is required on mainnet/);
    for (const url of ["https://mainnet.helius-rpc.com/", "https://beta.helius-rpc.com/", "https://x.helius.xyz/", "https://mainnet.helius-rpc.com./", "https://helius.dev../", "http://prices.example/", "not a url"]) {
      expect((await call(req("/api/prices"), main({ PRICE_RPC_URL: url }))).res.status, url).toBe(500);
    }
    mockFetch(rpcReply(account(), goodB()));
    const priceUrl = `https://prices${n}.example/secret0123456789`;
    expect((await call(req("/api/prices"), main({ PRICE_RPC_URL: priceUrl }))).res.status).toBe(200);
    expect(calls.map((c) => c.url)).toEqual([priceUrl]);
  });

  it("reads all accounts in one getMultipleAccounts call", async () => {
    at(NOW);
    mockFetch(rpcReply(account(), goodB()));
    const { res, body } = await call(req("/api/prices"), pythEnv());
    expect(res.status).toBe(200);
    expect(body).toEqual({
      [MINT_A]: { price: "15000000000", expo: -8, publishTime: NOW - 25 },
      [MINT_B]: { price: "100000000", expo: -8, publishTime: NOW - 25 },
    });
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe(`https://rpc${n}.example/`); // override host: no Helius key
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({
      jsonrpc: "2.0", id: 1, method: "getMultipleAccounts", params: [[ACC_A, ACC_B], { encoding: "base64", commitment: "confirmed" }],
    });
  });

  it("is a 503 naming the asset for each failed check, never a fixed fallback", async () => {
    at(NOW);
    const cases: [string, () => Response][] = [
      ["USDC", rpcReply(account(), null)], // missing account
      ["USDC", rpcReply(account(), goodB({ owner: "11111111111111111111111111111111" }))],
      ["USDC", rpcReply(account(), goodB({ level: 0 }))], // Partial
      ["USDC", rpcReply(account(), goodB({ feed: FEED_A }))],
      ["USDC", rpcReply(account(), goodB({ published: BigInt(NOW - 61) }))],
      ["USDC", rpcReply(account(), goodB({ published: BigInt(NOW + 61) }))],
      ["USDC", rpcReply(account(), goodB({ conf: 1_000_001n }))], // over 100 bps of 1e8
      ["SOL", rpcReply(account({ price: 0n }), goodB())],
      ["SOL", rpcReply(account({ price: -5n }), goodB())],
      ["SOL", rpcReply({ ...account(), data: ["AAAA", "base64"] }, goodB())], // too short
      ["SOL", rpcReply(account({ disc: [0, 0, 0, 0, 0, 0, 0, 0] }), goodB())], // another account type
      ["", rpcReply(account())], // fewer accounts than asked
      ["", () => new Response("down", { status: 500 })],
      ["", () => { throw new Error(`fetch failed ${KEY}`); }],
    ];
    for (const [i, [name, c]] of cases.entries()) {
      mockFetch(c);
      const { res, body } = await call(req("/api/prices"), pythEnv());
      expect(res.status, `case ${i}`).toBe(503);
      expect(body.error).toContain(name);
      expect(body[MINT_A]).toBeUndefined();
    }
    mockFetch(rpcReply(account(), goodB({ conf: 1_000_000n, published: BigInt(NOW + 60) }))); // edges pass
    expect((await call(req("/api/prices"), pythEnv())).res.status).toBe(200);
  });

  it("caches for 5 s and re-checks age on every read", async () => {
    const e = pythEnv();
    const clock = at(NOW);
    const old = BigInt(NOW - 58);
    mockFetch(rpcReply(account({ published: old }), goodB({ published: old })));
    expect((await call(req("/api/prices"), e)).res.status).toBe(200);
    clock.mockReturnValue((NOW + 1) * 1000);
    await call(req("/api/prices"), e);
    expect(calls).toHaveLength(1);
    clock.mockReturnValue((NOW + 3) * 1000); // still inside 5 s but now 61 s old: read again
    await call(req("/api/prices"), e);
    expect(calls).toHaveLength(2);
  });
});
