// The swap client in Chromium on the local network (tests/localnet), behind
// the stand-in Worker (tests/standin.ts). Each test starts with a new in-page
// wallet: faucet, unlock, register, deposit.
import { expect, test, type Page } from "@playwright/test";
import type { Keeler, KeelerError } from "../../src/keeler/index";

declare global {
  interface Window {
    keeler: Keeler;
    KeelerError: typeof KeelerError;
    proofMs: () => number;
    ready: boolean;
  }
}

const DEPOSIT = 50_000_000n;
const SELL = 20_000_000n;

async function set(page: Page, switches: Record<string, unknown>) {
  const res = await page.request.post("/__test/set", { data: switches });
  expect(res.ok()).toBe(true);
}

async function open(page: Page) {
  page.on("pageerror", (e) => console.log("[pageerror]", e.message));
  await page.goto("/tests/harness/index.html");
  await page.waitForFunction(() => window.ready);
  await page.evaluate(async () => {
    const k = window.keeler;
    await k.connect(k.wallets().find((w) => w.name.includes("in-page"))!);
  });
}

async function setup(page: Page) {
  await set(page, { idle: false, paused: false, badSignature: false, pricesDown: false, maxSell: null });
  await open(page);
  await page.evaluate(
    async ({ deposit }) => {
      const k = window.keeler;
      await k.faucet();
      await k.unlock();
      if (!(await k.isRegistered())) await k.register();
      const [spl] = (await k.config()).assets;
      await k.deposit(spl!, BigInt(deposit));
    },
    { deposit: String(DEPOSIT) },
  );
}

/** Runs one swap of SELL tSPL for SOL with the best quote. */
function swap(page: Page) {
  return page.evaluate(async ({ sell }) => {
    const k = window.keeler;
    const [spl, sol] = (await k.config()).assets;
    const { fair, priceTime, quotes } = await k.quotes(spl!, sol!, BigInt(sell));
    const steps: string[] = [];
    const o = await k.swap(quotes[0]!, (s) => steps.push(s));
    return { fair: String(fair), priceTime, dealer: quotes[0]!.dealer, id: o.id, status: o.status, expiry: o.expiry, buy: String(o.buyAmount), steps, proofMs: window.proofMs() };
  }, { sell: String(SELL) });
}

/** The KeelerError code (and message) a call in the page throws, or "ok". */
function codeOf(page: Page, body: string) {
  return page.evaluate(async (src) => {
    try {
      await (0, eval)(`(async (k) => { ${src} })`)(window.keeler);
      return "ok";
    } catch (e) {
      return e instanceof window.KeelerError ? `${e.code}: ${e.message}` : `other: ${String(e)}`;
    }
  }, body);
}

const status = (page: Page, id: string) =>
  page.evaluate(async (id) => (await window.keeler.orders()).find((o) => o.id === id)?.status, id);

const privateBalance = (page: Page, i: number) =>
  page.evaluate(async (i) => String((await window.keeler.balances())[i]!.private), i);

async function waitPastExpiry(page: Page, expiry: number) {
  await expect.poll(() => page.evaluate(() => window.keeler.chainNow()), { timeout: 120_000, intervals: [5_000] }).toBeGreaterThan(expiry);
}

test("swap, then refund after expiry (the idle dealer never settles)", async ({ page }) => {
  await setup(page);
  await set(page, { idle: true });
  const o = await swap(page);
  console.log("swap", o);
  expect(o.steps).toEqual(["checking", "proving", "signing", "locked"]);
  expect(o.status).toBe("locked");
  expect(await privateBalance(page, 0)).toBe(String(DEPOSIT - SELL));
  // The signed quote is kept with the order.
  const sig = await page.evaluate((id) => {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i)!;
      if (key.startsWith("keeler:order:") && key.endsWith(id)) return JSON.parse(localStorage.getItem(key)!).signature;
    }
  }, o.id);
  expect(sig).toMatch(/^[0-9a-f]{128}$/);

  await waitPastExpiry(page, o.expiry);
  await expect.poll(() => status(page, o.id), { timeout: 60_000, intervals: [5_000] }).toBe("refundable");
  await page.evaluate((id) => window.keeler.refund(id), o.id);
  expect(await status(page, o.id)).toBe("refunded");
  expect(await privateBalance(page, 0)).toBe(String(DEPOSIT));
});

test("swap, then the dealer settles", async ({ page }) => {
  await setup(page);
  const solBefore = BigInt(await privateBalance(page, 1));
  const o = await swap(page);
  console.log("swap", o);
  expect(o.status).toBe("locked");
  await expect.poll(() => status(page, o.id), { timeout: 120_000, intervals: [5_000] }).toBe("settled");
  expect(BigInt(await privateBalance(page, 1))).toBe(solBefore + BigInt(o.buy));
  expect(await privateBalance(page, 0)).toBe(String(DEPOSIT - SELL));
});

test("an order whose record was lost is found on chain and refunded", async ({ page }) => {
  await setup(page);
  await set(page, { idle: true });
  const o = await swap(page);
  const removed = await page.evaluate(() => {
    const keys = Array.from({ length: localStorage.length }, (_, i) => localStorage.key(i)!).filter((k) => k.startsWith("keeler:order:"));
    keys.forEach((k) => localStorage.removeItem(k));
    return { removed: keys.length, left: localStorage.length };
  });
  expect(removed.removed).toBe(1);
  expect(removed.left).toBeGreaterThan(0); // the in-page wallet key stays

  await page.reload();
  await page.waitForFunction(() => window.ready);
  await page.evaluate(async () => {
    const k = window.keeler;
    await k.connect(k.wallets().find((w) => w.name.includes("in-page"))!);
    await k.unlock(); // runs the scan once
  });
  const list = (rescan: boolean) =>
    page.evaluate(async (rescan) => {
      const k = window.keeler;
      return (await (rescan ? k.rescanOrders() : k.orders())).map((x) => ({ id: x.id, status: x.status, sell: String(x.sellAmount), buy: String(x.buyAmount), expiry: x.expiry, sym: x.sell.symbol }));
    }, rescan);
  const want = [{ id: o.id, status: "locked", sell: String(SELL), buy: o.buy, expiry: o.expiry, sym: "tSPL" }];
  const afterUnlock = await list(false);
  console.log("found by the unlock scan", afterUnlock);
  // The indexer can lag the chain by seconds; "Find my orders" then finds it.
  if (afterUnlock.length === 0) await expect.poll(() => list(true), { timeout: 60_000, intervals: [3_000] }).toEqual(want);
  else expect(afterUnlock).toEqual(want);

  await waitPastExpiry(page, o.expiry);
  await expect.poll(() => status(page, o.id), { timeout: 60_000, intervals: [5_000] }).toBe("refundable");
  await page.evaluate((id) => window.keeler.refund(id), o.id);
  expect(await status(page, o.id)).toBe("refunded");
  expect(await privateBalance(page, 0)).toBe(String(DEPOSIT));
  // A refunded order is final: rescanning keeps it and reads nothing more for it.
  expect((await page.evaluate(() => window.keeler.rescanOrders())).map((x) => x.status)).toEqual(["refunded"]);
});

test("safety rules refuse before anything is sent", async ({ page }) => {
  await setup(page);
  const quote = `const [s, b] = (await k.config()).assets; return k.quotes(s, b, ${SELL}n);`;

  await set(page, { paused: true });
  expect(await codeOf(page, quote)).toMatch(/^paused: Swaps are paused/);
  expect(await page.evaluate(async () => (await window.keeler.config()).paused)).toBe(true);
  expect(await privateBalance(page, 0)).toBe(String(DEPOSIT)); // balances still work
  await set(page, { paused: false });

  await set(page, { pricesDown: true });
  expect(await codeOf(page, quote)).toMatch(/^network: Reference prices are unavailable/);
  await set(page, { pricesDown: false });

  await set(page, { badSignature: true });
  expect(await codeOf(page, quote)).toMatch(/^no-quotes:/);
  await set(page, { badSignature: false });

  await set(page, { maxSell: String(SELL - 1n) });
  await page.reload(); // config is read once per page
  await page.waitForFunction(() => window.ready);
  await page.evaluate(async () => {
    const k = window.keeler;
    await k.connect(k.wallets().find((w) => w.name.includes("in-page"))!);
    await k.unlock();
  });
  expect(await page.evaluate(async () => String((await window.keeler.config()).assets[0]!.maxSell))).toBe(String(SELL - 1n));
  expect(await codeOf(page, quote)).toMatch(/^no-quotes: The most one order can sell/);
  await set(page, { maxSell: null });
  await page.reload();
  await page.waitForFunction(() => window.ready);
  await page.evaluate(async () => {
    const k = window.keeler;
    await k.connect(k.wallets().find((w) => w.name.includes("in-page"))!);
    await k.unlock();
  });

  // The order record cannot be written: the swap stops before signing, nothing is locked.
  await set(page, { idle: true });
  const noStore = await codeOf(
    page,
    `const put = Storage.prototype.setItem;
     Storage.prototype.setItem = function (key, value) { if (String(key).startsWith("keeler:order:")) throw new Error("quota"); return put.call(this, key, value); };
     try { const [s, b] = (await k.config()).assets; const { quotes } = await k.quotes(s, b, ${SELL}n); await k.swap(quotes[0], () => {}); }
     finally { Storage.prototype.setItem = put; }`,
  );
  expect(noStore).toMatch(/^storage: Could not save the order/);
  expect(await privateBalance(page, 0)).toBe(String(DEPOSIT));
  expect(await page.evaluate(async () => (await window.keeler.orders()).length)).toBe(0);

  // A second swap while one is in flight is refused.
  const busy = await page.evaluate(async (sell) => {
    const k = window.keeler;
    const [s, b] = (await k.config()).assets;
    const { quotes } = await k.quotes(s!, b!, BigInt(sell));
    const first = k.swap(quotes[0]!, () => {});
    const second = await k.swap(quotes[0]!, () => {}).then(
      () => "ok",
      (e) => (e instanceof window.KeelerError ? e.code : String(e)),
    );
    const done = await first.then((o) => o.status, (e) => `first failed: ${String(e)}`);
    return { second, done };
  }, String(SELL));
  expect(busy).toEqual({ second: "busy", done: "locked" });
});
