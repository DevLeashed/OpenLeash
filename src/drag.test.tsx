// @vitest-environment jsdom
/// <reference types="node" />
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { useState } from "react";

// The reorder lists were rebuilt on pointer events because the window's native
// file-drop handler has to stay installed (real paths, see ui/drag.ts). These
// pin the behaviour that swap had to preserve: a click still opens the row, a
// drag still commits against the row under the pointer, and Escape abandons.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { useRowDrag } from "./ui/drag";
import { get, set } from "./store";
import { overComposer } from "./ui/Composer";

afterEach(cleanup);

/**
 * jsdom implements neither pointer capture nor hit testing: `setPointerCapture`
 * is missing entirely, and a click always fires on the element it was fired on.
 * Both gaps have to be stood in for, because the bug this pins is exactly the
 * gap between them.
 *
 * `captured` records which elements hold capture, and `clickLike` applies the
 * one browser rule that matters — Pointer Events 3 §4.2.12.3: "if `userEvent`
 * was dispatched while the corresponding pointer was captured, then let
 * `target` be the target of `userEvent`". A click under a captured pointer is
 * dispatched to the *capturing element*, not to the control under it. Without
 * this shim a row's child button looks perfectly clickable in the test and is
 * dead in the app.
 */
const captured = new WeakSet<Element>();

beforeAll(() => {
  Element.prototype.setPointerCapture = function (this: Element) { captured.add(this); };
  Element.prototype.releasePointerCapture = function (this: Element) { captured.delete(this); };
  Element.prototype.hasPointerCapture = function (this: Element) { return captured.has(this); };
});

/** A press and release that never moved, then the click that follows — a real
 *  click on whatever is under the pointer, retargeted if the row took capture.
 *
 *  The press goes to `el`, not to the row: that is the whole point of the two
 *  tests below. A press landing on the row gives `useRowDrag` `target ===
 *  currentTarget`, so it takes capture and the click is retargeted to the row.
 *  A press landing on a control *inside* the row does not, and the control keeps
 *  its click. Dispatching both on the row would test the same path twice and
 *  pass for the wrong reason. */
function clickLike(el: Element) {
  const row = el.closest("[data-dragrow]")!;
  press(el, 5);
  fireEvent.pointerUp(window, { pointerId: 1, clientY: 5 });
  fireEvent.click(captured.has(row) ? row : el);
}

/** The drag from a press on the row's own text rather than on its edge — the
 *  queue row's message, where most of a drag actually starts. */
function dragFrom(el: Element, toY: number) {
  press(el, 5);
  fireEvent.pointerMove(window, { pointerId: 1, clientY: toY });
  fireEvent.pointerUp(window, { pointerId: 1, clientY: toY });
}

/** Three rows at known heights. jsdom gives every element a zero rect, so the
 *  geometry is stubbed — the hit test reads real `getBoundingClientRect`s.
 *  Each row carries a child button and a child span, the way a chat row carries
 *  its pin and archive buttons and a queue row its message text. */
function List({ onDrop, log }: { onDrop: (a: string, b: string) => void; log: string[] }) {
  const drag = useRowDrag(onDrop);
  const [open, setOpen] = useState<string | null>(null);
  const ids = ["a", "b", "c"];
  return (
    <div ref={drag.listRef}>
      {ids.map((id, i) => (
        <div key={id} data-dragrow={id} data-dragidx={i}
          onPointerDown={drag.press(id)}
          onClick={drag.click(() => { log.push(`click ${id}`); setOpen(id); })}
          className={drag.dragging === id ? "dragging" : drag.over === id ? "over" : ""}>
          <span className="qtext">{open === id ? `opened ${id}` : id}</span>
          <button type="button" className="rowact" onClick={(e) => { e.stopPropagation(); log.push(`act ${id}`); }} />
        </div>
      ))}
    </div>
  );
}

/** Lay the rows out as three 20px bands starting at y=0. */
function stubRects(container: HTMLElement) {
  const rows = Array.from(container.querySelectorAll<HTMLElement>("[data-dragrow]"));
  rows.forEach((el, i) => {
    el.getBoundingClientRect = () => ({ top: i * 20, bottom: i * 20 + 20, left: 0, right: 100, width: 100, height: 20, x: 0, y: i * 20, toJSON: () => "" }) as DOMRect;
  });
}

const press = (el: Element, y = 0) => fireEvent.pointerDown(el, { button: 0, pointerId: 1, clientY: y });

describe("reordering rows without HTML5 drag and drop", () => {
  it("commits against the row the pointer is over", () => {
    const log: string[] = [];
    const drops: string[][] = [];
    const { container } = render(<List onDrop={(a, b) => drops.push([a, b])} log={log} />);
    stubRects(container);
    const a = container.querySelector("[data-dragrow]")!;

    dragFrom(a, 45);

    // Row "a" (band 0) dropped on row "c" (band 2).
    expect(drops).toEqual([["a", "c"]]);
    expect(log, "the drag must not also open the chat").toEqual([]);
  });

  // The fix that un-deadens the row buttons must not cost the drag its capture:
  // a drag normally begins on the row's text, not on its edge.
  it("reorders from a press on the row's text, not just its edge", () => {
    const drops: string[][] = [];
    const { container } = render(<List onDrop={(a, b) => drops.push([a, b])} log={[]} />);
    stubRects(container);

    dragFrom(container.querySelector(".qtext")!, 45);

    expect(drops).toEqual([["a", "c"]]);
  });

  it("still opens the row on an ordinary click", () => {
    const log: string[] = [];
    const drops: string[][] = [];
    const { container } = render(<List onDrop={(a, b) => drops.push([a, b])} log={log} />);
    stubRects(container);
    const a = container.querySelector("[data-dragrow]")!;

    press(a, 5);
    fireEvent.pointerUp(window, { pointerId: 1, clientY: 5 });
    fireEvent.click(a);

    // This is the reason the press needs slop: a row is also the click target
    // that opens the chat, and a chat that can't be opened by clicking it is
    // worse than one you can't reorder.
    expect(log).toEqual(["click a"]);
    expect(drops).toEqual([]);
  });

  // This is the sidebar's pin and archive buttons. The row takes capture on
  // pointerdown to keep a drag alive once the pointer outruns it, and capture
  // retargets the click to the capturing element — so the click landed on the
  // row instead of the button, and every control inside a row was silently
  // dead. `onPointerDown` only gets the row's handler by bubbling, so the
  // capture is taken even when the press started on a child.
  it("clicks a button inside the row instead of the row itself", () => {
    const log: string[] = [];
    const { container } = render(<List onDrop={() => {}} log={log} />);
    stubRects(container);

    clickLike(container.querySelector(".rowact")!);

    expect(log, "the button must run, and the row must not open behind it").toEqual(["act a"]);
  });

  // The mirror of the above, and the reason the fix can't just be "never take
  // capture": the row is itself the click target, so the click still has to
  // open the chat it landed on.
  it("still opens the row when the press started on the row", () => {
    const log: string[] = [];
    const { container } = render(<List onDrop={() => {}} log={log} />);
    stubRects(container);

    clickLike(container.querySelector("[data-dragrow]")!);

    expect(log).toEqual(["click a"]);
  });

  it("does not reorder when the pointer never moved", () => {
    const drops: string[][] = [];
    const { container } = render(<List onDrop={(a, b) => drops.push([a, b])} log={[]} />);
    stubRects(container);
    const a = container.querySelector("[data-dragrow]")!;
    press(a, 5);
    fireEvent.pointerUp(window, { pointerId: 1, clientY: 22 });
    expect(drops, "a one-pixel wobble is a click, not a drag").toEqual([]);
  });

  it("abandons the drag on Escape without committing", () => {
    const drops: string[][] = [];
    const { container } = render(<List onDrop={(a, b) => drops.push([a, b])} log={[]} />);
    stubRects(container);
    const a = container.querySelector("[data-dragrow]")!;
    press(a, 5);
    fireEvent.pointerMove(window, { pointerId: 1, clientY: 45 });
    act(() => { fireEvent.keyDown(window, { key: "Escape" }); });
    expect(drops).toEqual([]);
    // And the row is not left stuck in its dragging state.
    expect(container.querySelector(".dragging")).toBeNull();
  });

  it("ignores a right-click, which opens the context menu", () => {
    const drops: string[][] = [];
    const { container } = render(<List onDrop={(a, b) => drops.push([a, b])} log={[]} />);
    stubRects(container);
    const a = container.querySelector("[data-dragrow]")!;
    fireEvent.pointerDown(a, { button: 2, pointerId: 1, clientY: 5 });
    fireEvent.pointerMove(window, { pointerId: 1, clientY: 45 });
    fireEvent.pointerUp(window, { pointerId: 1, clientY: 45 });
    expect(drops).toEqual([]);
  });

  it("clears the drag state when the window loses focus mid-drag", () => {
    const { container } = render(<List onDrop={() => {}} log={[]} />);
    stubRects(container);
    const a = container.querySelector("[data-dragrow]")!;
    press(a, 5);
    fireEvent.pointerMove(window, { pointerId: 1, clientY: 45 });
    expect(container.querySelector(".dragging")).not.toBeNull();
    // alt-tab delivers no pointerup; without this the row stays faded forever.
    act(() => { fireEvent.blur(window); });
    expect(container.querySelector(".dragging")).toBeNull();
  });

  it("marks the row the pointer is over while dragging", () => {
    const { container } = render(<List onDrop={() => {}} log={[]} />);
    stubRects(container);
    const a = container.querySelector("[data-dragrow]")!;
    press(a, 5);
    fireEvent.pointerMove(window, { pointerId: 1, clientY: 25 });
    expect(container.querySelector("[data-dragrow='b']")!.className).toContain("over");
  });
});

// The composer's hit test against the window-level drop event. It divides by the
// DPR and by the app zoom; the zoom half used to be missing, which made drops
// land on nothing at any zoom but 100%.
describe("dropping a file onto the composer", () => {
  afterEach(() => {
    set({ settings: null });
    document.body.innerHTML = "";
  });

  const at = (x: number, y: number) => ({ payload: { type: "drop", paths: ["C:/a.png"], position: { x, y } } }) as unknown as { payload: import("@tauri-apps/api/window").DragDropEvent };

  function composerRect() {
    const el = document.createElement("div");
    el.className = "composer";
    // Reported in pre-zoom CSS pixels, which is what getBoundingClientRect does
    // for an element inside a CSS `zoom`ed subtree.
    el.getBoundingClientRect = () => ({ left: 100, right: 400, top: 300, bottom: 500, width: 300, height: 200, x: 100, y: 300, toJSON: () => "" }) as DOMRect;
    document.body.appendChild(el);
  }

  it("hit-tests the reported position against the box", () => {
    set({ settings: { ui_zoom: 100 } as never });
    composerRect();
    // Inside at 100%: x 250, y 400.
    expect(overComposer(at(250, 400))).toBe(true);
    expect(overComposer(at(50, 400))).toBe(false);
    expect(overComposer(at(250, 200))).toBe(false);
  });

  it("scales the hit test with the app zoom", () => {
    // At 200% zoom the box occupies twice the reported CSS pixels, so a point
    // that is inside the box must be at twice the unzoomed coordinates.
    set({ settings: { ui_zoom: 200 } as never });
    composerRect();
    expect(overComposer(at(500, 800))).toBe(true);
    // ...and the unzoomed coordinate that used to land inside is now outside.
    expect(overComposer(at(250, 400))).toBe(false);
  });

  it("leaves a zoom of 100% alone", () => {
    set({ settings: null });
    composerRect();
    expect(overComposer(at(250, 400))).toBe(true);
    expect(get().settings?.ui_zoom).toBeUndefined();
  });

  it("reports nothing for the events that carry no position", () => {
    composerRect();
    expect(overComposer({ payload: { type: "leave" } } as never)).toBe(false);
  });
});