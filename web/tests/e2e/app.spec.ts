// The whole product on a local network: piece C's built page, the real swap
// client, the real Worker (wrangler dev) and the real HTTP dealer, driven only
// through the page's visible controls.
import { readFileSync } from "node:fs";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { STACK } from "../playwright.config";

const panel = (page: Page) => page.getByRole("region", { name: "Keeler app" });
async function view(page: Page, name: string) {
  const link = page.getByRole("navigation", { name: "Application" }).getByRole("link", { name: new RegExp(`^${name}`) });
  await link.click();
  await expect(link).toHaveAttribute("aria-current", "page");
}
const ordersSection = (page: Page) => page.locator("section[aria-labelledby=orders-title]");

/** Base units from the page's formatted amount ("1,234.5"). */
function units(text: string, decimals: number): bigint {
  const [whole, frac = ""] = text.replace(/,/g, "").trim().split(".");
  return BigInt(whole! + frac.padEnd(decimals, "0"));
}

const DECIMALS: Record<string, number> = { tUSD: 6, tSOL: 9 };

async function balances(page: Page) {
  await view(page, "Balances");
  const table = panel(page).getByRole("table");
  await expect(table.getByText("Reading balances…")).toHaveCount(0);
  const out: Record<string, { public: bigint; private: bigint }> = {};
  for (const row of await table.locator("tbody tr").all()) {
    const sym = (await row.locator("th").innerText()).trim();
    const [pub, priv] = await row.locator("td").allInnerTexts();
    out[sym] = { public: units(pub!, DECIMALS[sym]!), private: units(priv!, DECIMALS[sym]!) };
  }
  return out;
}

async function move(page: Page, mode: "Deposit" | "Withdraw", amount: string, symbol: string) {
  const p = panel(page);
  await view(page, "Balances");
  await p.getByRole("group", { name: "Direction" }).getByRole("button", { name: mode }).click();
  await p.getByRole("combobox", { name: "Asset", exact: true }).selectOption({ label: symbol });
  await p.locator("#move-amt").fill(amount);
  await p.locator("form.move button[type=submit]").click();
  await expect(p.getByText(`${mode === "Deposit" ? "Deposited" : "Withdrew"} ${amount} ${symbol}.`)).toBeVisible({ timeout: 120_000 });
}

/** Sells `amount` tUSD for tSOL with the selected quote; resolves once the order is locked or settled. */
async function startSwap(page: Page, amount: string, onProving?: () => void) {
  const p = panel(page);
  await view(page, "Swap");
  await p.locator("#sell-amt").fill(amount);
  await p.getByRole("button", { name: "Get quotes" }).click();
  await expect(p.getByText("Signature verified")).toBeVisible({ timeout: 30_000 });
  const quoted = await p.locator(".quote .quote-amt").first().innerText();
  await p.getByRole("button", { name: "Swap privately" }).click();
  if (onProving) {
    await expect(p.locator(".step-now", { hasText: "Proving your order" })).toBeVisible({ timeout: 30_000 });
    onProving();
  }
  await expect(p.getByText(/^(Locked\. Waiting for the dealer|Settled\.)/)).toBeVisible({ timeout: 120_000 });
  return units(quoted.replace(/\s*tSOL$/, ""), 9);
}

async function connectAndUnlock(page: Page) {
  const p = panel(page);
  const wallet = p.getByRole("button", { name: /Browser test wallet/ });
  await expect(wallet.locator("img")).toHaveJSProperty("complete", true);
  expect(await wallet.locator("img").evaluate((img: HTMLImageElement) => img.naturalWidth)).toBeGreaterThan(0);
  await wallet.click();
  await p.getByRole("button", { name: "Sign to unlock" }).click();
}

function dealerPid(): number {
  return Number(readFileSync(`${STACK}/dealer.pid`, "utf8").trim());
}

test("swap, settle, withdraw, an unsettled swap taken back, and orders found again", async ({ page }) => {
  // Everything the browser refuses or fails is collected and must stay empty.
  const problems: string[] = [];
  page.on("console", (m) => m.type() === "error" && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`failed: ${r.url()} ${r.failure()?.errorText}`));
  page.on("response", (r) => r.status() >= 400 && problems.push(`HTTP ${r.status()}: ${r.url()}`));
  const requests: string[] = [];
  page.on("request", (r) => requests.push(new URL(r.url()).pathname));
  await page.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (e) => console.error(`CSP blocked ${e.violatedDirective}: ${e.blockedURI}`));
  });

  const main = await page.goto("/#swap");
  expect(main?.headers()["content-security-policy"]).toContain("'wasm-unsafe-eval'");

  // Wallet, unlock, test tokens (a new wallet has no SOL for the fee), register.
  await connectAndUnlock(page);
  await panel(page).getByRole("button", { name: "Get test tokens" }).click();
  await expect(panel(page).getByText("Test tokens sent to your public balance.")).toBeVisible({ timeout: 60_000 });
  await panel(page).getByRole("button", { name: "Register" }).click();
  await expect(panel(page).getByRole("heading", { name: "Swap privately" })).toBeVisible({ timeout: 120_000 });

  // 500 tUSD into the private balance.
  const funded = await balances(page);
  expect(funded.tUSD!.public).toBe(1_000_000_000n);
  await move(page, "Deposit", "500", "tUSD");
  const deposited = await balances(page);
  expect(deposited.tUSD).toEqual({ public: 500_000_000n, private: 500_000_000n });

  // A swap the dealer settles: private tUSD down by 100, private tSOL up by the quoted amount.
  const bought = await startSwap(page, "100");
  await expect(panel(page).getByText(/^Settled\. You received/)).toBeVisible({ timeout: 120_000 });
  await panel(page).getByRole("button", { name: "New swap" }).click();
  await expect.poll(async () => (await balances(page)).tSOL!.private, { timeout: 60_000 }).toBe(deposited.tSOL!.private + bought);
  expect((await balances(page)).tUSD!.private).toBe(400_000_000n);

  // Withdraw 50 tUSD back to the public balance.
  await move(page, "Withdraw", "50", "tUSD");
  expect((await balances(page)).tUSD).toEqual({ public: 550_000_000n, private: 350_000_000n });

  // A swap the dealer cannot settle: it answers at click, then is stopped while the browser proves.
  const pid = dealerPid();
  try {
    process.kill(pid, "SIGSTOP");
    await page.route("**/api/quotes", async (r) => {
      process.kill(pid, "SIGCONT");
      const res = await r.fetch().finally(() => process.kill(pid, "SIGSTOP"));
      await r.fulfill({ response: res });
    });
    await startSwap(page, "100");
    await page.unroute("**/api/quotes");
    expect((await balances(page)).tUSD!.private).toBe(250_000_000n);

    // Lose this browser's order records (the in-page wallet key stays), reload, find the order on chain.
    const removed = await page.evaluate(() => {
      const keys = Object.keys(localStorage).filter((k) => k.startsWith("keeler:order:"));
      keys.forEach((k) => localStorage.removeItem(k));
      return keys.length;
    });
    expect(removed).toBeGreaterThan(0);
    await page.reload();
    await connectAndUnlock(page);
    await expect(panel(page).getByRole("heading", { name: "Swap privately" })).toBeVisible({ timeout: 60_000 });
    await view(page, "Activity");
    const orders = ordersSection(page);
    await orders.getByRole("button", { name: "Find my orders" }).click();
    await expect(orders.getByRole("button", { name: "Find my orders" })).toBeEnabled({ timeout: 120_000 });
    const items = orders.locator("li.order");
    await expect(items).toHaveCount(2);
    await expect(orders.locator("li.order-settled")).toHaveCount(1);
    const open: Locator = orders.locator("li.order-locked, li.order-refundable");
    await expect(open).toHaveCount(1);

    // After the quote expires (180 s on this stack) the funds come back.
    await expect(open.getByRole("button", { name: "Take funds back" })).toBeVisible({ timeout: 400_000 });
    await open.getByRole("button", { name: "Take funds back" }).click();
    await expect(orders.locator("li.order-refunded")).toHaveCount(1, { timeout: 120_000 });
    await expect(orders.locator("li.order-refunded .badge")).toHaveText("Funds taken back");
    await expect.poll(async () => (await balances(page)).tUSD!.private, { timeout: 60_000 }).toBe(350_000_000n);
  } finally {
    process.kill(pid, "SIGCONT");
  }

  // The Poseidon hasher is inlined in the bundle: the built site never fetches a separate .wasm for it.
  expect(requests.filter((p) => p.includes("hasher"))).toEqual([]);
  expect(requests.some((p) => /\/assets\/prover\.worker-.*\.js$/.test(p))).toBe(true);
  expect(problems).toEqual([]);
});
