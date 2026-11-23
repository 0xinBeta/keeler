// Exposes the swap client to Playwright: `window.keeler`, `window.KeelerError`.
import { createKeeler } from "../../src/keeler/client";
import { KeelerError } from "../../src/keeler/index";
import { lastProofMs } from "../../src/keeler/swap";

Object.assign(window, { keeler: createKeeler(), KeelerError, proofMs: () => lastProofMs, ready: true });
