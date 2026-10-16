import { cloneElement, isValidElement, ReactElement, ReactNode, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

type Side = "top" | "bottom" | "left" | "right";
/** `start` lines a top/bottom tip up with the anchor's left edge, so a tip that
 *  spells out a clipped line starts exactly where that line starts. */
type Align = "center" | "start";
type Rect = Pick<DOMRect, "top" | "right" | "bottom" | "left" | "width" | "height">;
type Size = { width: number; height: number };
type Position = { left: number; top: number };

const GAP = 8;
const EDGE = 8;
/** `.tooltip`'s left padding plus its border: a `start` tip moves left by this
 *  much so its first character sits over the anchor's first character. */
const START_INSET = 9;
let activeTooltip: { owner: symbol; close: () => void } | null = null;

export function tooltipPosition(anchor: Rect, tip: Size, side: Side, viewport: Size, align: Align = "center"): Position {
  const space = {
    top: anchor.top - EDGE,
    bottom: viewport.height - anchor.bottom - EDGE,
    left: anchor.left - EDGE,
    right: viewport.width - anchor.right - EDGE,
  };
  const opposite: Record<Side, Side> = { top: "bottom", bottom: "top", left: "right", right: "left" };
  const needed = side === "top" || side === "bottom" ? tip.height + GAP : tip.width + GAP;
  const actual = space[side] < needed && space[opposite[side]] > space[side] ? opposite[side] : side;
  const clamp = (value: number, max: number) => Math.max(EDGE, Math.min(value, Math.max(EDGE, max - EDGE)));
  const left = actual === "left" ? anchor.left - tip.width - GAP
    : actual === "right" ? anchor.right + GAP
    : align === "start" ? anchor.left - START_INSET
    : anchor.left + (anchor.width - tip.width) / 2;
  const top = actual === "top" ? anchor.top - tip.height - GAP
    : actual === "bottom" ? anchor.bottom + GAP
    : anchor.top + (anchor.height - tip.height) / 2;
  return { left: clamp(left, viewport.width - tip.width), top: clamp(top, viewport.height - tip.height) };
}

/** Whether an element's text is cut off by its own `overflow: hidden`. */
export const isTruncated = (el: HTMLElement) => el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1;

/**
 * `truncated`: only show when the anchor's own text is clipped. A tip that just
 * repeats a line the user can already read in full is noise, and over a list it
 * covers the neighbouring row for nothing. Implies `align="start"` and left-aligned
 * text, so the full value reads as a continuation of the clipped one.
 */
export function Tooltip({ content, children, side = "top", delay = 200, truncated = false, align }: { content?: ReactNode; children: ReactElement; side?: Side; delay?: number; truncated?: boolean; align?: Align }) {
  const placement: Align = align ?? (truncated ? "start" : "center");
  const [target, setTarget] = useState<HTMLElement | null>(null);
  const [visible, setVisible] = useState(false);
  const [position, setPosition] = useState<Position | null>(null);
  // A clipped line's tip is set in the line's own face, so a mono path reads as
  // the same path rather than a reflowed one in another font.
  const [font, setFont] = useState<string | undefined>(undefined);
  const tooltip = useRef<HTMLDivElement>(null);
  const timer = useRef<number | undefined>(undefined);
  const owner = useRef(Symbol("tooltip"));

  const close = useCallback(() => {
    clearTimeout(timer.current);
    setVisible(false);
    setPosition(null);
    if (activeTooltip?.owner === owner.current) activeTooltip = null;
  }, []);

  useEffect(() => close, [close]);
  useLayoutEffect(() => {
    if (!visible || !target || !tooltip.current) return;
    const update = () => {
      if (!target.isConnected) { close(); return; }
      const rect = target.getBoundingClientRect();
      if (rect.bottom < 0 || rect.top > window.innerHeight || rect.right < 0 || rect.left > window.innerWidth) { close(); return; }
      const box = tooltip.current?.getBoundingClientRect();
      if (box) setPosition(tooltipPosition(rect, box, side, { width: window.innerWidth, height: window.innerHeight }, placement));
    };
    update();
    window.addEventListener("scroll", update, true);
    window.addEventListener("resize", update);
    const observer = new ResizeObserver(update);
    observer.observe(target);
    observer.observe(tooltip.current);
    return () => {
      window.removeEventListener("scroll", update, true);
      window.removeEventListener("resize", update);
      observer.disconnect();
    };
  }, [visible, target, side, content, close, placement]);

  if (!content || !isValidElement(children)) return children;

  const open = (el: HTMLElement) => {
    clearTimeout(timer.current);
    if (truncated && !isTruncated(el)) return;
    if (truncated) setFont(getComputedStyle(el).fontFamily || undefined);
    if (activeTooltip?.owner !== owner.current) activeTooltip?.close();
    activeTooltip = { owner: owner.current, close };
    setTarget(el);
    setPosition(null);
    timer.current = window.setTimeout(() => setVisible(true), delay);
  };
  const original = children.props as Record<string, unknown>;
  const child = cloneElement(children, {
    onMouseEnter: (e: React.MouseEvent<HTMLElement>) => { (original.onMouseEnter as ((e: React.MouseEvent<HTMLElement>) => void) | undefined)?.(e); open(e.currentTarget); },
    onMouseLeave: (e: React.MouseEvent<HTMLElement>) => { (original.onMouseLeave as ((e: React.MouseEvent<HTMLElement>) => void) | undefined)?.(e); close(); },
    onFocus: (e: React.FocusEvent<HTMLElement>) => { (original.onFocus as ((e: React.FocusEvent<HTMLElement>) => void) | undefined)?.(e); open(e.currentTarget); },
    // Focus moving between the target's own children (segmented keys, buttons) is not "leaving": keep the tooltip.
    onBlur: (e: React.FocusEvent<HTMLElement>) => { (original.onBlur as ((e: React.FocusEvent<HTMLElement>) => void) | undefined)?.(e); if (e.relatedTarget instanceof Node && e.currentTarget.contains(e.relatedTarget)) return; close(); },
  } as never);

  return <>{child}{visible && target && createPortal(
    <div ref={tooltip} className={"tooltip" + (placement === "start" ? " start" : "") + (truncated ? " full" : "")} role="tooltip" style={{ left: position?.left ?? 0, top: position?.top ?? 0, visibility: position ? "visible" : "hidden", fontFamily: truncated ? font : undefined }}>{content}</div>,
    document.body,
  )}</>;
}
