import { useState } from "react";
import type { Keeler, Order } from "../keeler/index";
import { countdown } from "./format";
import { Amount, ErrorNote, errorText, ON_CHAIN, OrbLinks, useChainNow } from "./shared";

const LABEL: Record<Order["status"], string> = {
  pending: "Sending",
  locked: "Waiting for the dealer",
  settled: "Settled",
  refundable: "Dealer did not settle",
  refunded: "Funds taken back",
  failed: "Failed",
};

/** The order's make and, once spent, its settle or refund, each linked on Orb. */
export function OrderLinks({ order }: { order: Order }) {
  const spent = order.status === "refunded" ? "View refund on Orb" : "View settlement on Orb";
  return <OrbLinks links={[{ signature: order.signature, label: "View order on Orb" }, { signature: order.spentBy, label: spent }]} says={ON_CHAIN.swap} />;
}

type Props = {
  keeler: Keeler;
  orders: Order[];
  chainOffset: number | null;
  refresh: () => Promise<unknown>;
  refreshBalances: () => Promise<unknown>;
  rescan: () => Promise<Order[]>;
};

export function Orders({ keeler, orders, chainOffset, refresh, refreshBalances, rescan }: Props) {
  const now = useChainNow(chainOffset);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [scan, setScan] = useState<"idle" | "busy" | "done">("idle");

  const find = async () => {
    setScan("busy");
    setError(null);
    try {
      await rescan();
      setScan("done");
    } catch (e) {
      setError(errorText(e));
      setScan("idle");
    }
  };

  const refund = async (id: string) => {
    setBusy(id);
    setError(null);
    try {
      await keeler.refund(id);
      await Promise.all([refresh(), refreshBalances()]);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="activity" aria-labelledby="orders-title">
      <div className="panel-header">
        <h2 id="orders-title">Your orders</h2>
        <button className="button button-secondary button-small" onClick={find} disabled={scan === "busy"} aria-busy={scan === "busy"}>
          {scan === "busy" ? "Reading the chain…" : "Find my orders"}
        </button>
      </div>
      <p className="status muted" aria-live="polite">
        {scan === "busy" ? "Looking for this wallet's orders on the chain. This can take a little while." : ""}
      </p>
      {orders.length === 0 ? (
        <p className="activity-empty">
          {scan === "done"
            ? "No orders found for this wallet."
            : "No orders yet. Get quotes in the Swap view to make your first private swap, or find orders made from another browser."}
        </p>
      ) : (
        <ul className="orders">
          {orders.map((o) => {
            const left = now === null ? null : o.expiry - now;
            return (
              <li key={o.id} className={`order order-${o.status}`}>
                <span className="order-pair">
                  <Amount value={o.sellAmount} asset={o.sell} />
                  <span className="muted" aria-hidden="true">
                    {" "}→{" "}
                  </span>
                  <span className="sr-only"> for </span>
                  <Amount value={o.buyAmount} asset={o.buy} />
                </span>
                <span className="order-status">
                  <span className="badge">{LABEL[o.status]}</span>
                  {o.status === "locked" && left !== null && (
                    <span className="muted num">{left > 0 ? `refund possible in ${countdown(left)}` : "checking…"}</span>
                  )}
                  {o.error && <span className="bad">{o.error}</span>}
                </span>
                {o.status === "locked" && (
                  <span className="order-note muted">
                    If the price moves, a dealer can pass. Nothing is lost: you take your funds back when the quote
                    expires.
                  </span>
                )}
                <OrderLinks order={o} />
                {o.status === "refundable" && (
                  <button className="button button-primary button-small" onClick={() => refund(o.id)} disabled={!!busy} aria-busy={busy === o.id}>
                    {busy === o.id ? "Taking funds back…" : "Take funds back"}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
      <ErrorNote text={error} />
    </section>
  );
}
