// Earn and Borrow on the mock client, driven only through the page's visible controls.
// LEND_SCREENS=<dir> also saves screenshots there, named lend-<project>-<n>-<what>.jpg.
import { expect, test, type Page, type TestInfo } from "@playwright/test";

const panel = (page: Page) => page.getByRole("region", { name: "Private swap" });
const activity = (page: Page) => page.locator("section[aria-labelledby=lending-title]");

function units(text: string, decimals: number): bigint {
  const [whole, frac = ""] = text.replace(/[^\d.]/g, "").split(".");
  return BigInt(whole! + frac.padEnd(decimals, "0"));
}

async function shot(page: Page, info: TestInfo, name: string, target?: "panel" | string) {
  const dir = process.env.LEND_SCREENS;
  if (!dir) return;
  const path = `${dir}/lend-${info.project.name}-${name}.jpg`;
  await page.waitForTimeout(400);
  if (target === "panel") await panel(page).screenshot({ path, type: "jpeg", quality: 82 });
  else if (target) await page.locator(target).screenshot({ path, type: "jpeg", quality: 82 });
  else await page.screenshot({ path, type: "jpeg", quality: 82 });
}

async function setUp(page: Page, search = "") {
  const problems: string[] = [];
  page.on("console", (m) => m.type() === "error" && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`/${search}`);
  const p = panel(page);
  await p.getByRole("button", { name: /Browser test wallet/ }).click();
  await p.getByRole("button", { name: "Sign to unlock" }).click();
  await p.getByRole("button", { name: "Get test tokens" }).click();
  await expect(p.getByText("Test tokens sent to your public balance.")).toBeVisible();
  await p.getByRole("button", { name: "Register" }).click();
  await expect(p.getByRole("heading", { name: "Swap privately" })).toBeVisible();
  // 600 tUSD and 5 tSOL into the private balance.
  for (const [amount, symbol] of [["600", "tUSD"], ["5", "tSOL"]] as const) {
    await p.getByRole("button", { name: "Balances", exact: true }).click();
    await p.getByRole("group", { name: "Direction" }).getByRole("button", { name: "Deposit" }).click();
    await p.getByRole("combobox", { name: "Asset", exact: true }).selectOption({ label: symbol });
    await p.locator("#move-amt").fill(amount);
    await p.locator("form.move button[type=submit]").click();
    await expect(p.getByText(`Deposited ${amount} ${symbol}.`)).toBeVisible({ timeout: 10_000 });
  }
  return problems;
}

/** Fills the Borrow form for one action and waits until it lands. */
async function borrowAction(page: Page, action: "Add" | "Borrow" | "Repay" | "Withdraw", amount: string, button: string) {
  const p = panel(page);
  await p.getByRole("group", { name: "Position action" }).getByRole("button", { name: action, exact: true }).click();
  await p.locator("#borrow-amt").fill(amount);
  await p.getByRole("button", { name: button, exact: true }).last().click();
  await expect(p.getByText(/^Done\./)).toBeVisible({ timeout: 20_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();
}

test("earn: deposit, interest accrues, withdraw", async ({ page }, info) => {
  const problems = await setUp(page);
  const p = panel(page);
  await p.getByRole("button", { name: "Earn", exact: true }).click();
  await expect(p.getByRole("heading", { name: "Earn privately" })).toBeVisible();
  await expect(p.getByText("supply APR")).toBeVisible();
  await shot(page, info, "01-earn-empty", "panel");

  await p.locator("#earn-amt").fill("500");
  await p.getByRole("button", { name: "Deposit and earn" }).click();
  await expect(p.locator(".step-now", { hasText: "Proving in your browser" })).toBeVisible();
  await expect(p.locator(".step-now", { hasText: "Waiting for the keeper" })).toBeVisible({ timeout: 10_000 });
  await expect(activity(page).locator("li.order-pending")).toHaveCount(1);
  await shot(page, info, "02-earn-keeper", "panel");
  await expect(p.getByText("Done. Your deposit is in the pool and earning.")).toBeVisible({ timeout: 15_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();

  // The deposit's value grows while the page is open.
  const value = p.locator(".mine-big");
  const first = units(await value.innerText(), 6);
  expect(first).toBeGreaterThanOrEqual(499_999_999n);
  await expect.poll(async () => units(await value.innerText(), 6), { timeout: 15_000 }).toBeGreaterThan(first);
  await shot(page, info, "03-earn-deposit", "panel");

  await p.getByRole("group", { name: "Earn action" }).getByRole("button", { name: "Withdraw" }).click();
  await p.locator("#earn-amt").fill("100");
  await p.getByRole("button", { name: "Withdraw", exact: true }).last().click();
  await expect(p.getByText("Done. The withdrawal is in your private balance.")).toBeVisible({ timeout: 15_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();
  await expect.poll(async () => units(await value.innerText(), 6)).toBeLessThan(401_000_000n);
  await expect(activity(page).locator("li.order-settled")).toHaveCount(2);

  // Withdraw all: by shares, so nothing is left in the deposit.
  await p.getByRole("group", { name: "Earn action" }).getByRole("button", { name: "Withdraw" }).click();
  await p.getByRole("button", { name: "Withdraw all" }).click();
  await p.getByRole("button", { name: "Withdraw", exact: true }).last().click();
  await expect(p.getByText("Done. The withdrawal is in your private balance.")).toBeVisible({ timeout: 15_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();
  await expect(activity(page).locator("li.order-settled")).toHaveCount(3);
  await expect(p.locator(".mine-big")).toHaveCount(0);
  expect(problems).toEqual([]);
});

test("borrow: open, borrow, repay, withdraw, liquidation, repay all and close", async ({ page }, info) => {
  const problems = await setUp(page);
  const p = panel(page);
  await p.getByRole("button", { name: "Borrow", exact: true }).click();
  await expect(p.getByRole("heading", { name: "Borrow privately" })).toBeVisible();
  await expect(p.getByText("No position yet.")).toBeVisible();

  // Below the minimum is refused in the page.
  await p.locator("#borrow-amt").fill("0.01");
  await p.getByRole("button", { name: "Open a position" }).click();
  await expect(p.getByText("A new position needs at least 0.05 tSOL.")).toBeVisible();

  await p.locator("#borrow-amt").fill("4");
  await p.getByRole("button", { name: "Open a position" }).click();
  await expect(p.getByText("Done. The collateral is in your position.")).toBeVisible({ timeout: 20_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();
  await expect(p.locator(".position .badge")).toHaveText("Healthy");

  // More than 80% is refused with the limit, and the preview moves as the amount is typed.
  await p.getByRole("group", { name: "Position action" }).getByRole("button", { name: "Borrow", exact: true }).click();
  await p.locator("#borrow-amt").fill("500");
  await expect(p.getByText("Loan-to-value after")).toBeVisible();
  await p.getByRole("button", { name: "Borrow", exact: true }).last().click();
  // 4 tSOL at 142.37, less the keeper's 0.5% draw margin, at 80%: 453.30.
  await expect(p.getByText(/That would take you past 80%\. You can borrow up to 453\.30/)).toBeVisible();
  await p.locator("#borrow-amt").fill("400");
  await shot(page, info, "04-borrow-preview", "panel");
  await p.getByRole("button", { name: "Borrow", exact: true }).last().click();
  await expect(p.getByText("Done. It's in your private balance.")).toBeVisible({ timeout: 20_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();
  const meter = p.getByRole("meter", { name: "Loan-to-value" });
  await expect(meter).toHaveAttribute("aria-valuetext", /^70\.\d%, healthy$/);
  await shot(page, info, "05-borrow-position", "panel");

  await borrowAction(page, "Repay", "50", "Repay");
  await borrowAction(page, "Withdraw", "0.5", "Withdraw collateral");
  await expect(p.locator(".position dd").first()).toHaveText("3.5 tSOL");

  // A price drop past 85%: the keeper sells part of the collateral and the position is back at the threshold.
  await p.getByRole("button", { name: "Crash to liquidation" }).click();
  await expect(p.locator(".position .badge")).toHaveText("Liquidatable", { timeout: 4000 });
  await shot(page, info, "06-liquidatable", "panel");
  await expect(p.getByText(/^Liquidated /)).toBeVisible({ timeout: 15_000 });
  await expect(p.getByText(/The position is back to 85% or less, and the rest is still yours\./)).toBeVisible();
  await expect(meter).toHaveAttribute("aria-valuetext", /^8[45]\.\d%/);
  const collateral = units(await p.locator(".position dd").first().innerText(), 9);
  expect(collateral).toBeLessThan(3_500_000_000n);
  expect(collateral).toBeGreaterThan(2_500_000_000n);
  await expect(activity(page).getByText("Liquidation", { exact: true })).toBeVisible();
  await shot(page, info, "07-liquidated", "panel");
  await shot(page, info, "08-activity", "section[aria-labelledby=lending-title]");

  // Repay everything, then close without the keeper.
  await p.getByRole("button", { name: "Reset" }).click();
  await p.getByRole("group", { name: "Position action" }).getByRole("button", { name: "Repay", exact: true }).click();
  await p.getByRole("button", { name: "Repay all" }).click();
  await p.getByRole("button", { name: "Repay", exact: true }).last().click();
  await expect(p.getByText("Done. The repayment is applied.")).toBeVisible({ timeout: 20_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();
  await p.getByRole("button", { name: /^Close position and take back/ }).click();
  await expect(p.getByText("Done. The position is closed and the collateral is in your private balance.")).toBeVisible({ timeout: 20_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();
  await expect(p.getByText("No position yet.")).toBeVisible();
  expect(problems).toEqual([]);
});

test("an intent the keeper does not fill is taken back after it expires", async ({ page }) => {
  const problems = await setUp(page, "?mock=keeper-stalls");
  const p = panel(page);
  await p.getByRole("button", { name: "Earn", exact: true }).click();
  await p.locator("#earn-amt").fill("50");
  await p.getByRole("button", { name: "Deposit and earn" }).click();
  await expect(p.getByText(/you can take it back in \d:\d\d/)).toBeVisible({ timeout: 10_000 });
  await expect(p.getByRole("button", { name: "Take funds back" })).toBeVisible({ timeout: 45_000 });
  await expect(activity(page).getByRole("button", { name: "Take funds back" })).toBeVisible();
  await p.getByRole("button", { name: "Take funds back" }).click();
  await expect(p.getByText("Done. The funds are back in your private balance.")).toBeVisible({ timeout: 10_000 });
  await expect(activity(page).locator("li.order-refunded .badge")).toHaveText("Funds taken back");
  expect(problems).toEqual([]);
});

test("a keeper that is offline is reported before anything is sent", async ({ page }) => {
  await setUp(page, "?mock=keeper-offline");
  const p = panel(page);
  await p.getByRole("button", { name: "Earn", exact: true }).click();
  await p.locator("#earn-amt").fill("50");
  await p.getByRole("button", { name: "Deposit and earn" }).click();
  await expect(p.getByRole("alert")).toHaveText(/The keeper is not answering right now/);
  await expect(p.locator("#earn-amt")).toBeVisible();
});

test("landing: lend sections and FAQ", async ({ page }, info) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /Jupiter-style lending\. The chain never sees your position's size\./ })).toBeVisible();
  await expect(page.getByText(/supply APR now/)).toBeVisible();
  const privacy = page.locator("#lend-privacy");
  await privacy.getByRole("button", { name: "On Keeler" }).click();
  await expect(privacy.getByText("Here they can't.", { exact: false })).toBeVisible();
  for (const q of ["Who runs the pool?", "What if the keeper is offline?", "What is visible when I lend or borrow?", "Is Keeler on mainnet?"]) {
    await page.locator("#faq summary", { hasText: q }).click();
  }
  await expect(page.getByText(/By design it cannot take your principal/)).toBeVisible();
  // The lend buttons move focus into the panel (its heading, as on every panel change).
  await page.getByRole("button", { name: "Start earning" }).click();
  await expect(panel(page).getByRole("heading", { name: "Choose a wallet" })).toBeFocused();
  for (const [id, name] of [["#lend", "09-landing-lend"], ["#lend-privacy", "10-landing-privacy"], ["#liquidation", "11-landing-liquidation"], [".section-facts", "12-landing-numbers"], ["#faq", "13-landing-faq"]] as const)
    await shot(page, info, name, id);
});
