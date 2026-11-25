// The landing on the mock client. LEND_SCREENS=<dir> also saves screenshots there, named lend-<project>-<n>-<what>.jpg.
import { expect, test, type Page, type TestInfo } from "@playwright/test";

const panel = (page: Page) => page.getByRole("region", { name: "Private swap" });

async function shot(page: Page, info: TestInfo, name: string, target: string) {
  const dir = process.env.LEND_SCREENS;
  if (!dir) return;
  await page.waitForTimeout(400);
  await page.locator(target).screenshot({ path: `${dir}/lend-${info.project.name}-${name}.jpg`, type: "jpeg", quality: 82 });
}

test("landing: lend sections and FAQ", async ({ page }, info) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");
  await expect(panel(page)).toBeHidden();
  await expect(page.getByRole("heading", { name: /Jupiter-style lending\. The chain never sees your position's size\./ })).toBeVisible();
  await expect(page.getByText(/supply APR now/)).toBeVisible();
  const privacy = page.locator("#lend-privacy");
  await privacy.getByRole("button", { name: "On Keeler" }).click();
  await expect(privacy.getByText("Here they can't.", { exact: false })).toBeVisible();
  for (const q of ["Who runs the pool?", "What if the keeper is offline?", "What is visible when I lend or borrow?", "Is Keeler on mainnet?"]) {
    await page.locator("#faq summary", { hasText: q }).click();
  }
  await expect(page.getByText(/By design it cannot take your principal/)).toBeVisible();
  for (const [id, name] of [["#lend", "09-landing-lend"], ["#lend-privacy", "10-landing-privacy"], ["#liquidation", "11-landing-liquidation"], [".section-facts", "12-landing-numbers"], ["#faq", "13-landing-faq"]] as const)
    await shot(page, info, name, id);
  await page.getByRole("button", { name: "Start earning" }).click();
  await expect(page).toHaveURL(/#earn$/);
  await expect(panel(page).getByRole("heading", { name: "Choose a wallet" })).toBeFocused();
  await expect(page.locator("#faq")).toHaveCount(0);
});
