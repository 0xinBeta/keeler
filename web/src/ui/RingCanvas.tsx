import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { Ring } from "./ring";
import type { RingInput, RingLayout } from "./ring";

type Props =
  | { mode: "hero"; input: RingInput; anchor: RefObject<HTMLElement | null>; hero: RefObject<HTMLElement | null> }
  | { mode: "arc" };

const IDLE: RingInput = { dealers: null, proving: false, settled: 0 };

/**
 * hero: covers its positioned parent down to the bottom of `hero`, the ring around `anchor` (the swap panel).
 * arc: fills its positioned parent with the far half of a large ring, a slow arc for the closing section.
 */
export function RingCanvas(props: Props) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const ring = useRef<Ring | null>(null);
  const [failed, setFailed] = useState(false);
  const anchor = props.mode === "hero" ? props.anchor : null;
  const hero = props.mode === "hero" ? props.hero : null;
  const input = props.mode === "hero" ? props.input : IDLE;

  useEffect(() => {
    const el = canvas.current!;
    const host = el.parentElement!;
    const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
    let r: Ring;
    try {
      r = new Ring(el, still, anchor ? 1 : 0.45);
    } catch {
      setFailed(true);
      return;
    }
    ring.current = r;
    // shortcut: a lost context falls back to the CSS ring until reload; restoring it is not worth the code yet.
    const lost = (e: Event) => {
      e.preventDefault();
      setFailed(true);
    };
    el.addEventListener("webglcontextlost", lost);
    const place = () => {
      const box = host.getBoundingClientRect();
      if (!anchor) {
        const w = box.width;
        const h = box.height;
        r.resize(w, h, { cx: w * 0.5, cy: h * 0.92, radius: Math.max(w * 0.62, 420), lights: 1.6 });
        return;
      }
      const bottom = hero?.current?.getBoundingClientRect().bottom ?? box.bottom;
      const h = { left: box.left, top: box.top, width: box.width, height: Math.max(bottom - box.top, 1) };
      el.style.height = `${h.height}px`;
      const a = anchor.current?.getBoundingClientRect();
      let layout: RingLayout;
      if (h.width >= 800 && a) {
        layout = {
          // Centred a little left of the panel and wide, so the ring reaches behind the headline's right edge.
          cx: a.left - h.left + a.width * 0.42,
          // Anchored to the panel's top, not its height, so the ring does not jump as the panel changes.
          cy: a.top - h.top + 300,
          radius: Math.max(a.width * 1.55, h.width * 0.5),
          lights: 3.35, // near arc, left of the panel
        };
      } else {
        const top = a ? a.top - h.top : 0;
        // One column: the ring sits above the panel, its near arc passing behind the panel's top.
        layout = {
          cx: a ? a.left - h.left + a.width * 0.55 : h.width / 2,
          cy: top - 24,
          radius: Math.min(h.width * 0.98, 460),
          lights: 1.2, // far arc, above the panel
        };
      }
      r.resize(h.width, h.height, layout);
    };
    place();
    const ro = new ResizeObserver(place);
    ro.observe(host);
    if (hero?.current) ro.observe(hero.current);
    return () => {
      ro.disconnect();
      el.removeEventListener("webglcontextlost", lost);
      r.destroy();
      ring.current = null;
    };
  }, [anchor, hero]);

  useEffect(() => {
    ring.current?.set(input);
  }, [input]);

  const cls = anchor ? "ring" : "ring ring-arc";
  return (
    <>
      <canvas ref={canvas} className={cls} aria-hidden="true" hidden={failed} />
      {failed && <div className={`${cls} ring-fallback`} aria-hidden="true" />}
    </>
  );
}
