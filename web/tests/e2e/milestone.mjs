// Scratch driver for the first milestone; replaced by swap.spec.ts.
import { chromium } from "@playwright/test";

const browser = await chromium.launchPersistentContext(new URL("../.localnet/profile", import.meta.url).pathname);
const page = await browser.newPage();
page.on("console", (m) => console.log("[page]", m.type(), m.text()));
page.on("pageerror", (e) => console.log("[pageerror]", e.message));
await page.goto("http://127.0.0.1:5180/tests/harness/index.html");
await page.waitForFunction(() => window.ready);
const step = async (name, fn, arg) => {
  const t = Date.now();
  try {
    const out = await page.evaluate(fn, arg);
    console.log(`OK ${name} (${Date.now() - t} ms)`, out ?? "");
    return out;
  } catch (e) {
    console.log(`FAIL ${name}:`, e.message);
    await browser.close();
    process.exit(1);
  }
};
await step("connect", async () => {
  const k = window.keeler;
  const w = k.wallets().find((w) => w.name.includes("in-page"));
  return (await k.connect(w)).address;
});
if (process.env.MODE === "refund") {
  await step("unlock", () => window.keeler.unlock());
  const orders = await step("orders", async () => (await window.keeler.orders()).map((o) => [o.id, o.status]));
  for (const [id, st] of orders) if (st === "refundable") await step("refund", (id) => window.keeler.refund(id), id);
  await step("orders", async () => (await window.keeler.orders()).map((o) => `${o.id.slice(0, 8)} ${o.status}`));
  await step("balances", async () => (await window.keeler.balances()).map((x) => `${x.asset.symbol} pub=${x.public} priv=${x.private}`).join("; "));
  await browser.close();
  process.exit(0);
}
await step("faucet", () => window.keeler.faucet());
await step("unlock", () => window.keeler.unlock());
await step("register", async () => {
  if (!(await window.keeler.isRegistered())) await window.keeler.register();
  return window.keeler.isRegistered();
});
const bal = (b) => b.map((x) => `${x.asset.symbol} pub=${x.public} priv=${x.private} max=${x.largestNote}`).join("; ");
await step("deposit", async () => {
  const k = window.keeler;
  const [spl] = (await k.config()).assets;
  await k.deposit(spl, 200_000_000n);
  return (await k.balances()).map((x) => `${x.asset.symbol} pub=${x.public} priv=${x.private} max=${x.largestNote}`).join("; ");
});
await page.request.post("http://127.0.0.1:5180/__test/idle", { data: { on: process.env.IDLE !== "0" } });
const order = await step("swap", async () => {
  const k = window.keeler;
  const [spl, sol] = (await k.config()).assets;
  const { fair, quotes } = await k.quotes(spl, sol, 100_000_000n);
  const steps = [];
  const o = await k.swap(quotes[0], (s) => steps.push(s));
  return { fair: String(fair), dealer: quotes[0].dealer, id: o.id, status: o.status, expiry: o.expiry, steps, proofMs: window.proofMs() };
});
for (;;) {
  const s = await step("orders", async () => (await window.keeler.orders()).map((o) => `${o.id.slice(0, 8)} ${o.status}`));
  if (s.some((x) => x.endsWith("refundable") || x.endsWith("settled"))) break;
  await new Promise((r) => setTimeout(r, 5000));
}
if (process.env.IDLE !== "0") {
  await step("refund", (id) => window.keeler.refund(id), order.id);
  await step("orders", async () => (await window.keeler.orders()).map((o) => `${o.id.slice(0, 8)} ${o.status}`));
  await step("proofMs", () => window.proofMs());
}
await step("balances", async () => (await window.keeler.balances()).map((x) => `${x.asset.symbol} pub=${x.public} priv=${x.private}`).join("; "));
await browser.close();
