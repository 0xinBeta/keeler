import { Fragment, useEffect, useState } from "react";
import { KeelerError } from "../keeler/index";
import type { Asset, Balance, Config } from "../keeler/index";
import { orbTx } from "../keeler/network";
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

/** An amount as plain text ("12.5 tSOL"), for captions that are not markup. */
export const amountText = (value: bigint, asset: Asset) => `${formatUnits(value, asset.decimals)} ${asset.symbol}`;

/** One line on every desk while funds wait on a click in Activity. */
export function RefundNotice({ count }: { count: number }) {
  if (count === 0) return null;
  return (
    <p className="note note-refund">
      {count === 1 ? "One order or operation has" : `${count} orders or operations have`} funds you can take back.{" "}
      <a href="#activity">Go to Activity</a>
    </p>
  );
}

/** The public and private balance of each asset, beside a desk. */
export function Funds({ balances }: { balances: Balance[] | null }) {
  return (
    <section className="rail-panel" aria-label="Available funds">
      <div className="rail-head">
        <h2>Available funds</h2>
        <a className="rail-link" href="#balances">
          Manage balances
        </a>
      </div>
      <table className="data-table funds-table">
        <thead>
          <tr>
            <th scope="col">Asset</th>
            <th scope="col">Public</th>
            <th scope="col">Private</th>
          </tr>
        </thead>
        <tbody>
          {(balances ?? []).map((b) => (
            <tr key={b.asset.mint}>
              <th scope="row">{b.asset.symbol}</th>
              <td className="num">{formatUnits(b.public, b.asset.decimals)}</td>
              <td className="num">{formatUnits(b.private, b.asset.decimals)}</td>
            </tr>
          ))}
          {!balances && (
            <tr>
              <td colSpan={3} className="muted">
                Reading balances…
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </section>
  );
}

/**
 * What is public on chain for each kind of operation: lending and moves from the README's public and hidden table, an
 * unfilled or refunded request from the `intent` and `cancel_intent` instructions, swaps from the README's Swaps
 * section (the swap circuits' public inputs), registration from the user registry record (the nullifier and viewing public keys).
 */
export const ON_CHAIN = {
  swap: "Public on chain: who traded, and that the order was made and then settled or refunded. The tokens, the amounts and the price are not.",
  lend: "Public on chain: the wallets, the instruction and which pool or vault. The amounts are not.",
  request: "Public on chain: your wallet, the token and, on a refund, the request's expiry. The amount is not, and neither is what the request is for until a keeper fills it.",
  liquidation: "Public on chain: the wallets, that the position was liquidated, the vault and the price bound. The amounts are not.",
  move: "Public on chain: your wallet and the amount moved.",
  register: "Public on chain: your wallet and the public keys of your private balance.",
} as const;

export type OrbLink = { signature?: string; label: string };

const hrefs = (links: OrbLink[]) =>
  links.flatMap((l) => {
    const href = l.signature ? orbTx(l.signature) : undefined;
    return href ? [{ href, label: l.label }] : [];
  });

/** Each landed transaction's page on Orb and, when given, what of it is public; nothing when no link can be built. */
export function OrbLinks({ links, says }: { links: OrbLink[]; says?: string }) {
  const shown = hrefs(links);
  if (shown.length === 0) return null;
  return (
    <>
      <p className="order-note">
        {shown.map((l, i) => (
          <Fragment key={l.href}>
            {i > 0 && <span aria-hidden="true"> · </span>}
            <a href={l.href} target="_blank" rel="noopener noreferrer">
              {l.label}
              <span className="sr-only"> (opens in a new tab)</span>
            </a>
          </Fragment>
        ))}
      </p>
      {says && <p className="order-note">{says}</p>}
    </>
  );
}

/** Below a list whose rows show their links alone: each distinct sentence of the rows that show a link, once. */
export function OnChainNotes({ rows }: { rows: { links: OrbLink[]; says: string }[] }) {
  const says = [...new Set(rows.filter((r) => hrefs(r.links).length > 0).map((r) => r.says))];
  return says.map((s) => (
    <p key={s} className="order-note">
      {s}
    </p>
  ));
}

export function ErrorNote({ text }: { text: string | null }) {
  if (!text) return null;
  return (
    <p className="note note-error" role="alert">
      {text}
    </p>
  );
}
