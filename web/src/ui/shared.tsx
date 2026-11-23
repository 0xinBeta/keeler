import { useEffect, useState } from "react";
import { KeelerError } from "../keeler/index";
import type { Asset } from "../keeler/index";
import { formatUnits } from "./format";

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
