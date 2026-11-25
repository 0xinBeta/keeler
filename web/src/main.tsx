/// <reference types="vite/client" />
import "./styles/tokens.css";
import "./styles/app.css";
import { createRoot } from "react-dom/client";
import type { Keeler } from "./keeler/index";
import { App } from "./ui/App";

// The real client may not exist yet; a glob import resolves to nothing instead of failing the build.
const clients = import.meta.glob<{ createKeeler: () => Keeler | Promise<Keeler> }>("./keeler/client.ts");

async function keeler(): Promise<Keeler> {
  // Statically replaced at build time, so a build without the flag never bundles the mock.
  if (import.meta.env.VITE_KEELER_MOCK === "1") return (await import("./keeler/mock")).createMockKeeler();
  const load = clients["./keeler/client.ts"];
  if (!load) throw new Error("Keeler client missing: build with src/keeler/client.ts, or set VITE_KEELER_MOCK=1 for the mock.");
  return (await load()).createKeeler();
}

keeler().then((k) => createRoot(document.getElementById("root")!).render(<App keeler={k} />));
