// Earn, Borrow, Swap, Balances and Activity on the mock client, driven only through the page's visible controls.
// LEND_SCREENS=<dir> also saves screenshots there, named lend-<project>-<n>-<what>.jpg.
import { expect, test, type Page, type TestInfo } from "@playwright/test";

const panel = (page: Page) => page.getByRole("region", { name: "Keeler app" });
const activity = (page: Page) => page.locator("section[aria-labelledby=lending-title]");
const caption = (page: Page) => page.locator(".observatory figcaption");
const navLink = (page: Page, name: string) => page.getByRole("navigation", { name: "Application" }).getByRole("link", { name: new RegExp(`^${name}`) });
async function open(page: Page, name: "Earn" | "Borrow" | "Swap" | "Balances" | "Activity") {
  await navLink(page, name).click();
  await expect(navLink(page, name)).toHaveAttribute("aria-current", "page");
}

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
  else await page.screenshot({ path, type: "jpeg", quality: 82, fullPage: true });
}

async function setUp(page: Page, search = "") {
  const problems: string[] = [];
  page.on("console", (m) => m.type() === "error" && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`/${search}#swap`);
  const p = panel(page);
  await p.getByRole("button", { name: /Browser test wallet/ }).click();
  await p.getByRole("button", { name: "Sign to unlock" }).click();
  await p.getByRole("button", { name: "Get test tokens" }).click();
  await expect(p.getByText("Test tokens sent to your public balance.")).toBeVisible();
  await p.getByRole("button", { name: "Register" }).click();
  await expect(p.getByRole("heading", { name: "Swap privately" })).toBeVisible();
  // 600 tUSD and 5 tSOL into the private balance.
  for (const [amount, symbol] of [["600", "tUSD"], ["5", "tSOL"]] as const) {
    await open(page, "Balances");
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
  await open(page, "Earn");
  await expect(p.getByRole("heading", { name: "Earn privately" })).toBeVisible();
  await expect(p.getByText("supply APR")).toBeVisible();
  await shot(page, info, "01-earn-empty");

  await p.locator("#earn-amt").fill("500");
  await p.getByRole("button", { name: "Deposit and earn" }).click();
  await expect(p.locator(".step-now", { hasText: "Proving in your browser" })).toBeVisible();
  await expect(p.locator(".step-now", { hasText: "Waiting for the keeper" })).toBeVisible({ timeout: 10_000 });
  await expect(activity(page).locator("li.order-pending")).toHaveCount(1);
  await shot(page, info, "02-earn-keeper");
  await expect(p.getByText("Done. Your deposit is in the pool and earning.")).toBeVisible({ timeout: 15_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();

  // The deposit's value grows while the page is open.
  const value = p.locator(".mine-big");
  const first = units(await value.innerText(), 6);
  expect(first).toBeGreaterThanOrEqual(499_999_999n);
  await expect.poll(async () => units(await value.innerText(), 6), { timeout: 15_000 }).toBeGreaterThan(first);
  await shot(page, info, "03-earn-deposit");

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
  await open(page, "Borrow");
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
  await shot(page, info, "04a-borrow-invalid");
  await p.locator("#borrow-amt").fill("400");
  await shot(page, info, "04-borrow-preview");
  await p.getByRole("button", { name: "Borrow", exact: true }).last().click();
  await expect(p.getByText("Done. It's in your private balance.")).toBeVisible({ timeout: 20_000 });
  await p.getByRole("button", { name: "Done", exact: true }).click();
  const meter = p.getByRole("meter", { name: "Loan-to-value" });
  await expect(meter).toHaveAttribute("aria-valuetext", /^70\.\d%, healthy$/);
  await shot(page, info, "05-borrow-position");

  await borrowAction(page, "Repay", "50", "Repay");
  await borrowAction(page, "Withdraw", "0.5", "Withdraw collateral");
  await expect(p.locator(".position dd").first()).toHaveText("3.5 tSOL");

  // A price drop past 85%: the keeper sells part of the collateral and the position is back at the threshold.
  await p.getByRole("button", { name: "Crash to liquidation" }).click();
  await expect(p.locator(".position .badge")).toHaveText("Liquidatable", { timeout: 4000 });
  const note = p.locator(".note", { hasText: /^Past 85%\./ });
  await expect(note).toBeVisible();
  expect((await note.boundingBox())!.y).toBeLessThan((await p.locator("#borrow-amt").boundingBox())!.y);
  await shot(page, info, "06-liquidatable");
  await expect(p.getByText(/^Liquidated /)).toBeVisible({ timeout: 15_000 });
  await expect(p.getByText(/It brought the position back to 85% or less, and the rest stayed yours\./)).toBeVisible();
  await expect(meter).toHaveAttribute("aria-valuetext", /^8[45]\.\d%/);
  const collateral = units(await p.locator(".position dd").first().innerText(), 9);
  expect(collateral).toBeLessThan(3_500_000_000n);
  expect(collateral).toBeGreaterThan(2_500_000_000n);
  await shot(page, info, "07-liquidated");
  await open(page, "Activity");
  await expect(activity(page).getByText("Liquidation", { exact: true })).toBeVisible();
  await shot(page, info, "08-activity");
  await open(page, "Borrow");

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

test("an intent the keeper does not fill is taken back after it expires", async ({ page }, info) => {
  const problems = await setUp(page, "?mock=keeper-stalls");
  const p = panel(page);
  await open(page, "Earn");
  await p.locator("#earn-amt").fill("50");
  await p.getByRole("button", { name: "Deposit and earn" }).click();
  await expect(p.getByText(/you can take it back in \d:\d\d/)).toBeVisible({ timeout: 10_000 });
  await expect(p.getByRole("button", { name: "Take funds back" })).toBeVisible({ timeout: 45_000 });
  await shot(page, info, "09-earn-refundable");
  await open(page, "Activity");
  await expect(activity(page).getByRole("button", { name: "Take funds back" })).toBeVisible();
  await open(page, "Earn");
  await p.getByRole("button", { name: "Take funds back" }).click();
  await expect(p.getByText("Done. The funds are back in your private balance.")).toBeVisible({ timeout: 10_000 });
  await expect(activity(page).locator("li.order-refunded .badge")).toHaveText("Funds taken back");
  expect(problems).toEqual([]);
});

test("a keeper that is offline is reported before anything is sent", async ({ page }) => {
  await setUp(page, "?mock=keeper-offline");
  const p = panel(page);
  await open(page, "Earn");
  await p.locator("#earn-amt").fill("50");
  await p.getByRole("button", { name: "Deposit and earn" }).click();
  await expect(p.getByRole("alert")).toHaveText(/The keeper is not answering right now/);
  await expect(p.locator("#earn-amt")).toBeVisible();
});

test("a running operation survives another desk, Activity and the landing", async ({ page }) => {
  const problems = await setUp(page, "?mock=keeper-stalls");
  const p = panel(page);
  await open(page, "Earn");
  await p.locator("#earn-amt").fill("50");
  await p.getByRole("button", { name: "Deposit and earn" }).click();
  await expect(p.locator(".step-now", { hasText: "Waiting for the keeper" })).toBeVisible({ timeout: 10_000 });

  await open(page, "Borrow");
  await expect(page).toHaveURL(/#borrow$/);
  await expect(p.getByRole("heading", { name: "Borrow privately" })).toBeVisible();
  await open(page, "Activity");
  await expect(activity(page).locator("li.order-pending")).toBeVisible();
  await page.getByRole("link", { name: "Keeler, top of page" }).click();
  await expect(page.getByRole("heading", { name: "Lend in private." })).toBeVisible();
  await expect(p).toBeHidden();

  await page.getByRole("button", { name: "Supply tUSD" }).first().click();
  await expect(page).toHaveURL(/#earn$/);
  await expect(p.getByRole("heading", { name: "Earn privately" })).toBeVisible();
  await expect(p.locator(".step-now", { hasText: "Waiting for the keeper" })).toBeVisible();
  await expect(p.getByRole("button", { name: "Take funds back" })).toBeVisible({ timeout: 45_000 });
  expect(problems).toEqual([]);
});

test("the ring's caption projects a valid amount only, while the form previews both", async ({ page }, info) => {
  const problems = await setUp(page);
  const p = panel(page);

  await open(page, "Earn");
  await p.locator("#earn-amt").fill("100");
  await expect(caption(page)).toHaveText(/^Your deposit after this\s*100 tUSD$/);
  await p.locator("#earn-amt").fill("5000");
  await expect(caption(page)).toHaveText(/^Your deposit\s*0 tUSD$/);
  await p.getByRole("button", { name: "Deposit and earn" }).click();
  await expect(p.getByText(/One deposit spends one private note/)).toBeVisible();

  await open(page, "Borrow");
  await p.locator("#borrow-amt").fill("2");
  await expect(caption(page)).toHaveText(/^Loan-to-value after\s*No debt$/);
  await p.locator("#borrow-amt").fill("50");
  await expect(caption(page)).toHaveText(/^Loan-to-value\s*No position$/);
  await expect(p.getByText("Loan-to-value after")).toBeVisible();
  await shot(page, info, "10-borrow-over-limit");

  await open(page, "Swap");
  await p.locator("#sell-amt").fill("100");
  await expect(caption(page)).toHaveText(/^You sell\s*100 tUSD$/);
  await p.locator("#sell-amt").fill("300");
  await expect(caption(page)).toHaveText(/^You sell\s*—$/);
  await shot(page, info, "11-swap-over-limit");
  await p.locator("#sell-amt").fill("100");
  await p.getByRole("button", { name: "Get quotes" }).click();
  await expect(caption(page)).toHaveText(/^Quote from DeaL…RtYk\s*[\d.,]+ tSOL$/);
  await shot(page, info, "12-swap-quotes");
  await p.locator("input[name=quote]").nth(1).check();
  await expect(caption(page)).toHaveText(/^Quote from DeaL…NmPr/);
  expect(problems).toEqual([]);
});

test("a refundable order shows its count on Activity and a notice on every desk", async ({ page }, info) => {
  test.setTimeout(240_000);
  const problems = await setUp(page);
  const p = panel(page);
  await open(page, "Swap");
  await p.locator("#sell-amt").fill("100");
  await p.getByRole("button", { name: "Get quotes" }).click();
  await p.locator("input[name=quote]").nth(1).check();
  await p.getByRole("button", { name: "Swap privately" }).click();
  await expect(p.getByText("Locked. Waiting for the dealer to settle.")).toBeVisible({ timeout: 20_000 });
  await shot(page, info, "13-swap-progress");
  await expect(p.getByRole("button", { name: "Take funds back" })).toBeVisible({ timeout: 90_000 });

  await expect(navLink(page, "Activity")).toHaveAccessibleName("Activity, 1 to take back");
  for (const desk of ["Earn", "Borrow", "Swap", "Balances"] as const) {
    await open(page, desk);
    await expect(p.locator(".desk:not([hidden]) .note-refund")).toHaveText(/One order or operation has funds you can take back\./);
  }
  await shot(page, info, "14-balances");
  await p.locator(".desk:not([hidden]) .note-refund").getByRole("link", { name: "Go to Activity" }).click();
  await expect(page).toHaveURL(/#activity$/);
  await expect(page.locator("section[aria-labelledby=orders-title] li.order-refundable")).toBeVisible();
  await shot(page, info, "15-activity");
  await page.locator("section[aria-labelledby=orders-title]").getByRole("button", { name: "Take funds back" }).click();
  await expect(navLink(page, "Activity")).toHaveAccessibleName("Activity", { timeout: 15_000 });
  await expect(p.locator(".note-refund")).toHaveCount(0);
  expect(problems).toEqual([]);
});

test("the nav links and the hash stay in step", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/#swap");
  for (const [name, hash, title] of [
    ["Earn", "earn", "Earn privately"],
    ["Borrow", "borrow", "Borrow privately"],
    ["Balances", "balances", "Balances"],
    ["Activity", "activity", "Activity"],
    ["Swap", "swap", "Swap privately"],
  ] as const) {
    await open(page, name);
    await expect(page).toHaveURL(new RegExp(`#${hash}$`));
    await expect(navLink(page, name)).toHaveAttribute("aria-current", "page");
    await expect(page.getByRole("navigation", { name: "Application" }).locator("[aria-current]")).toHaveCount(1);
    await expect(panel(page).getByRole("heading", { level: 1 })).toHaveText(title);
  }
  await page.goBack();
  await expect(page).toHaveURL(/#activity$/);
  await expect(navLink(page, "Activity")).toHaveAttribute("aria-current", "page");
  await page.evaluate(() => (location.hash = "borrow"));
  await expect(navLink(page, "Borrow")).toHaveAttribute("aria-current", "page");
});

test("the desk ring comes up in WebGL beside each desk", async ({ page }) => {
  const problems: string[] = [];
  page.on("console", (m) => m.type() === "error" && problems.push(m.text()));
  await page.addInitScript(() => {
    const seen: string[] = [];
    (window as unknown as { renderers: string[] }).renderers = seen;
    new MutationObserver((list) => {
      for (const m of list) seen.push((m.target as Element).getAttribute("data-renderer") ?? "");
    }).observe(document, { subtree: true, attributes: true, attributeFilter: ["data-renderer"] });
  });
  await page.goto("/#earn");
  const figure = page.locator(".observatory");
  await expect(figure).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as unknown as { renderers: string[] }).renderers[0])).toBe("webgl");
  for (const desk of ["Borrow", "Swap"] as const) {
    await open(page, desk);
    await expect(figure).toBeVisible();
    expect((await figure.locator("canvas").boundingBox())!.width).toBeGreaterThan(100);
  }
  await open(page, "Balances");
  await expect(figure).toBeHidden();
  expect(problems).toEqual([]);
});

test("without WebGL the desk shows the still frames", async ({ page }) => {
  await page.addInitScript(() => {
    const get = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, ...rest: unknown[]) {
      if (/webgl/.test(kind)) return null;
      return (get as (...a: unknown[]) => RenderingContext | null).call(this, kind, ...rest);
    } as typeof get;
  });
  await page.goto("/#borrow");
  const figure = page.locator(".observatory");
  await expect(figure).toHaveAttribute("data-renderer", "still");
  const still = figure.locator(".aperture > div");
  await expect(still).toHaveCount(1);
  expect(await still.evaluate((el) => getComputedStyle(el).backgroundImage)).toMatch(/url\(/);
});
