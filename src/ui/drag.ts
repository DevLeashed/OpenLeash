import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent as RPointerEvent, MouseEvent as RMouseEvent } from "react";

/**
 * Reordering a list of rows, without HTML5 drag and drop.
 *
 * Why not `draggable`: the window's native file-drop handler has to stay
 * installed, because it is the only thing that can hand the backend a real path
 * for a file dropped from Explorer — the browser's `DataTransfer` carries the
 * bytes and never the path, and there is no Tauri 2 bridge that recovers it.
 * On Windows that handler is an OLE drop target registered over the webview,
 * and it consumes *every* drag before the page sees it: `dragstart` fires but
 * `dragover`/`drop` never do, and the cursor shows "not allowed". The two
 * mechanisms are exclusive on this platform, which is why the window config
 * can't simply have both.
 *
 * So the reorder is built from pointer events, which the OLE handler leaves
 * alone, and keeps the same shape the HTML5 handlers had: begin, hover a target
 * row, commit against it.
 */

/** How far the pointer has to travel before a press counts as a drag. A chat row
 *  is also the click target that opens it, so the slop is what keeps an
 *  ordinary click from turning into a drag. */
const SLOP = 5;

/**
 * What counts as a control that wants the click for itself: anything that is
 * natively activatable, plus the row's own text fields. A press on one of these
 * must not take pointer capture (see `press`), or the click would be retargeted
 * onto the row and the control would go dead.
 *
 * Deliberately narrow — every extra selector here is a drag surface that has to
 * be started by hand, because a drag that loses capture partway through stops
 * receiving its moves.
 */
const CONTROL = "button, a, input, textarea, select, [role='button'], .qedit, .rename";

/** What the press is currently doing. In a ref: these change on every
 *  pointermove and none of them should schedule a render on its own. */
type Live = { id: string; armed: boolean; y0: number; edges: Edge[] };

export interface RowDrag {
  /** Id of the row being dragged, or null. Drives the row's "dragging" class. */
  dragging: string | null;
  /** Id of the row the drop would land on, or null. */
  over: string | null;
  /** Goes on the element wrapping the rows — the hit test measures it. */
  listRef: (el: HTMLElement | null) => void;
  /** Spread onto each row: `onPointerDown={drag.press(id)}`. */
  press: (id: string) => (e: RPointerEvent) => void;
  /** Wrap a row's click. Returns without calling it for the click that ends a
   *  drag, which the browser still fires after a pointerup on the dragged row. */
  click: (fn: (e: RMouseEvent) => void) => (e: RMouseEvent) => void;
}

type Edge = { id: string; top: number; bottom: number };

/**
 * `onDrop` receives the dragged row's id and the id of the row to drop it
 * *onto* — which is what both lists already want ("put this one just above
 * that one"). The caller turns the pair into a position; the hook only knows
 * about rows. Passing the dragged id here rather than leaving the caller to
 * close over the hook's own state keeps the callback out of the render cycle
 * the hook depends on.
 */
export function useRowDrag(onDrop: (dragId: string, overId: string) => void): RowDrag {
  const [dragging, setDragging] = useState<string | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const list = useRef<HTMLElement | null>(null);
  const live = useRef<Live | null>(null);
  // Set when a press turned into a drag, and cleared by the click that follows.
  const swallowClick = useRef(false);
  // `onDrop` is a fresh closure on every render of the caller; reading it
  // through a ref keeps `press` stable, so the rows don't re-render on every
  // keystroke in the composer or every task event.
  const drop = useRef(onDrop);
  drop.current = onDrop;

  const listRef = useCallback((el: HTMLElement | null) => { list.current = el; }, []);

  /** Cache the rows once per drag. A `getBoundingClientRect` per row on every
   *  pointermove would force a layout per mouse move, and the rows don't move
   *  during a drag (the dragging class only changes opacity). */
  const measure = useCallback(() => {
    const root = list.current;
    return root
      ? Array.from(root.querySelectorAll<HTMLElement>("[data-dragrow]"))
        .map((el) => {
          const r = el.getBoundingClientRect();
          return { id: el.dataset.dragrow!, top: r.top, bottom: r.bottom };
        })
        // A collapsed row reports a zero rect and would swallow every position.
        .filter((e) => e.bottom > e.top)
      : [];
  }, []);

  /** The row under `y`: the first one whose bottom edge is below the pointer, so
   *  a pointer past the end of the list lands on the last row rather than on
   *  nothing. */
  const hit = (edges: Edge[], y: number) => edges.find((e) => y < e.bottom)?.id ?? null;

  const press = useCallback((id: string) => (e: RPointerEvent) => {
    // Primary button only: a right-click opens the chat's context menu and must
    // not also pick the row up.
    if (e.button !== 0) return;
    swallowClick.current = false;
    live.current = { id, armed: false, y0: e.clientY, edges: measure() };
    // Capture so a fast drag that outruns the row still delivers its moves, and
    // so the pointerup lands here instead of on whatever is under the pointer.
    // The window listeners below already cover both, so this is a safeguard
    // rather than the mechanism.
    //
    // Not when the press landed on something that wants the click for itself —
    // the chat row's pin and archive buttons, the queue row's send/trash/menu,
    // the rename field. Capture rewrites a click's target to the capturing
    // element (Pointer Events 3 §4.2.12.3: "if userEvent was dispatched while
    // the corresponding pointer was captured, then let target be the target of
    // userEvent"), so capturing the row aimed every one of those clicks at the
    // row instead of the control: the buttons did nothing at all, and the click
    // was spent opening the chat.
    //
    // The test has to be "is this interactive", not "is this the row": a drag
    // normally starts on whatever is under the pointer — the queue row's grip or
    // its message text, both children — and only a control has to keep the click.
    const owner = e.currentTarget;
    const pressed = e.target;
    const interactive = pressed instanceof Element && pressed.closest(CONTROL) !== null;
    if (!interactive) try { owner.setPointerCapture(e.pointerId); } catch { /* element gone */ }

    const move = (ev: PointerEvent) => {
      const l = live.current;
      if (!l) return;
      if (!l.armed) {
        if (Math.abs(ev.clientY - l.y0) < SLOP) return;
        l.armed = true;
        setDragging(id);
      }
      const target = hit(l.edges, ev.clientY);
      setOver((prev) => (target !== prev ? target : prev));
    };
    const finish = (ev: PointerEvent, commit: boolean) => {
      const l = live.current;
      live.current = null;
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("keydown", key, true);
      // Only what this press took: an interactive press took none, and so has
      // nothing to release. `ev.target` cannot answer this on its own — under
      // capture it names the row whatever was actually under the pointer.
      if (!interactive && owner.hasPointerCapture?.(ev.pointerId)) owner.releasePointerCapture(ev.pointerId);
      if (!l) return;
      swallowClick.current = l.armed;
      setDragging(null);
      setOver(null);
      const target = commit ? hit(l.edges, ev.clientY) : null;
      if (commit && l.armed && target && target !== l.id) drop.current(l.id, target);
    };
    const up = (ev: PointerEvent) => finish(ev, true);
    const cancel = (ev: PointerEvent) => finish(ev, false);
    const key = (ev: KeyboardEvent) => {
      if (ev.key !== "Escape") return;
      // Escape has to beat the global handler, which would otherwise treat it
      // as "pause this chat".
      ev.preventDefault();
      ev.stopPropagation();
      cancel(ev as unknown as PointerEvent);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("keydown", key, true);
  }, [measure]);

  // A window blur mid-drag (alt-tab, a modal the OS raised) delivers no
  // pointerup, which would otherwise leave the row stuck in its dragging state.
  useEffect(() => {
    const cancel = () => {
      if (!live.current) return;
      live.current = null;
      swallowClick.current = true;
      setDragging(null);
      setOver(null);
    };
    window.addEventListener("blur", cancel);
    return () => window.removeEventListener("blur", cancel);
  }, []);

  const click = useCallback((fn: (e: RMouseEvent) => void) => (e: RMouseEvent) => {
    if (swallowClick.current) { swallowClick.current = false; return; }
    fn(e);
  }, []);

  return { dragging, over, listRef, press, click };
}