/// <reference types="vite/client" />
import "@fontsource/unbounded/300.css";
import "@fontsource/unbounded/400.css";
import "@fontsource-variable/onest/wght.css";
import "./styles/app.css";
import { createRoot } from "react-dom/client";
import type { Keeler } from "./keeler/index";
import { createMockKeeler } from "./keeler/mock";
import { App } from "./ui/App";

// The real client may not exist yet; a glob import resolves to nothing instead of failing the build.
const clients = import.meta.glob<{ createKeeler: () => Keeler | Promise<Keeler> }>("./keeler/client.ts");

async function keeler(): Promise<Keeler> {
  const load = clients["./keeler/client.ts"];
  if (import.meta.env.VITE_KEELER_MOCK === "1" || !load) return createMockKeeler();
  return (await load()).createKeeler();
}

keeler().then((k) => createRoot(document.getElementById("root")!).render(<App keeler={k} />));
