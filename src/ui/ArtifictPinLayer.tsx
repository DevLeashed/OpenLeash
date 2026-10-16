import { useState } from "react";
import type { ArtifictAnnotation } from "./artifictApi";

export type PinPoint = { x: number; y: number };
export type DraftPin = { point: PinPoint; label: string };

/** Normalise a click inside an element to percentages of its own box.
 *  Percentages rather than pixels so a pin means the same place when the
 *  preview is resized, reused inline, or shown at another width. */
export function pointFromEvent(event: { clientX: number; clientY: number }, box: HTMLElement): PinPoint | null {
  const rect = box.getBoundingClientRect();
  if (!rect.width || !rect.height) return null;
  const round = (value: number) => Math.round(value * 10) / 10;
  return {
    x: round(Math.min(100, Math.max(0, ((event.clientX - rect.left) / rect.width) * 100))),
    y: round(Math.min(100, Math.max(0, ((event.clientY - rect.top) / rect.height) * 100))),
  };
}

const pin = (active: boolean) => ({
  position: "absolute" as const,
  transform: "translate(-50%,-50%)",
  minWidth: 20,
  height: 20,
  padding: "0 5px",
  borderRadius: 999,
  border: "1px solid var(--bg)",
  background: active ? "var(--accent)" : "var(--raise)",
  color: active ? "var(--bg)" : "var(--fg)",
  font: "600 11px var(--mono)",
  cursor: "pointer",
  lineHeight: "18px",
});

/** Numbered comment pins over a preview surface.
 *
 *  The host owns this layer: a pin is drawn from a saved annotation, and a click
 *  on the surface only ever produces a *draft* point. Nothing here writes to the
 *  project or reaches the agent — saving an annotation and submitting feedback
 *  are separate, explicit actions. Formats the host cannot read into (an
 *  isolated HTML frame) carry their point through the frame's own bounded draft
 *  channel instead; `onPoint` is simply not wired for them. */
export function ArtifictPinLayer({ annotations, activeId, onSelect, onPoint, commentMode, children, label }: {
  annotations: ArtifictAnnotation[];
  /** The pin currently being described in the comment panel. */
  activeId?: string | null;
  onSelect?: (annotation: ArtifictAnnotation) => void;
  /** Wired only for host-readable previews; a point is a draft, never a save. */
  onPoint?: (point: PinPoint) => void;
  commentMode?: boolean;
  children: React.ReactNode;
  label: string;
}) {
  const [draft, setDraft] = useState<PinPoint | null>(null);
  const [box, setBox] = useState<HTMLDivElement | null>(null);
  return <div
    ref={setBox}
    aria-label={label}
    style={{ position: "relative", cursor: commentMode && onPoint ? "crosshair" : undefined }}
    onClick={commentMode && onPoint && box ? (event) => {
      const target = event.target as HTMLElement;
      // Interacting with the document itself (a link, a control) stays that
      // interaction; only a click on inert canvas drops a new pin.
      if (target.closest("[data-artifict-pin]") || target.closest("a,button,input,textarea,select,label,summary")) return;
      const point = pointFromEvent(event, box);
      if (!point) return;
      setDraft(point);
      onPoint(point);
    } : undefined}
  >
    {children}
    {draft && commentMode && <span aria-hidden="true" style={{ ...pin(true), left: `${draft.x}%`, top: `${draft.y}%`, pointerEvents: "none" }}>+</span>}
    {annotations.map((annotation, index) => annotation.point && <button
      key={annotation.id}
      type="button"
      data-artifict-pin=""
      aria-label={`Comment ${index + 1}: ${annotation.text.slice(0, 80)}`}
      aria-pressed={annotation.id === activeId}
      title={annotation.text.slice(0, 200)}
      style={{ ...pin(annotation.id === activeId), left: `${annotation.point.x}%`, top: `${annotation.point.y}%` }}
      onClick={(event) => { event.stopPropagation(); onSelect?.(annotation); }}
    >{index + 1}</button>)}
  </div>;
}
