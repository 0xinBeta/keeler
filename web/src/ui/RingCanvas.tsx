import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { Ring } from "./ring";
import type { RingInput, RingLayout } from "./ring";

type Props = { input: RingInput; anchor: RefObject<HTMLElement | null>; hero: RefObject<HTMLElement | null> };

/** Covers its positioned parent down to the bottom of `hero`; centres the ring on `anchor` (the swap panel). */
export function RingCanvas({ input, anchor, hero }: Props) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const ring = useRef<Ring | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const el = canvas.current!;
    const host = el.parentElement!;
    const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
    let r: Ring;
    try {
      r = new Ring(el, still);
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
      const bottom = hero.current?.getBoundingClientRect().bottom ?? box.bottom;
      const h = { left: box.left, top: box.top, width: box.width, height: Math.max(bottom - box.top, 1) };
      el.style.height = `${h.height}px`;
      const a = anchor.current?.getBoundingClientRect();
      let layout: RingLayout;
      if (h.width >= 800 && a) {
        layout = {
          cx: a.left - h.left + a.width * 0.62,
          // Anchored to the panel's top, not its height, so the ring does not jump as the panel changes.
          cy: a.top - h.top + 290,
          radius: Math.max(a.width * 1.3, h.width * 0.4),
        };
      } else {
        const top = a ? a.top - h.top : 0;
        // One column: the ring sits above the panel, its near arc passing behind the panel's top.
        layout = { cx: h.width * 0.56, cy: top - 20, radius: h.width * 0.86 };
      }
      r.resize(h.width, h.height, layout);
    };
    place();
    const ro = new ResizeObserver(place);
    ro.observe(host);
    if (hero.current) ro.observe(hero.current);
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

  return (
    <>
      <canvas ref={canvas} className="ring" aria-hidden="true" hidden={failed} />
      {failed && <div className="ring ring-fallback" aria-hidden="true" />}
    </>
  );
}
