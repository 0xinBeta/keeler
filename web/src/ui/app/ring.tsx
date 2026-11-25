// The desk ring beside the active desk: one scene for the page's life, fed only by the visible desk's model.
import { useEffect, useRef, useState } from "react";
import type { DeskModel, DeskTab } from "../orbit/model";
import type { Desk, RendererKind } from "../orbit/scene";

/** What the ring shows: the model it draws and the caption beside it (a projection only for a valid amount). */
export type Scene = { model: DeskModel; label: string; value: string };
export type Report = (tab: DeskTab, scene: Scene) => void;

/** Sends a desk's scene up whenever it changes, so the panel can show the visible desk's. */
export function useReport(tab: DeskTab, scene: Scene, report: Report) {
  const key = JSON.stringify(scene);
  useEffect(() => report(tab, JSON.parse(key) as Scene), [tab, key, report]);
}

/** The ring and its caption. The scene mounts on the canvas's first non-zero layout and stays until unmount. */
export function DeskRing({ scene, hidden }: { scene: Scene; hidden: boolean }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const desk = useRef<Desk | null>(null);
  const latest = useRef(scene.model);
  latest.current = scene.model;
  const [renderer, setRenderer] = useState<RendererKind | null>(null);
  const key = JSON.stringify(scene.model);

  useEffect(() => {
    desk.current?.update(latest.current);
  }, [key]);

  useEffect(() => {
    const el = canvas.current!;
    let cancelled = false;
    let started = false;
    const observer = new ResizeObserver(() => {
      if (started || el.clientWidth === 0 || el.clientHeight === 0) return;
      started = true;
      observer.disconnect();
      import("../orbit/scene").then(
        ({ mountDesk }) => {
          if (cancelled) return;
          desk.current = mountDesk(el, { onRenderer: setRenderer });
          desk.current.update(latest.current);
        },
        () => !cancelled && setRenderer("still"),
      );
    });
    observer.observe(el);
    return () => {
      cancelled = true;
      observer.disconnect();
      desk.current?.destroy();
      desk.current = null;
    };
  }, []);

  return (
    <figure className="observatory" hidden={hidden} data-renderer={renderer ?? undefined}>
      <div className="aperture" aria-hidden="true">
        <canvas ref={canvas} />
      </div>
      <figcaption>
        <span>{scene.label}</span>
        <strong>{scene.value}</strong>
      </figcaption>
    </figure>
  );
}
