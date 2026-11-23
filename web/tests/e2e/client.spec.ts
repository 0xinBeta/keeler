// The swap client's refusals on the local network, through the real Worker and the real dealer.
// The harness page (tests/harness) exposes the client; faults the real stack cannot produce on
// demand (a paused or altered config, prices down, a bad signature, a full storage) are injected
// at the browser's network layer with page.route, so the client sees exactly what a bad or
// compromised server would send. One wallet for the whole file keeps faucet calls to one.
import { expect, test, type Page } from "@playwright/test";
import type { Keeler, KeelerError } from "../../src/keeler/index";

declare global {
  interface Window {
    keeler: Keeler;
    KeelerError: typeof KeelerError;
    ready: boolean;
  }
}

const SELL = 20_000_000n; // 20 tUSD

test.describe.configure({ mode: "serial" });
let page: Page;

/** Opens the harness, connects and unlocks the in-page wallet (config is read once per page load). */
async function open() {
  await page.goto("/tests/harness/index.html");
  await page.waitForFunction(() => window.ready);
  await page.evaluate(async () => {
    const k = window.keeler;
    await k.connect(k.wallets().find((w) => w.name.includes("in-page"))!);
    await k.unlock();
  });
}

/** Serves /api/config with `patch` applied, then reloads so the client reads it. */
async function withConfig(patch: (c: Record<string, any>) => void) {
  await page.route("**/api/config", async (route) => {
    const res = await route.fetch();
    const c = await res.json();
    patch(c);
    await route.fulfill({ response: res, json: c });
  });
  await open();
}

/** The KeelerError code and message a call in the page throws, or "ok". */
function codeOf(body: string) {
  return page.evaluate(async (src) => {
    try {
      await (0, eval)(`(async (k) => { ${src} })`)(window.keeler);
      return "ok";
    } catch (e) {
      return e instanceof window.KeelerError ? `${e.code}: ${e.message}` : `other: ${String(e)}`;
    }
  }, body);
}

const quote = `const [s, b] = (await k.config()).assets; return k.quotes(s, b, ${SELL}n);`;
const privateUsd = () => page.evaluate(async () => String((await window.keeler.balances())[0]!.private));
const orderCount = () => page.evaluate(async () => (await window.keeler.orders()).length);

test.beforeAll(async ({ browser }) => {
  page = await browser.newPage();
  page.on("pageerror", (e) => console.log("[pageerror]", e.message));
  await open();
  await page.evaluate(async () => {
    const k = window.keeler;
    await k.faucet();
    await k.register();
    const [usd] = (await k.config()).assets;
    await k.deposit(usd!, 50_000_000n);
  });
});

test.afterEach(async () => {
  await page.unrouteAll({ behavior: "wait" });
});

test("the real stack quotes: signed, in band, inside the lock-time window", async () => {
  await open();
  const r = await page.evaluate(async (sell) => {
    const k = window.keeler;
    const [s, b] = (await k.config()).assets;
    const { quotes } = await k.quotes(s!, b!, BigInt(sell));
    return quotes.map((q) => q.inBand);
  }, String(SELL));
  expect(r).toEqual([true]);
});

test("paused: no quotes, balances still read", async () => {
  await withConfig((c) => (c.paused = true));
  expect(await codeOf(quote)).toMatch(/^paused: Swaps are paused/);
  expect(await privateUsd()).toBe("50000000");
});

test("quotes answering 503 while the config is not paused: dealer offline", async () => {
  await open();
  await page.route("**/api/quotes", (route) => route.fulfill({ status: 503, json: { error: "paused" } }));
  expect(await codeOf(quote)).toMatch(/^dealer-offline:/);
});

test("reference prices down: no quotes", async () => {
  await open();
  await page.route("**/api/prices", (route) => route.fulfill({ status: 503, json: { error: "down" } }));
  expect(await codeOf(quote)).toMatch(/^network: Reference prices are unavailable/);
});

test("a dealer signature that does not verify: no usable quote", async () => {
  await open();
  await page.route("**/api/quotes", async (route) => {
    const res = await route.fetch();
    const replies = await res.json();
    for (const r of replies) if (r.quote) r.quote.signature = (r.quote.signature[0] === "0" ? "1" : "0") + r.quote.signature.slice(1);
    await route.fulfill({ response: res, json: replies });
  });
  expect(await codeOf(quote)).toMatch(/^no-quotes: No usable quote/);
});

test("above the per-order limit: refused before asking", async () => {
  await withConfig((c) => (c.assets[0].maxSell = String(SELL - 1n)));
  expect(await codeOf(quote)).toMatch(/^no-quotes: The most one order can sell/);
});

test("a config listing other dealers than VITE_DEALERS: refused", async () => {
  await withConfig((c) => (c.dealers = [{ pubkey: "11111111111111111111111111111112" }]));
  expect(await codeOf(quote)).toMatch(/^no-quotes: The service lists different dealers/);
});

test("the dealer's 180 s quote under a 200 s minimum lock time: no usable quote", async () => {
  await withConfig((c) => (c.minTtlSecs = 200));
  expect(await codeOf(quote)).toMatch(/^no-quotes: No usable quote/);
});

test("out of band: shown as such, and swap refuses it before anything is sent", async () => {
  await open();
  // The page's reference price for tSOL 5% above the dealer's, so its quote is 5% short of fair.
  const solMint = await page.evaluate(async () => (await window.keeler.config()).assets[1]!.mint);
  await page.route("**/api/prices", async (route) => {
    const res = await route.fetch();
    const p = await res.json();
    const sol = p[solMint];
    sol.price = String((BigInt(sol.price) * 95n) / 100n);
    await route.fulfill({ response: res, json: p });
  });
  const before = await privateUsd();
  const r = await page.evaluate(async (sell) => {
    const k = window.keeler;
    const [s, b] = (await k.config()).assets;
    const { quotes } = await k.quotes(s!, b!, BigInt(sell));
    const inBand = quotes[0]!.inBand;
    const swap = await k.swap(quotes[0]!, () => {}).then(
      () => "ok",
      (e) => (e instanceof window.KeelerError ? e.code : String(e)),
    );
    return { inBand, swap };
  }, String(SELL));
  expect(r).toEqual({ inBand: false, swap: "out-of-band" });
  expect(await privateUsd()).toBe(before);
  expect(await orderCount()).toBe(0);
});

test("the order record cannot be saved: the swap stops before signing", async () => {
  await open();
  const noStore = await codeOf(
    `const put = Storage.prototype.setItem;
     Storage.prototype.setItem = function (key, value) { if (String(key).startsWith("keeler:order:")) throw new Error("quota"); return put.call(this, key, value); };
     try { const [s, b] = (await k.config()).assets; const { quotes } = await k.quotes(s, b, ${SELL}n); await k.swap(quotes[0], () => {}); }
     finally { Storage.prototype.setItem = put; }`,
  );
  expect(noStore).toMatch(/^storage: Could not save the order/);
  expect(await privateUsd()).toBe("50000000");
  expect(await orderCount()).toBe(0);
});

test("a second swap while one is in flight is refused; the first settles", async () => {
  await open();
  const r = await page.evaluate(async (sell) => {
    const k = window.keeler;
    const [s, b] = (await k.config()).assets;
    const { quotes } = await k.quotes(s!, b!, BigInt(sell));
    const first = k.swap(quotes[0]!, () => {});
    const second = await k.swap(quotes[0]!, () => {}).then(
      () => "ok",
      (e) => (e instanceof window.KeelerError ? e.code : String(e)),
    );
    const o = await first;
    return { second, id: o.id };
  }, String(SELL));
  expect(r.second).toBe("busy");
  await expect
    .poll(() => page.evaluate(async (id) => (await window.keeler.orders()).find((o) => o.id === id)?.status, r.id), { timeout: 120_000, intervals: [3_000] })
    .toBe("settled");
  expect(await privateUsd()).toBe(String(50_000_000n - SELL));
});
