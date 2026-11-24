// The lend client's real proving path, in a browser, on a local network with no keeper
// (scripts/lend-web-stack.sh; its helper stands in for the keeper's tickets and account view only).
// The lend WASM package, its proving keys, the note decoding and the transaction building all run
// for real: a supply intent is proved, signed and sent, lands, and is refunded after it expires;
// a debt-free position the helper opened is adopted from the account view and closed.
// Skipped unless that stack is up.
import { existsSync, readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";
import type { Keeler, KeelerError } from "../../src/keeler/index";
import { LEND_STACK } from "../playwright.lend.config";

declare global {
  interface Window {
    keeler: Keeler;
    KeelerError: typeof KeelerError;
    ready: boolean;
  }
}

type Stack = { keeperUrl: string; tusd: string; walletSeed: string; wallet: string };
const stack: Stack | undefined = existsSync(LEND_STACK) ? JSON.parse(readFileSync(LEND_STACK, "utf8")) : undefined;
const SOL = "11111111111111111111111111111111";

async function up() {
  if (!stack) return false;
  return fetch(`${stack.keeperUrl}/health`).then((r) => r.ok, () => false);
}

async function helper(path: string) {
  const res = await fetch(`${stack!.keeperUrl}${path}`, { method: "POST" });
  const body = await res.json();
  if (!res.ok) throw new Error(`${path}: ${body.error}`);
  return body;
}

test.describe.configure({ mode: "serial" });
let page: Page;

/** Private balances as decimal strings, by mint. */
const privates = () =>
  page.evaluate(async () => Object.fromEntries((await window.keeler.balances()).map((b) => [b.asset.mint, String(b.private)])));

/** The lend account, bigints as strings. */
const account = () =>
  page.evaluate(async () => JSON.parse(JSON.stringify(await window.keeler.lend!.lendAccount(), (_, v) => (typeof v === "bigint" ? v.toString() : v))));

/** Runs `body` (with `k` the client) in the page; a KeelerError comes back as its code and message. */
const run = (body: string, arg?: unknown) =>
  page.evaluate(
    async ([src, a]) => {
      try {
        const out = await (0, eval)(`(async (k, arg) => { ${src} })`)(window.keeler, a);
        return JSON.parse(JSON.stringify(out ?? null, (_, v) => (typeof v === "bigint" ? v.toString() : v)));
      } catch (e) {
        throw new Error(e instanceof window.KeelerError ? `${e.code}: ${e.message}` : String(e));
      }
    },
    [body, arg] as const,
  );

test.beforeAll(async ({ browser }) => {
  test.skip(!(await up()), "the lend web stack is down: scripts/lend-web-stack.sh start");
  page = await browser.newPage();
  page.on("pageerror", (e) => console.log("[pageerror]", e.message));
  page.on("console", (m) => m.type() === "error" && console.log("[console]", m.text()));
  // The helper knows this wallet's key: it funds it and makes its deposit intent.
  await page.addInitScript((seed) => localStorage.setItem("keeler:in-page-wallet:v1", seed), stack!.walletSeed);
  await page.goto("/tests/harness/index.html");
  await page.waitForFunction(() => window.ready);
  const address = await run(`const r = await k.connect(k.wallets().find((w) => w.name === "Browser test wallet")); await k.unlock(); return r.address;`);
  expect(address).toBe(stack!.wallet);
  expect(await run(`return k.lend !== undefined;`)).toBe(true);
  const before = await privates();
  await helper("/test/fund");
  await run(`await k.register();`);
  // The helper shields to the keys the Rust SDK derives from the same wallet: the page must see them.
  const p = await privates();
  expect(BigInt(p[SOL]!) - BigInt(before[SOL]!)).toBe(2_000_000_000n);
  expect(BigInt(p[stack!.tusd]!) - BigInt(before[stack!.tusd]!)).toBe(50_000_000n);
});

test("a supply intent is proved, sent and lands, then is refunded after it expires", async () => {
  const usd = BigInt((await privates())[stack!.tusd]!);
  const op = await run(`return k.lend.supply(arg, 5000000n, () => {});`, stack!.tusd);
  expect(op).toMatchObject({ kind: "supply", status: "pending", step: "keeper" });
  expect(BigInt((await privates())[stack!.tusd]!)).toBe(usd - 5_000_000n);
  const seen = (await account()).operations.find((o: { id: string }) => o.id === op.id);
  expect(seen).toMatchObject({ status: "pending" });

  // The helper's ticket gives the intent 65 s; the keeper never fills it.
  await expect.poll(async () => (await account()).operations.find((o: { id: string }) => o.id === op.id)?.status, { timeout: 180_000, intervals: [5_000] }).toBe("refundable");
  await run(`await k.lend.refundIntent(arg);`, op.id);
  expect((await account()).operations.find((o: { id: string }) => o.id === op.id)).toMatchObject({ status: "refunded" });
  expect(BigInt((await privates())[stack!.tusd]!)).toBe(usd);
});

test("a debt-free position from the account view is closed by the page alone", async () => {
  const { position } = await helper("/test/open");
  const before = BigInt((await privates())[SOL]!);
  const held = (await account()).positions.find((p: { id: string }) => p.id === position.position);
  expect(held).toMatchObject({ vault: 1, collateral: position.collateral, debt: "0" });
  const op = await run(`return k.lend.closePosition(arg, () => {});`, position.position);
  expect(op).toMatchObject({ kind: "close", status: "landed" });
  expect((await account()).positions.find((p: { id: string }) => p.id === position.position)).toBeUndefined();
  // The deposit intent spent 0.5 SOL of the 2; the close pays the collateral back.
  expect(BigInt((await privates())[SOL]!)).toBe(before + BigInt(position.collateral));
});
