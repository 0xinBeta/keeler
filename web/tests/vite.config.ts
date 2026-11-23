// Dev server for the Playwright tests: the app's Vite setup with the
// test-only stand-in for the Worker (tests/standin.ts). Start it after the
// local network (tests/localnet), whose dealer keys it compiles in.
import { readFileSync } from "node:fs";
import { defineConfig } from "vite";
import { proxy, standin } from "./standin";

const net = JSON.parse(readFileSync(new URL("./.localnet/localnet.json", import.meta.url), "utf8"));
// What a deploy sets at build time (plan G1): the site's own copy of the dealer keys.
process.env.VITE_DEALERS = JSON.stringify([net.dealer.pubkey, net.idleDealer.pubkey]);

export default defineConfig({
  root: new URL("..", import.meta.url).pathname,
  plugins: [standin()],
  server: { port: 5180, strictPort: true, host: "127.0.0.1", proxy },
  worker: { format: "es" },
});
