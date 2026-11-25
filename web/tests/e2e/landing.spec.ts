// The landing on the mock client: the flight, its instruments, the renderer, the buttons into the app and the FAQ.
// LEND_SCREENS=<dir> also saves a screenshot of each chapter there, named landing-<project>-<n>-<chapter>.jpg.
import { expect, test, type Page, type TestInfo } from "@playwright/test";

const panel = (page: Page) => page.getByRole("region", { name: "Private swap" });
const active = (page: Page) => page.locator('.kl-chapter[data-active="true"]');
const CHAPTERS: [number, RegExp][] = [
  [0, /^Lend in private\.$/],
  [0.22, /^Put tUSD to work\.$/],
  [0.45, /^Borrow against your tSOL\.$/],
  [0.66, /^Know your liquidation price\.$/],
  [0.84, /^Private amounts\. Public activity\.$/],
  [1, /^Try private lending\.$/],
];

async function flyTo(page: Page, progress: number) {
  await page.locator(".kl-journey").waitFor();
  await page.evaluate((p) => {
    const journey = document.querySelector<HTMLElement>(".kl-journey")!;
    const stage = document.querySelector<HTMLElement>(".kl-stage")!;
    const range = journey.offsetHeight - stage.offsetHeight;
    window.scrollTo({ top: window.scrollY + journey.getBoundingClientRect().top + p * range, behavior: "instant" });
  }, progress);
}

async function expectChapter(page: Page, title: RegExp) {
  await expect(active(page)).toHaveCount(1);
  await expect(active(page).getByRole("heading")).toHaveText(title);
}

async function shot(page: Page, info: TestInfo, name: string) {
  const dir = process.env.LEND_SCREENS;
  if (!dir) return;
  await page.waitForTimeout(600);
  await page.screenshot({ path: `${dir}/landing-${info.project.name}-${name}.jpg`, type: "jpeg", quality: 82 });
}

test("landing: the flight scrolls both ways through every chapter, with no bottom rail", async ({ page }, info) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");
  await expect(panel(page)).toBeHidden();
  await expectChapter(page, CHAPTERS[0]![1]);
  for (const [i, [stop, title]] of CHAPTERS.entries()) {
    await flyTo(page, stop);
    await expectChapter(page, title);
    await expect(page.locator(".kl-stage")).toBeInViewport({ ratio: 1 });
    await shot(page, info, `${i + 1}-${stop}`);
  }
  for (const [stop, title] of [...CHAPTERS].reverse()) {
    await flyTo(page, stop);
    await expectChapter(page, title);
  }
  await flyTo(page, 0.3);
  await expectChapter(page, CHAPTERS[1]![1]);
  await flyTo(page, 0.36);
  await expectChapter(page, CHAPTERS[2]![1]);

  await expect(page.locator(".kl-landing nav")).toHaveCount(1);
  await expect(page.locator(".kl-landing nav").getByRole("button")).toHaveText([/Open app/]);
  for (const gone of ["Explore the ring", "Offline preview", "Offline demo"]) await expect(page.getByText(gone)).toHaveCount(0);
  await expect(page.locator(".kl-landing a[href^='#']")).toHaveCount(0);
  await expect(page.locator(".kl-mast").getByText("Devnet beta")).toBeVisible();
  await expect(page.locator(".kl-chapter[inert], .kl-chapter[aria-hidden]")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Know your liquidation price." })).toBeAttached();
});

test("landing: the instruments change their figures, and the rate is the pool's", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");

  await flyTo(page, 0.22);
  await expect(active(page).getByText(/tUSD supply APR now/)).toBeVisible();
  await expect(active(page).locator(".kl-rate strong")).toHaveText(/^\d+\.\d\d%$/);
  await expect(active(page).getByText("Illustrative deposit")).toBeVisible();

  await flyTo(page, 0.45);
  const capacity = page.locator('[data-kl="capacity"]');
  await expect(capacity).toHaveText("484.92 tUSD");
  await page.getByLabel("tSOL collateral").fill("8");
  await expect(capacity).toHaveText("969.85 tUSD");
  await expect(page.locator('[data-kl="collateral"]')).toHaveText("8 tSOL");
  await page.getByLabel("tSOL collateral").press("Home");
  await expect(capacity).toHaveText("60.61 tUSD");

  await flyTo(page, 0.66);
  const ltv = page.locator('[data-kl="ltv"]');
  const health = page.locator('[data-kl="health"]');
  await expect(ltv).toHaveText("69.3%");
  await expect(health).toHaveText("Within borrow limit");
  await page.getByLabel("tSOL price drop").fill("15");
  await expect(ltv).toHaveText("81.5%");
  await expect(health).toHaveText("Above borrow limit");
  await page.getByLabel("tSOL price drop").fill("30");
  await expect(health).toHaveText("Liquidatable");
  await expect(page.getByRole("meter", { name: "Loan-to-value" })).toHaveAttribute("aria-valuetext", /liquidatable/);

  await flyTo(page, 0.84);
  const readout = page.locator('[data-kl="privacy"]');
  await expect(readout).toHaveText("tSOL / tUSD vault");
  await page.getByRole("button", { name: "Your position" }).click();
  await expect(readout).toHaveText("4 tSOL collateral · 420 tUSD debt");
  await expect(page.getByText("Illustrative · Visible to you and the pool keeper")).toBeVisible();
  await expect(page.getByRole("button", { name: "Your position" })).toHaveAttribute("aria-pressed", "true");
  await page.getByRole("button", { name: "Public chain" }).click();
  await expect(readout).toHaveText("tSOL / tUSD vault");
});

test("landing: reduced motion shows each chapter still, and focusing a control brings its chapter", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");
  await expect(page.locator(".kl-copy").first()).toHaveCSS("transform", "none");
  await expect(page.locator(".kl-relief")).toHaveCSS("opacity", "0");
  await page.getByRole("button", { name: "Open the borrow desk" }).focus();
  await expectChapter(page, CHAPTERS[2]![1]);
  await page.getByLabel("tSOL price drop").focus();
  await expectChapter(page, CHAPTERS[3]![1]);
  await page.keyboard.press("ArrowRight");
  await expect(page.locator('[data-kl="ltv"]')).toHaveText("70.0%");
  await page.getByRole("button", { name: "Your position" }).focus();
  await expectChapter(page, CHAPTERS[4]![1]);
  await page.keyboard.press("Enter");
  await expect(page.locator('[data-kl="privacy"]')).toHaveText(/collateral/);
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Shift+Tab");
  await expectChapter(page, CHAPTERS[3]![1]);
});

test("landing: without WebGL the packaged still frames show", async ({ page }, info) => {
  await page.addInitScript(() => {
    const original = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, type: string, ...rest: unknown[]) {
      return /webgl/i.test(type) ? null : (original as (...a: unknown[]) => unknown).call(this, type, ...rest);
    } as typeof original;
  });
  await page.goto("/");
  await expect(page.locator(".kl-stage")).toHaveAttribute("data-renderer", "still");
  const tall = info.project.name === "phone";
  const frame = (n: number) => page.locator(`.kl-cosmos img[src*="${tall ? "tall" : "wide"}-${n}"]`);
  await expect(frame(0)).toHaveCSS("opacity", "1");
  await expect(page.locator(".kl-cosmos canvas")).toHaveCSS("opacity", "0");
  await flyTo(page, 0.45);
  await expectChapter(page, CHAPTERS[2]![1]);
  await expect(frame(2)).toHaveCSS("opacity", "1");
  await flyTo(page, 0);
  await expect(frame(0)).toHaveCSS("opacity", "1");
});

test("landing: with motion, the flight first comes up in WebGL", async ({ page }) => {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  await page.addInitScript(() => {
    const seen: string[] = [];
    (window as unknown as { renderers: string[] }).renderers = seen;
    new MutationObserver((records) => {
      for (const r of records) seen.push((r.target as Element).getAttribute("data-renderer") ?? "");
    }).observe(document, { subtree: true, attributes: true, attributeFilter: ["data-renderer"] });
  });
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => (window as unknown as { renderers: string[] }).renderers[0])).toBe("webgl");
  await flyTo(page, 0.76);
  await expectChapter(page, CHAPTERS[4]![1]);
  await expect.poll(() => page.locator(".kl-stage").evaluate((s) => Number(getComputedStyle(s).getPropertyValue("--kl-relief-opacity")))).toBeGreaterThan(0.5);
  expect(errors.filter((e) => e.includes("Orbit scene"))).toEqual([]);
});

test("landing: the buttons reach the app, and the scroll cue flies to the next chapter", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");
  await page.getByRole("button", { name: "Scroll to fly through" }).click();
  await expectChapter(page, CHAPTERS[1]![1]);
  await expect(page.getByRole("heading", { name: "Put tUSD to work." })).toBeFocused();
  await expect(page).toHaveURL(/\/$/);

  for (const [stop, name, route] of [
    [0, "Supply tUSD", "earn"],
    [0, "Borrow tUSD", "borrow"],
    [0.22, "Open the earn desk", "earn"],
    [0.45, "Open the borrow desk", "borrow"],
    [0.66, "Review a position", "borrow"],
    [1, "Explore private swaps", "swap"],
    [1, "Open app", "earn"],
  ] as const) {
    await page.goto("/");
    await flyTo(page, stop);
    await active(page).or(page.locator(".kl-mast")).getByRole("button", { name, exact: true }).first().click();
    await expect(page).toHaveURL(new RegExp(`#${route}$`));
    await expect(panel(page)).toBeVisible();
    await expect(page.locator(".kl-landing")).toHaveCount(0);
  }
  await page.goBack();
  await expect(page.locator(".kl-landing")).toHaveCount(1);
});

test("landing: the FAQ keeps its anchors and its answers", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/#faq");
  await expect(page.locator("#faq")).toBeInViewport();
  await expect(panel(page)).toBeHidden();
  for (const q of ["Who runs the pool?", "What if the keeper is offline?", "What is visible when I lend or borrow?", "Will the keeper always see lending amounts?", "Is Keeler on mainnet?"]) {
    await page.locator("#faq summary", { hasText: q }).click();
  }
  await expect(page.getByText(/By design it cannot take your principal/)).toBeVisible();
  await expect(page.getByText("Running the keeper in an attested enclave, so that nobody can read amounts, is planned.")).toBeVisible();
  await expect(page.locator("footer.kl-foot")).toBeVisible();
  await expect(page.locator("footer.kl-foot")).toContainText("Devnet beta");

  await page.goto("/");
  await page.goto("/#faq-dealers");
  await expect(page.locator("#faq-dealers")).toBeInViewport();
  await page.locator("#faq-dealers summary").click();
  await expect(page.getByText(/During the beta, Keeler lists the dealers/)).toBeVisible();
  await page.evaluate(() => (location.hash = "earn"));
  await expect(panel(page)).toBeVisible();
  await page.evaluate(() => (location.hash = "faq"));
  await expect(page.locator("#faq")).toBeInViewport();
});
