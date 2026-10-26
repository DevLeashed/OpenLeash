// @vitest-environment jsdom
// Loading older history threw the reader off the message they were reading.
//
// The transcript is pinned to the bottom while the reader is at the live end,
// and it anchors on its own rows while they are reading further up. A page of
// older messages arriving above them moves their row down, and the view has to
// move down with it — by exactly as much as their row moved.
//
// It used to be corrected by how much the *content* grew, and those are two
// different numbers. The rows above the reader have never been on screen, so
// the browser counts each at its `content-visibility` estimate, and the
// transcript also grows *below* the reader (the agent streaming while the page
// was in flight). Correcting by the content's growth moved the reader by the
// sum of both, and the row anchoring then pulled them back again: the two
// fought over the view, and the reader ended up somewhere else entirely —
// sometimes at the very top of the transcript, reading a different message.
import { afterEach, expect, it, vi } from "vitest";
import { api, type Item, type TaskSummary } from "./api";
import { get, loadPreviousHistory, openTask, set, subscribe } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
afterEach(() => { vi.restoreAllMocks(); });

const item = (id: string): Item => ({ id, kind: "text", text: id, data: {}, ts: "2026-10-16T10:00:00Z" });
const summary = (id: string) => ({ id, title: "Chat", status: "done" } as unknown as TaskSummary);

/** The height the browser counts for a row it has never rendered:
 *  `content-visibility: auto` gives every such row the same estimate, so the
 *  height of a transcript and the height of the rows in it are two different
 *  numbers. */
const ESTIMATE = 72;

/**
 * A transcript, as the browser sees it.
 *
 * jsdom does no layout, so the positions are computed here, with the rule
 * `content-visibility: auto` really uses: a row that has not been on screen
 * is counted at its estimate, and once it has been, it is counted at its real
 * height and keeps it. The rows come from the store, so a page of older
 * history landing above the reader prepends itself the way it does in the app.
 */
function transcript(id: string, opts: { top: number; view?: number }) {
  const view = opts.view ?? 600;
  /** The real height of each row, and whether it has been on screen. */
  const rows = new Map<string, { height: number; rendered: boolean }>();
  const box = document.createElement("div");
  document.body.appendChild(box);
  const els = new Map<string, HTMLElement>();

  const ids = () => (get().items[id] ?? []).map((i) => i.id);
  const height = (rowId: string) => {
    const r = rows.get(rowId);
    return r?.rendered ? r.height : ESTIMATE;
  };
  /** Where a row starts in the content: everything above it. */
  const contentTop = (rowId: string) => {
    let y = 0;
    for (const x of ids()) { if (x === rowId) break; y += height(x); }
    return y;
  };

  let top = opts.top;
  Object.defineProperty(box, "scrollHeight", { get: () => ids().reduce((n, x) => n + height(x), 0), configurable: true });
  Object.defineProperty(box, "clientHeight", { get: () => view, configurable: true });
  Object.defineProperty(box, "scrollTop", { get: () => top, set: (v: number) => { top = v; }, configurable: true });
  box.getBoundingClientRect = () => ({ top: 0, bottom: view, left: 0, right: 800, width: 800, height: view, x: 0, y: 0, toJSON: () => ({}) });

  /** Put the rows in the DOM, the way a re-render does, and let the rows that
   *  are on screen trade their estimate for their real height — which is what
   *  the browser does as they come into view. */
  const paint = () => {
    for (const rowId of ids()) {
      if (!rows.has(rowId)) rows.set(rowId, { height: 100, rendered: false });
      let el = els.get(rowId);
      if (!el) { el = document.createElement("div"); el.dataset.item = rowId; box.appendChild(el); els.set(rowId, el); }
      el.getBoundingClientRect = () => {
        const y = contentTop(rowId) - top;
        const h = height(rowId);
        return { top: y, bottom: y + h, left: 0, right: 800, width: 800, height: h, x: 0, y, toJSON: () => ({}) };
      };
    }
    for (const [rowId, el] of els) {
      if (!ids().includes(rowId)) { el.remove(); els.delete(rowId); rows.delete(rowId); }
    }
    // The rows in the view have been rendered: they are counted at their real
    // height from now on.
    for (const rowId of ids()) {
      const r = rows.get(rowId)!;
      if (r.rendered) continue;
      const b = els.get(rowId)!.getBoundingClientRect();
      if (b.bottom > 0 && b.top < view) r.rendered = true;
    }
  };
  paint();

  const stop = subscribe(() => paint());

  return {
    box,
    /** The row the reader is looking at, and where it sits in the view: the
     *  first row that reaches into the view, the way the transcript finds it. */
    reader: () => {
      for (const rowId of ids()) {
        const b = els.get(rowId)!.getBoundingClientRect();
        if (b.bottom <= 0) continue;
        return { id: rowId, top: b.top };
      }
      return null;
    },
    /** The reader scrolls: the view moves, and then the event lands. */
    scrollTo: (v: number) => { top = v; paint(); },
    /** A row's real height: it has been on screen and been measured. */
    setHeight: (rowId: string, h: number) => {
      const r = rows.get(rowId);
      if (r) { r.height = h; r.rendered = true; }
      paint();
    },
    /** A row's real height, which the browser does not know yet: it is still
     *  counted at the estimate, and trades it for the real height when it
     *  comes into view. */
    setUnrenderedHeight: (rowId: string, h: number) => {
      const r = rows.get(rowId);
      if (r) r.height = h;
      paint();
    },
    stop,
  };
}

/** A transcript of `n` rows, tall enough to scroll. */
const rows = (n: number): Item[] => Array.from({ length: n }, (_, i) => item(`row-${i}`));

async function openWithHistory(id: string, items: Item[], older: Item[] = [item("old-1"), item("old-2")]) {
  vi.spyOn(api, "taskOpen").mockResolvedValue({ summary: summary(id), items, bg: [], cursor: "older" } as never);
  vi.spyOn(api, "taskHistory").mockResolvedValue({ items: older, cursor: null } as never);
  vi.spyOn(api, "draftGet").mockResolvedValue(null as never);
  set({ task: id });
  await openTask(id);
}
const frame = () => new Promise((resolve) => requestAnimationFrame(() => resolve(null)));

it("keeps a reader at the live end when older history arrives, even when the transcript fit on screen", async () => {
  await openWithHistory("short-chat", [item("new")]);
  // The feed fits (a finished turn folded its work away): top is the bottom.
  const box = transcript("short-chat", { top: 0 });
  const load = loadPreviousHistory("short-chat", box.box as unknown as HTMLElement);
  await load; await frame();
  // Before the fix this kept the distance from the top and the chat showed
  // older messages instead of the live end.
  expect(box.box.scrollTop).toBe(box.box.scrollHeight);
  box.stop();
});

it("does not yank a reader back to where they were when they asked for older messages", async () => {
  await openWithHistory("moved-chat", rows(20));
  const box = transcript("moved-chat", { top: 400 });
  const load = loadPreviousHistory("moved-chat", box.box as unknown as HTMLElement);
  // The reader keeps scrolling while the page is in flight, up to the top.
  box.scrollTo(0);
  await load; await frame();
  // The row they ended up on is where they are now, not where they were when
  // they asked: the old code (holding the offset from before the request)
  // jumped them back down the transcript.
  expect(box.reader()?.id).toBe("row-0");
  box.stop();
});

it("keeps the place of a reader who scrolled up to read older messages", async () => {
  await openWithHistory("long-chat", rows(20));
  const box = transcript("long-chat", { top: 400 });
  const load = loadPreviousHistory("long-chat", box.box as unknown as HTMLElement);
  await load; await frame();
  // The reader's row is where it was: the page landed above them, and the
  // view moved down with it.
  const reader = box.reader();
  expect(reader?.id).toBe("row-5");
  expect(reader?.top).toBe(-40);
  box.stop();
});

it("keeps the reader on their message when the transcript grows below them too", async () => {
  await openWithHistory("busy-chat", rows(20));
  const box = transcript("busy-chat", { top: 400 });
  // The agent keeps working while the page is in flight: the last row grows,
  // so the transcript is taller by more than the page adds above the reader.
  vi.spyOn(api, "taskHistory").mockImplementation(async () => {
    box.setHeight("row-19", 300);
    return { items: [item("old-1"), item("old-2")], cursor: null } as never;
  });
  const before = box.reader();
  const load = loadPreviousHistory("busy-chat", box.box as unknown as HTMLElement);
  await load; await frame();
  const after = box.reader();
  // The same row, at the same place in the view. Correcting by how much the
  // content grew — which counts the row *below* the reader as well — moved
  // them off it by the height of that row.
  expect(after?.id).toBe(before?.id);
  expect(after?.top).toBe(before?.top);
  box.stop();
});

it("keeps the reader on their message when the rows above them have never been rendered", async () => {
  await openWithHistory("lazy-chat", rows(20));
  const box = transcript("lazy-chat", { top: 400 });
  // The rows above the reader have never been on screen, so the browser counts
  // each at its estimate — and each of them is really taller than that. The
  // reader scrolls up while the page is in flight, and the rows they scroll
  // into are rendered for the first time: the content grows by more than the
  // page added, and none of it is the reader's row moving.
  //
  // This is the teleport. Correcting by how much the content grew moved the
  // reader by the sum of all of it — the page, and the rows re-rendering —
  // instead of by the one thing that matters, which is how far their own row
  // moved. In a long transcript that threw them off the message entirely,
  // sometimes to the very top.
  for (const row of ["row-0", "row-1", "row-2", "row-3"]) box.setUnrenderedHeight(row, 240);
  const load = loadPreviousHistory("lazy-chat", box.box as unknown as HTMLElement);
  // The reader keeps scrolling up while the page is in flight.
  box.scrollTo(0);
  await load; await frame();
  const after = box.reader();
  // They are at the top now, and the rows that are on screen are the ones
  // that were there before the page landed: the reader's own row, and the
  // rows above it, moved down by exactly what the page added, and the view
  // moved with them. Correcting by the content's growth moved them further,
  // by the height of the rows that re-rendered on the way.
  expect(after?.id).toBe("row-0");
  expect(after?.top).toBe(0);
  box.stop();
});
