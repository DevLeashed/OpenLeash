import { useEffect, useState } from "react";
import { MessageSquarePlus } from "lucide-react";
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

const KIND: Record<string, string> = {
  h1: "heading", h2: "heading", h3: "heading", h4: "heading", h5: "heading", h6: "heading",
  p: "paragraph", li: "list item", td: "table cell", th: "table header", code: "code", pre: "code block",
  blockquote: "quote", img: "image", a: "link", table: "table",
};

/** A readable name for what was clicked in a host-drawn preview — the same kind
 *  of label the HTML frame's bridge gives a picked element — so a comment says
 *  which heading or row it is about, not only where. */
export function describeTarget(target: EventTarget | null, root: HTMLElement): string {
  let el = target instanceof Element ? target : null;
  while (el && el !== root && !KIND[el.localName]) el = el.parentElement;
  if (!el || el === root) return "";
  const kind = KIND[el.localName]!;
  const text = (el.getAttribute("alt") || el.textContent || "").replace(/\s+/g, " ").trim();
  return text ? `${kind} “${text.length > 60 ? `${text.slice(0, 57)}…` : text}”` : kind;
}

export const pinStyle = (active: boolean) => ({
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
 *  isolated HTML frame) are pinned by the frame's own bridge instead; `onPoint`
 *  is simply not wired for them.
 *
 *  `pending` makes the draft point controlled (the studio owns it, so it can
 *  open a composer there); `composer` is drawn at that point.
 *
 *  Text can be commented on as text: select a passage and the comment is about
 *  exactly that quote. In comment mode the selection opens the composer at its
 *  end; otherwise a small Comment button is offered beside it, so selecting to
 *  copy still just selects. Quoted comments are highlighted in place. */
export function ArtifictPinLayer({ annotations, activeId, onSelect, onPoint, onQuote, commentMode, children, label, pending, pendingQuote, composer }: {
  annotations: ArtifictAnnotation[];
  /** The pin currently being described in the comment panel. */
  activeId?: string | null;
  onSelect?: (annotation: ArtifictAnnotation) => void;
  /** Wired only for host-readable previews; a point is a draft, never a save. */
  onPoint?: (point: PinPoint, target: string) => void;
  /** A passage the user selected to comment on, and where its selection ends. */
  onQuote?: (point: PinPoint, quote: string) => void;
  commentMode?: boolean;
  children: React.ReactNode;
  label: string;
  pending?: PinPoint | null;
  /** The quote the comment being written is about, highlighted while it is. */
  pendingQuote?: string | null;
  composer?: React.ReactNode;
}) {
  const [draft, setDraft] = useState<PinPoint | null>(null);
  const [box, setBox] = useState<HTMLDivElement | null>(null);
  const [offer, setOffer] = useState<{ point: PinPoint; quote: string } | null>(null);
  const shown = pending !== undefined ? pending : draft;
  useQuoteHighlights(box, annotations, activeId ?? null, pendingQuote ?? null);
  /** The selection inside this surface, as a quote and the point it ends at. */
  const readSelection = (): { point: PinPoint; quote: string } | null => {
    if (!box || typeof window === "undefined") return null;
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || !selection.rangeCount) return null;
    if (!box.contains(selection.anchorNode) || !box.contains(selection.focusNode)) return null;
    const quote = normalizeQuote(selection.toString());
    if (!quote) return null;
    const rect = selection.getRangeAt(0).getBoundingClientRect();
    const point = pointFromEvent({ clientX: rect.right, clientY: rect.bottom }, box);
    return point ? { point, quote } : null;
  };
  return <div
    ref={setBox}
    aria-label={label}
    className={commentMode && onPoint ? "artifict-pin-surface commenting" : "artifict-pin-surface"}
    style={{ position: "relative", cursor: commentMode && onPoint ? "crosshair" : undefined }}
    onMouseDown={(event) => { if (!(event.target as HTMLElement).closest("[data-artifict-quote-offer]")) setOffer(null); }}
    onMouseUp={onQuote ? (event) => {
      if ((event.target as HTMLElement).closest("[data-artifict-composer],[data-artifict-pin],[data-artifict-quote-offer]")) return;
      const picked = readSelection();
      if (!picked) return;
      if (commentMode) { setDraft(picked.point); onQuote(picked.point, picked.quote); }
      else setOffer(picked);
    } : undefined}
    onClick={commentMode && onPoint && box ? (event) => {
      const target = event.target as HTMLElement;
      // Interacting with the document itself (a link, a control) stays that
      // interaction; only a click on inert canvas drops a new pin. The composer
      // drawn at a pending point is part of this layer and is not canvas, and a
      // click that ended a text selection was a quote, not a point.
      if (target.closest("[data-artifict-pin]") || target.closest("[data-artifict-composer]") || target.closest("a,button,input,textarea,select,label,summary")) return;
      if (onQuote && readSelection()) return;
      const point = pointFromEvent(event, box);
      if (!point) return;
      setDraft(point);
      onPoint(point, describeTarget(target, box));
    } : undefined}
  >
    {children}
    {offer && onQuote && !shown && <button
      type="button"
      data-artifict-quote-offer=""
      className="artifict-quote-offer"
      style={{ left: `${offer.point.x}%`, top: `${offer.point.y}%` }}
      onClick={() => { const picked = offer; setOffer(null); setDraft(picked.point); onQuote(picked.point, picked.quote); }}
    ><MessageSquarePlus size={12} aria-hidden="true" />Comment</button>}
    {shown && commentMode && <span aria-hidden="true" style={{ ...pinStyle(true), left: `${shown.x}%`, top: `${shown.y}%`, pointerEvents: "none" }}>+</span>}
    {shown && composer && <div data-artifict-composer="" className="artifict-composer-anchor" data-flip-x={shown.x > 60 ? "true" : undefined} style={{ left: `${shown.x}%`, top: `${shown.y}%` }}>{composer}</div>}
    {annotations.map((annotation, index) => annotation.point && <button
      key={annotation.id}
      type="button"
      data-artifict-pin=""
      aria-label={`Comment ${index + 1}: ${annotation.text.slice(0, 80)}`}
      aria-pressed={annotation.id === activeId}
      title={annotation.text.slice(0, 200)}
      className="artifict-pin"
      data-sent={annotation.shared ? "true" : undefined}
      style={{ ...pinStyle(annotation.id === activeId), left: `${annotation.point.x}%`, top: `${annotation.point.y}%` }}
      onClick={(event) => { event.stopPropagation(); onSelect?.(annotation); }}
    >{index + 1}</button>)}
  </div>;
}

/** A selection as a quote: whitespace collapsed, bounded. */
export const MAX_QUOTE = 280;
export function normalizeQuote(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > MAX_QUOTE ? `${flat.slice(0, MAX_QUOTE - 1)}…` : flat;
}
/** A comment's quote, from the target it was saved with (“…”), or null. */
export function quoteOf(target: string | null | undefined): string | null {
  const match = /^“([\s\S]+)”$/.exec(target ?? "");
  return match ? match[1]!.replace(/…$/, "") : null;
}

/** Find a quote in the rendered text, matching across element boundaries with
 *  whitespace collapsed the way `normalizeQuote` collapsed it. */
export function findQuoteRange(root: HTMLElement, quote: string): Range | null {
  if (!quote || typeof document === "undefined") return null;
  const chars: { node: Text; offset: number }[] = [];
  let flat = "";
  let space = true;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (node) => (node.parentElement?.closest("[data-artifict-pin],[data-artifict-composer],[data-artifict-quote-offer]") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  for (let node = walker.nextNode() as Text | null; node; node = walker.nextNode() as Text | null) {
    const value = node.data;
    for (let i = 0; i < value.length; i++) {
      if (/\s/.test(value[i]!)) { if (space) continue; space = true; flat += " "; }
      else { space = false; flat += value[i]; }
      chars.push({ node, offset: i });
    }
  }
  const at = flat.indexOf(quote);
  if (at < 0) return null;
  const start = chars[at], end = chars[at + quote.length - 1];
  if (!start || !end) return null;
  const range = document.createRange();
  range.setStart(start.node, start.offset);
  range.setEnd(end.node, end.offset + 1);
  return range;
}

type HighlightRegistry = { set(name: string, value: unknown): void; delete(name: string): void };
/** Highlight quoted comments in place with the CSS Custom Highlight API: no
 *  wrapper elements in the rendered document, so nothing the markdown renderer
 *  owns is touched. Engines without the API simply show no highlight. */
function useQuoteHighlights(box: HTMLElement | null, annotations: ArtifictAnnotation[], activeId: string | null, pendingQuote: string | null) {
  const key = JSON.stringify([annotations.map((a) => [a.id, a.target]), activeId, pendingQuote]);
  useEffect(() => {
    const registry = (globalThis as { CSS?: { highlights?: HighlightRegistry } }).CSS?.highlights;
    const Highlight = (globalThis as { Highlight?: new (...ranges: Range[]) => unknown }).Highlight;
    if (!box || !registry || !Highlight) return;
    const quoted: Range[] = [], active: Range[] = [];
    for (const a of annotations) {
      const quote = quoteOf(a.target);
      const range = quote ? findQuoteRange(box, quote) : null;
      if (range) (a.id === activeId ? active : quoted).push(range);
    }
    const pendingRange = pendingQuote ? findQuoteRange(box, pendingQuote) : null;
    if (pendingRange) active.push(pendingRange);
    registry.set("artifict-quote", new Highlight(...quoted));
    registry.set("artifict-quote-active", new Highlight(...active));
    return () => { registry.delete("artifict-quote"); registry.delete("artifict-quote-active"); };
    // `key` stands for the inputs that change what is highlighted.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [box, key]);
}
