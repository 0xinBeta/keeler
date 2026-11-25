import { useEffect, useState } from "react";
import { KeelerError } from "../keeler/index";
import type { Asset, Config } from "../keeler/index";
import { formatUnits } from "./format";

const PILL = { local: "Local", devnet: "Devnet beta", mainnet: "Mainnet" } as const;

export function NetworkPill({ config }: { config: Config | null }) {
  if (!config) return null;
  return (
    <span className="net-pill">
      <span className="net-dot" aria-hidden="true" />
      {PILL[config.network]}
    </span>
  );
}

export function Logo() {
  return (
    <svg viewBox="0 0 40 24" width="34" height="20" aria-hidden="true" className="logo">
      <ellipse cx="20" cy="12" rx="17" ry="7" fill="none" stroke="currentColor" strokeWidth="1.6" />
      <ellipse cx="20" cy="12" rx="12" ry="4.6" fill="none" stroke="currentColor" strokeWidth="1.6" opacity=".55" />
      <circle cx="31.5" cy="8.6" r="1.9" fill="#F4EBD6" />
    </svg>
  );
}

/** Chain time in seconds, ticking each second from the last `chainNow()` reading. */
export function useChainNow(offset: number | null): number | null {
  const [now, setNow] = useState(() => (offset === null ? null : Date.now() / 1000 + offset));
  useEffect(() => {
    if (offset === null) return;
    const tick = () => setNow(Date.now() / 1000 + offset);
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  }, [offset]);
  return now;
}

export function errorText(e: unknown): string {
  if (e instanceof KeelerError) return e.message;
  const m = e instanceof Error ? e.message : String(e);
  return `Something went wrong (${m}). Reload the page and try again; your funds are not affected by this message.`;
}

export function Amount({ value, asset }: { value: bigint; asset: Asset }) {
  return (
    <span className="amount">
      {formatUnits(value, asset.decimals)} <span className="sym">{asset.symbol}</span>
    </span>
  );
}

export function ErrorNote({ text }: { text: string | null }) {
  if (!text) return null;
  return (
    <p className="note note-error" role="alert">
      {text}
    </p>
  );
}
