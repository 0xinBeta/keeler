// The landing's illustrative instruments, ported from the design handoff's lending-instruments.js: the supply
// example, the collateral slider, the price stress and the privacy switch. Fixed sample figures, labelled as such;
// the rate and the vault's percentages are the keeper's.
import { useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { Pair } from "../Landing";
import type { Market } from "../Lend";

const PRICE_CENTS = 15154;
const DEBT_CENTS = 42000;
const RISK_COLLATERAL = 4;
const METER_CEILING = 110;
const units = new Intl.NumberFormat("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const price = (cents: number) => units.format(cents / 100);
const pct = (bps: number) => `${(bps / 100).toFixed(2)}%`;

/** The vault's borrow limit and liquidation threshold in percent; the program's 80 and 85 until the market is read. */
export function limitsOf(market: Market | null) {
  const v = market?.vaults[0];
  return { borrow: v ? v.collateralFactorBps / 100 : 80, liquidation: v ? v.liquidationThresholdBps / 100 : 85 };
}

const vars = (v: Record<string, string | number>) => v as CSSProperties;

export function SupplyExample({ pair, market }: { pair: Pair; market: Market | null }) {
  const pool = market?.pools[0];
  return (
    <div className="kl-instrument kl-inst-pool">
      <div className="kl-pool-values">
        <div className="kl-value">
          <span>Illustrative deposit</span>
          <strong>
            750 <small>{pair.debt}</small>
          </strong>
        </div>
        <div className="kl-value kl-value-out">
          <span>Pool shares minted</span>
          <strong>
            750 <small>shares</small>
          </strong>
        </div>
      </div>
      <div className="kl-pool-path" aria-hidden="true">
        <span className="kl-thread kl-thread-upper" />
        <span className="kl-thread kl-thread-center" />
        <span className="kl-thread kl-thread-lower" />
        <span className="kl-pool-seal" />
      </div>
      <p className="kl-note">Illustrative share price of 1.00 {pair.debt} per share.</p>
      <p className="kl-rate">
        {pool ? (
          <>
            <span>{pool.asset.symbol} supply APR now</span> <strong>{pct(pool.supplyAprBps)}</strong>
            <span>Borrow at {pct(pool.borrowAprBps)}</span>
          </>
        ) : (
          <span>Reading the pool…</span>
        )}
      </p>
    </div>
  );
}

export function CollateralSlider({ pair, market }: { pair: Pair; market: Market | null }) {
  const [collateral, setCollateral] = useState(4);
  const { borrow } = limitsOf(market);
  const capacity = price(Math.floor((PRICE_CENTS * collateral * borrow) / 100));
  const min = 0.5;
  const max = 8;
  return (
    <div className="kl-instrument kl-inst-borrow" style={vars({ "--kl-progress": (collateral - min) / (max - min) })}>
      <div className="kl-control-row">
        <div className="kl-value">
          <label htmlFor="kl-collateral">{pair.collateral} collateral</label>
          <output htmlFor="kl-collateral" className="kl-number" data-kl="collateral">
            {collateral} <small>{pair.collateral}</small>
          </output>
        </div>
        <div className="kl-value kl-value-out">
          <span>Illustrative borrow capacity</span>
          <output htmlFor="kl-collateral" className="kl-number" data-kl="capacity">
            {capacity} <small>{pair.debt}</small>
          </output>
        </div>
      </div>
      <div className="kl-slider">
        <input
          type="range"
          id="kl-collateral"
          min={min}
          max={max}
          step={0.5}
          value={collateral}
          onChange={(e) => setCollateral(e.currentTarget.valueAsNumber)}
          aria-describedby="kl-draw-note"
          aria-valuetext={`${collateral} ${pair.collateral} collateral, ${capacity} ${pair.debt} illustrative borrow capacity`}
        />
        <span className="kl-slider-track" aria-hidden="true" />
      </div>
      <div className="kl-extents" aria-hidden="true">
        <span>
          {min} {pair.collateral}
        </span>
        <span>
          {max} {pair.collateral}
        </span>
      </div>
      <p className="kl-note" id="kl-draw-note">
        Illustrative price {price(PRICE_CENTS)} {pair.debt} per {pair.collateral}. {borrow}% limit with no existing debt.
      </p>
    </div>
  );
}

export function PriceStress({ pair, market }: { pair: Pair; market: Market | null }) {
  const [drop, setDrop] = useState(0);
  const { borrow, liquidation } = limitsOf(market);
  const stressed = Math.round((PRICE_CENTS * (100 - drop)) / 100);
  const ltv = Math.ceil((DEBT_CENTS * 10000) / (RISK_COLLATERAL * stressed)) / 100;
  const health = ltv > liquidation ? "liquidatable" : ltv > borrow ? "near" : "within";
  const label = { liquidatable: "Liquidatable", near: "Above borrow limit", within: "Within borrow limit" }[health];
  const shown = `${ltv.toFixed(1)}%`;
  const liquidationPrice = price(Math.round((DEBT_CENTS * 100) / (RISK_COLLATERAL * liquidation)));
  const max = 35;
  return (
    <div
      className="kl-instrument kl-inst-risk"
      data-health={health}
      style={vars({
        "--kl-progress": drop / max,
        "--kl-ltv-fill": Math.min(1, ltv / METER_CEILING),
        "--kl-borrow-mark": `${(borrow / METER_CEILING) * 100}%`,
        "--kl-liquidation-mark": `${(liquidation / METER_CEILING) * 100}%`,
      })}
    >
      <div className="kl-control-row kl-risk-header">
        <div className="kl-value">
          <span>Loan-to-value</span>
          <output htmlFor="kl-stress" className="kl-number" data-kl="ltv">
            {shown}
          </output>
        </div>
        <span className="kl-state" data-kl="health">
          {label}
        </span>
      </div>
      <div
        className="kl-meter"
        role="meter"
        aria-label="Loan-to-value"
        aria-valuemin={0}
        aria-valuemax={METER_CEILING}
        aria-valuenow={ltv}
        aria-valuetext={`${shown}, ${label.toLowerCase()}`}
      >
        <span className="kl-meter-fill" aria-hidden="true" />
        <span className="kl-meter-mark kl-borrow-mark" aria-hidden="true" />
        <span className="kl-meter-mark kl-liquidation-mark" aria-hidden="true" />
      </div>
      <div className="kl-meter-labels">
        <span>{borrow}% borrow limit</span>
        <span>{liquidation}% liquidation threshold</span>
      </div>
      <div className="kl-stress-row">
        <label htmlFor="kl-stress">{pair.collateral} price drop</label>
        <output htmlFor="kl-stress">{drop}%</output>
      </div>
      <div className="kl-slider">
        <input
          type="range"
          id="kl-stress"
          min={0}
          max={max}
          step={1}
          value={drop}
          onChange={(e) => setDrop(e.currentTarget.valueAsNumber)}
          aria-describedby="kl-stress-note"
          aria-valuetext={`${drop}% ${pair.collateral} price drop, ${shown} loan-to-value, ${label.toLowerCase()}`}
        />
        <span className="kl-slider-track" aria-hidden="true" />
      </div>
      <p className="kl-note" id="kl-stress-note">
        Illustrative position of {RISK_COLLATERAL} {pair.collateral} against {DEBT_CENTS / 100} {pair.debt} debt.
        <span className="kl-price-levels">
          <span>
            Illustrative {pair.collateral} price {price(stressed)} {pair.debt}
          </span>
          <span>
            Liquidation price ≈{liquidationPrice} {pair.debt}
          </span>
        </span>
      </p>
    </div>
  );
}

export function PrivacySwitch({ pair }: { pair: Pair }) {
  const [position, setPosition] = useState(false);
  const readout = useRef<HTMLDivElement>(null);
  const pick = (next: boolean) => {
    if (next === position) return;
    setPosition(next);
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    readout.current?.animate?.([{ clipPath: "inset(48% 0 48%)", opacity: 0.3 }, { clipPath: "inset(0)", opacity: 1 }], {
      duration: 230,
      easing: "cubic-bezier(.22,.74,.25,1)",
    });
  };
  return (
    <div className="kl-instrument kl-privacy-switch">
      <div className="kl-privacy-controls" role="group" aria-label="Position visibility">
        <button type="button" aria-controls="kl-privacy-readout" aria-pressed={!position} onClick={() => pick(false)}>
          Public chain
        </button>
        <button type="button" aria-controls="kl-privacy-readout" aria-pressed={position} onClick={() => pick(true)}>
          Your position
        </button>
      </div>
      <div className="kl-privacy-readout" id="kl-privacy-readout" ref={readout} role="status" aria-atomic="true">
        <strong data-kl="privacy">
          {position
            ? `${RISK_COLLATERAL} ${pair.collateral} collateral · ${DEBT_CENTS / 100} ${pair.debt} debt`
            : `${pair.collateral} / ${pair.debt} vault`}
        </strong>
        <span>{position ? "Illustrative · Visible to you and the pool keeper" : "Collateral and debt amounts hidden"}</span>
      </div>
    </div>
  );
}
