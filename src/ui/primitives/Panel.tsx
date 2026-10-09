// A panel anchored under a control, portalled into the app root so it lands
// above the panes, the Details column and the timeline (those establish their
// own stacking contexts, so a popup rendered inside them paints behind text —
// and a wheel over such a popup is handed to the scroll container underneath
// instead of the list, which is why the task list never scrolled).
import { ReactNode, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { zoom } from "../../store";
import { MenuScrim } from "./Controls";
import { useEscapeClose } from "./escape";

interface Pos { top: number; left: number; up: boolean; maxHeight: number }

export function AnchoredPanel({ anchor, onClose, width, children }: { anchor: DOMRect | null; onClose: () => void; width: number; children: ReactNode }) {
  const [pos, setPos] = useState<Pos | null>(null);
  useLayoutEffect(() => {
    if (!anchor) return setPos(null);
    // Coordinates are in unzoomed CSS pixels; the app root is zoomed.
    const z = zoom();
    const vw = window.innerWidth / z, vh = window.innerHeight / z;
    const below = vh - anchor.bottom / z;
    // Flip above the control when there isn't room below — these panels hold
    // long lists, and one hanging off the bottom of the window is useless.
    const up = below < 280 && anchor.top / z > below;
    const top = up ? anchor.top / z - 6 : anchor.bottom / z + 6;
    setPos({ top, left: Math.max(12, Math.min(anchor.right / z - width, vw - width - 12)), up, maxHeight: (up ? top : vh - top) - 16 });
  }, [anchor, width]);
  // Read after the early-out: this runs during the server render the tests use,
  // where there is no document to ask and no panel to place anyway.
  if (!anchor || !pos) return null;
  const root = document.querySelector<HTMLElement>(".app");
  if (!root) return null;
  const at = pos.up
    ? { left: pos.left, width, bottom: `calc(100% - ${pos.top}px)` }
    : { top: pos.top, left: pos.left, width };
  return createPortal(
    <>
      {/* Over the chat so a press anywhere else lands here, under the panel. */}
      <MenuScrim onClick={onClose} style={{ zIndex: 70 }} />
      <div className="statuspop" style={{ ...at, maxHeight: pos.maxHeight }}>{children}</div>
    </>,
    root,
  );
}

/**
 * Opens a panel against a control and dismisses it the way Status does: a
 * scrim for outside presses plus Escape. `onOpen` fires once per open, after
 * the anchor is measured, for panels that fetch on open.
 */
export function useAnchoredPanel(onOpen?: () => void) {
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  const opened = useRef(false);
  const close = useCallback(() => setAnchor(null), []);
  // A closed panel is a fresh open, so the one-shot `onOpen` below has to be
  // re-armed every time the anchor clears.
  useEffect(() => { if (!anchor) opened.current = false; }, [anchor]);
  useEscapeClose(!!anchor, close);
  useEffect(() => {
    if (anchor && !opened.current) { opened.current = true; onOpen?.(); }
  }, [anchor, onOpen]);
  return {
    anchor,
    open: !!anchor,
    toggle: (e: React.MouseEvent) => (anchor ? close() : setAnchor((e.currentTarget as HTMLElement).getBoundingClientRect())),
    close,
  };
}
