// Earn and Borrow through the page's own controls, on a local network with the real keeper behind the real Worker
// (scripts/lend-web-stack.sh). The in-page test wallet signs; the browser proves intents, refunds and closes; the keeper
// fills, builds the owner-signed withdrawals and loans (checked by the page, co-signed), and liquidates after the test
// price is moved with `keeler lend-set-price`. Skipped unless that stack is up. Prints how long each step took.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";
import { LEND_STACK } from "../playwright.lend.config";

type Stack = {
  helperUrl: string;
  keeperUrl: string;
  keeperConfig: string;
  keeler: string;
  script: string;
  pauseFile: string;
  keeperState: string;
  tusd: string;
  walletSeed: string;
  wallet: string;
};
const stack: Stack | undefined = existsSync(LEND_STACK) ? JSON.parse(readFileSync(LEND_STACK, "utf8")) : undefined;
const SEED_KEY = "keeler:in-page-wallet:v1";
const KEEPER = 240_000;
const PROVE = 240_000;

const panel = (page: Page) => page.getByRole("region", { name: "Private swap" });
const activity = (page: Page) => page.locator("section[aria-labelledby=lending-title]");

function units(text: string, decimals: number): bigint {
  const [whole, frac = ""] = text.replace(/[^\d.]/g, "").split(".");
  return BigInt(whole! + frac.padEnd(decimals, "0"));
}

const times: [string, number][] = [];
async function timed<T>(name: string, f: () => Promise<T>): Promise<T> {
  const t = Date.now();
  const out = await f();
  const s = (Date.now() - t) / 1000;
  times.push([name, s]);
  console.log(`LEND-WEB ${name}: ${s.toFixed(1)} s`);
  return out;
}

async function up() {
  if (!stack?.helperUrl) return false;
  return fetch(`${stack.helperUrl}/health`).then((r) => r.ok, () => false);
}

/** The test price: tUSD per tSOL (x1e12 per base unit is dollars x 1e9), confidence 0.01. */
function setPrice(dollars: number) {
  execFileSync(stack!.keeler, ["lend-set-price", stack!.keeperConfig, "1", String(Math.round(dollars * 1e9)), "10000000"], { stdio: "pipe" });
}

const keeperTickets = (): { purpose: number; status: string }[] => {
  try {
    return JSON.parse(readFileSync(stack!.keeperState, "utf8")).tickets ?? [];
  } catch {
    return [];
  }
};

test.describe.configure({ mode: "serial" });
let page: Page;
const problems: string[] = [];

async function open() {
  await page.goto("/#swap");
  const p = panel(page);
  await p.getByRole("button", { name: /Browser test wallet/ }).click();
  await p.getByRole("button", { name: "Sign to unlock" }).click();
}

/** Private balance of `symbol` from the Balances tab, in base units. */
async function privateBalance(symbol: string, decimals: number) {
  const p = panel(page);
  await p.getByRole("group", { name: "Panel" }).getByRole("button", { name: "Balances", exact: true }).click();
  const row = p.locator("table.bal tr", { has: page.locator("th", { hasText: new RegExp(`^${symbol}$`) }) });
  const v = units(await row.locator("td").nth(1).innerText(), decimals);
  return v;
}

const tab = (name: "Earn" | "Borrow") => panel(page).getByRole("group", { name: "Panel" }).getByRole("button", { name, exact: true }).click();

/** Clicks `button` and waits for the operation to land, then Done. */
async function landed(button: string, done: RegExp | string, what: string, timeout = KEEPER + PROVE) {
  const p = panel(page);
  await timed(what, async () => {
    await p.getByRole("button", { name: button, exact: true }).last().click();
    // Fail at once when the page stops the operation, with its reason.
    await expect(p.getByText(done).or(p.getByText("It stopped."))).toBeVisible({ timeout });
    if (await p.getByText("It stopped.").isVisible()) throw new Error(`${what} stopped: ${await p.innerText()}`);
  });
  await p.getByRole("button", { name: "Done", exact: true }).click();
}

async function borrowAction(action: "Add" | "Borrow" | "Repay" | "Withdraw", amount: string | "max", button: string, done: string, what: string) {
  const p = panel(page);
  await p.getByRole("group", { name: "Position action" }).getByRole("button", { name: action, exact: true }).click();
  if (amount === "max") await p.getByRole("button", { name: "Repay all" }).click();
  else await p.locator("#borrow-amt").fill(amount);
  await landed(button, done, what);
}

const position = (i: number) => panel(page).locator(".position dd").nth(i);

test.beforeAll(async ({ browser }) => {
  test.skip(!(await up()), "the lend web stack is down: scripts/lend-web-stack.sh start");
  test.setTimeout(600_000);
  page = await browser.newPage();
  page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
  page.on("console", (m) => m.type() === "error" && console.log("[console]", m.text()));
  await page.emulateMedia({ reducedMotion: "reduce" });
  // The helper knows this wallet's key: it funds it before the page registers it.
  await page.addInitScript(([k, seed]) => localStorage.getItem(k) || localStorage.setItem(k, seed), [SEED_KEY, stack!.walletSeed] as const);
  const r = await fetch(`${stack!.helperUrl}/test/fund`, { method: "POST" });
  expect(r.ok, await r.text()).toBe(true);
  await open();
  const p = panel(page);
  await p.getByRole("button", { name: "Register" }).click();
  await expect(p.getByRole("heading", { name: "Swap privately" })).toBeVisible({ timeout: 60_000 });
  await expect.poll(() => privateBalance("tUSD", 6), { timeout: 60_000 }).toBe(50_000_000n);
});

test.afterEach(async ({}, info) => {
  // The page is shared, so Playwright's own failure capture does not see it: print what it showed.
  if (info.status !== info.expectedStatus && page) console.log("[page]", (await panel(page).innerText().catch(() => "")).slice(0, 2000));
});

test.afterAll(() => {
  console.log("LEND-WEB times:");
  for (const [n, s] of times) console.log(`  ${n}: ${s.toFixed(1)} s`);
});

test("earn: deposit, filled by the keeper, withdraw part, withdraw all", async () => {
  test.setTimeout(1_800_000);
  const before = await privateBalance("tUSD", 6);
  const p = panel(page);
  await tab("Earn");
  await expect(p.getByText("supply APR")).toBeVisible();
  await p.locator("#earn-amt").fill("5");
  await timed("earn deposit: click to proving done", async () => {
    await p.getByRole("button", { name: "Deposit and earn" }).click();
    await expect(p.locator(".step-now", { hasText: "Waiting for the keeper" })).toBeVisible({ timeout: PROVE });
  });
  await timed("earn deposit: sent to filled", () => expect(p.getByText("Done. Your deposit is in the pool and earning.")).toBeVisible({ timeout: KEEPER }));
  await p.getByRole("button", { name: "Done", exact: true }).click();
  const value = p.locator(".mine-big");
  await expect.poll(async () => units(await value.innerText(), 6), { timeout: 30_000 }).toBeGreaterThanOrEqual(4_999_999n);

  await p.getByRole("group", { name: "Earn action" }).getByRole("button", { name: "Withdraw" }).click();
  await p.locator("#earn-amt").fill("2");
  await landed("Withdraw", "Done. The withdrawal is in your private balance.", "earn withdraw part: click to landed");
  await expect.poll(async () => units(await value.innerText(), 6), { timeout: 30_000 }).toBeLessThan(3_100_000n);

  await p.getByRole("group", { name: "Earn action" }).getByRole("button", { name: "Withdraw" }).click();
  await p.getByRole("button", { name: "Withdraw all" }).click();
  await landed("Withdraw", "Done. The withdrawal is in your private balance.", "earn withdraw all: click to landed");
  await expect(p.locator(".mine-big")).toHaveCount(0);
  await expect(activity(page).locator("li.order-settled")).toHaveCount(3);
  // Everything back (interest at this utilisation is below a base unit).
  await expect.poll(() => privateBalance("tUSD", 6), { timeout: 60_000 }).toBeGreaterThanOrEqual(before);
});

test("borrow: open, borrow, add, repay part, repay all with change, withdraw, close", async () => {
  test.setTimeout(3_600_000);
  const sol = await privateBalance("SOL", 9);
  const p = panel(page);
  await tab("Borrow");
  await expect(p.getByText("No position yet.")).toBeVisible();
  await p.locator("#borrow-amt").fill("1");
  await landed("Open a position", "Done. The collateral is in your position.", "open position: click to filled");
  await expect(p.locator(".position .badge")).toHaveText("Healthy", { timeout: 30_000 });
  await expect(position(0)).toHaveText("1 SOL");

  await borrowAction("Borrow", "3", "Borrow", "Done. It's in your private balance.", "borrow: click to landed");
  // 3 tUSD on 1 SOL at 10: 30%; liquidation at 3 / 0.85 = 3.53.
  await expect(position(1)).toHaveText(/^3(\.0+\d*)? tUSD$/, { timeout: 30_000 });
  await expect(p.getByRole("meter", { name: "Loan-to-value" })).toHaveAttribute("aria-valuetext", /^30\.\d%, healthy$/);
  await expect(position(2)).toHaveText(/^3\.5\d* tUSD$/);

  await borrowAction("Add", "0.2", "Add collateral", "Done. The collateral is in your position.", "add collateral: click to filled");
  await expect(position(0)).toHaveText("1.2 SOL", { timeout: 30_000 });
  await borrowAction("Repay", "1", "Repay", "Done. The repayment is applied.", "repay part: click to filled");
  await expect.poll(async () => units(await position(1).innerText(), 6), { timeout: 30_000 }).toBeLessThan(2_001_000n);
  const usd = await privateBalance("tUSD", 6);
  await tab("Borrow");
  const debt = units(await position(1).innerText(), 6);
  await borrowAction("Repay", "max", "Repay", "Done. The repayment is applied.", "repay all: click to filled");
  await expect(position(1)).toHaveText("0 tUSD", { timeout: 30_000 });
  // The change came back: the wallet paid the debt, not the whole "Repay all" amount.
  await expect.poll(() => privateBalance("tUSD", 6), { timeout: 60_000 }).toBeGreaterThanOrEqual(usd - debt - 1_000n);
  await tab("Borrow");
  await borrowAction("Withdraw", "0.5", "Withdraw collateral", "Done. It's in your private balance.", "withdraw collateral: click to landed");
  await expect(position(0)).toHaveText("0.7 SOL", { timeout: 30_000 });
  await timed("close: click to landed", async () => {
    await p.getByRole("button", { name: /^Close position and take back 0\.7 SOL/ }).click();
    await expect(p.getByText("Done. The position is closed and the collateral is in your private balance.")).toBeVisible({ timeout: PROVE });
  });
  await p.getByRole("button", { name: "Done", exact: true }).click();
  await expect(p.getByText("No position yet.")).toBeVisible({ timeout: 30_000 });
  // The collateral is back, less nothing: fees are public SOL.
  await expect.poll(() => privateBalance("SOL", 9), { timeout: 60_000 }).toBe(sol);
});

test("liquidation: borrow near the limit, the price drops, the keeper liquidates", async () => {
  test.setTimeout(2_400_000);
  const p = panel(page);
  await tab("Borrow");
  await p.locator("#borrow-amt").fill("1");
  await landed("Open a position", "Done. The collateral is in your position.", "open position (liquidation case)");
  await expect(position(0)).toHaveText("1 SOL", { timeout: 30_000 });
  await borrowAction("Borrow", "7.5", "Borrow", "Done. It's in your private balance.", "borrow near the limit");
  await expect(p.getByRole("meter", { name: "Loan-to-value" })).toHaveAttribute("aria-valuetext", /^75\.\d%/, { timeout: 30_000 });
  // 7.5 / 0.85 = 8.82 and 7.5 / 0.90 = 8.33: at 8.6 (the keeper checks 8.65, price + confidence + its margin) the
  // position is past the 85% threshold and below the program's 90% max limit, so the liquidation is partial.
  setPrice(8.6);
  await timed("liquidation: price moved to the page showing it", () => expect(p.getByText(/^Liquidated /)).toBeVisible({ timeout: KEEPER }));
  await expect(p.getByText(/It brought the position back to 85% or less, and the rest stayed yours\./)).toBeVisible();
  const collateral = units(await position(0).innerText(), 9);
  expect(collateral).toBeLessThan(1_000_000_000n);
  expect(collateral).toBeGreaterThan(500_000_000n);
  await expect(activity(page).getByText("Liquidation", { exact: true })).toBeVisible();
  setPrice(10);
  // Repay the rest: the position is debt-free for the cases below.
  await borrowAction("Repay", "max", "Repay", "Done. The repayment is applied.", "repay all after the liquidation");
  await expect(position(1)).toHaveText("0 tUSD", { timeout: 30_000 });
});

test("recovery: with the browser's storage cleared, the receipt and the position come back", async () => {
  test.setTimeout(1_200_000);
  const p = panel(page);
  await tab("Earn");
  // The last Earn action was a withdrawal of everything: the form is back on Deposit.
  await expect(p.getByRole("group", { name: "Earn action" }).getByRole("button", { name: "Deposit" })).toHaveAttribute("aria-pressed", "true");
  await p.locator("#earn-amt").fill("3");
  await landed("Deposit and earn", "Done. Your deposit is in the pool and earning.", "earn deposit (recovery case)");
  await tab("Borrow");
  const collateral = await position(0).innerText();
  await page.evaluate((k) => {
    const seed = localStorage.getItem(k)!;
    localStorage.clear();
    sessionStorage.clear();
    localStorage.setItem(k, seed);
  }, SEED_KEY);
  await timed("recovery: reload to receipts and positions shown", async () => {
    await open();
    // The page never asks the wallet on its own: the reader allows the session to fetch the keeper's view.
    await page.getByRole("button", { name: "Check with the keeper" }).click({ timeout: 120_000 });
    await tab("Earn");
    await expect(p.locator(".mine-big")).toBeVisible({ timeout: 120_000 });
    await tab("Borrow");
    await expect(position(0)).toHaveText(collateral, { timeout: 120_000 });
  });
  await tab("Earn");
  expect(units(await p.locator(".mine-big").innerText(), 6)).toBeGreaterThanOrEqual(2_999_999n);
});

test("an intent the keeper does not fill becomes refundable, and the refund works", async () => {
  test.setTimeout(1_200_000);
  const p = panel(page);
  const usd = await privateBalance("tUSD", 6);
  await tab("Earn");
  await p.getByRole("group", { name: "Earn action" }).getByRole("button", { name: "Deposit" }).click();
  await p.locator("#earn-amt").fill("2");
  // The keeper is paused for new supply once it has signed this ticket: the intent lands, unfilled.
  const issued = keeperTickets().length;
  await p.getByRole("button", { name: "Deposit and earn" }).click();
  await expect.poll(() => keeperTickets().length, { timeout: 60_000, intervals: [100] }).toBeGreaterThan(issued);
  writeFileSync(stack!.pauseFile, "");
  try {
    await expect(p.locator(".step-now", { hasText: "Waiting for the keeper" })).toBeVisible({ timeout: PROVE });
    await timed("refund: sent to refundable (ticket 120 s)", () => expect(p.getByRole("button", { name: "Take funds back" })).toBeVisible({ timeout: 300_000 }));
    await timed("refund: click to refunded", async () => {
      await p.getByRole("button", { name: "Take funds back" }).click();
      await expect(p.getByText("Done. The funds are back in your private balance.")).toBeVisible({ timeout: PROVE });
    });
    await p.getByRole("button", { name: "Done", exact: true }).click();
    await expect(activity(page).locator("li.order-refunded .badge").first()).toHaveText("Funds taken back");
    await expect.poll(() => privateBalance("tUSD", 6), { timeout: 60_000 }).toBe(usd);
    // While paused, a new deposit is refused before anything is sent.
    await tab("Earn");
    await p.locator("#earn-amt").fill("2");
    await p.getByRole("button", { name: "Deposit and earn" }).click();
    await expect(p.getByRole("alert")).toHaveText(/Lending is paused|paused/, { timeout: 60_000 });
    await p.getByRole("button", { name: "Done", exact: true }).click();
  } finally {
    rmSync(stack!.pauseFile, { force: true });
  }
});

test("keeper offline: the page says so, and a debt-free position still closes", async () => {
  test.setTimeout(1_200_000);
  const p = panel(page);
  execFileSync(stack!.script, ["keeper-stop"], { stdio: "pipe" });
  try {
    await tab("Earn");
    await p.locator("#earn-amt").fill("2");
    await p.getByRole("button", { name: "Deposit and earn" }).click();
    await expect(p.getByRole("alert")).toHaveText(/The keeper is not answering right now/, { timeout: 60_000 });
    await tab("Borrow");
    const c = await position(0).innerText();
    await timed("close with the keeper offline", async () => {
      await p.getByRole("button", { name: new RegExp(`^Close position and take back ${c.replace(".", "\\.")}`) }).click();
      await expect(p.getByText("Done. The position is closed and the collateral is in your private balance.")).toBeVisible({ timeout: PROVE });
    });
    await p.getByRole("button", { name: "Done", exact: true }).click();
    await expect(p.getByText("No position yet.")).toBeVisible({ timeout: 30_000 });
  } finally {
    // No pipes: the keeper it starts in the background would hold them open, and this call would never return.
    execFileSync(stack!.script, ["keeper-start"], { stdio: "ignore" });
  }
  expect(problems).toEqual([]);
});
