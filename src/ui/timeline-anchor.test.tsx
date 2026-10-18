// @vitest-environment jsdom
// Rows change height above the reader: a finished turn folds its work into one
// "Worked" row, a finished subagent card collapses, an off-screen row trades its
// content-visibility estimate for its real height. Each of those used to move
// whatever the reader was looking at. The timeline anchors by hand: it notes
// where the rows at the top of the view sit and, when the content resizes,
// scrolls by however far the first surviving one moved.
//
// jsdom has no layout, so row positions are stated, and the ResizeObserver is
// captured so the test decides when "the layout changed".
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import { set } from "../store";
import { Timeline } from "./Session";
import type { Item, TaskSummary } from "../api";

const task = { id: "t1", title: "chat", status: "done", model: "anthropic/claude-opus-5", branch: "main", subs: [], usage: {}, todos: [], pending: {} } as unknown as TaskSummary;
const reply = (id: string): Item => ({ id, kind: "text", text: `reply ${id}`, data: {}, ts: "2026-09-01T00:00:00Z" }) as Item;

let resize: () => void = () => {};
beforeEach(() => {
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
    constructor(callback: () => void) { resize = callback; }
    observe() {} disconnect() {} unobserve() {}
  };
});
afterEach(() => cleanup());

/** A scroll box and rows with stated positions (viewport-relative tops). */
function layout(tops: Record<string, number>, scroll: { height: number; top: number; view: number }) {
  const box = document.querySelector<HTMLElement>(".timeline")!;
  box.getBoundingClientRect = () => ({ top: 0, bottom: scroll.view, left: 0, right: 800, width: 800, height: scroll.view, x: 0, y: 0, toJSON: () => ({}) });
  Object.defineProperty(box, "scrollHeight", { get: () => scroll.height, configurable: true });
  Object.defineProperty(box, "clientHeight", { get: () => scroll.view, configurable: true });
  box.scrollTop = scroll.top;
  for (const row of box.querySelectorAll<HTMLElement>("[data-item]")) {
    row.getBoundingClientRect = () => {
      const top = tops[row.dataset.item!] ?? 9999;
      return { top, bottom: top + 100, left: 0, right: 800, width: 800, height: 100, x: 0, y: top, toJSON: () => ({}) };
    };
  }
  return box;
}

it("keeps the row a reader is on in place when content above it shrinks", async () => {
  set({ view: "session" as const, task: "t1", tasks: { t1: task }, items: { t1: ["a", "b", "c", "d"].map(reply) }, settings: { show_thinking: false } as never });
  render(<Timeline task={task} />);
  const tops: Record<string, number> = { a: -250, b: -150, c: 40, d: 140 };
  const box = layout(tops, { height: 3000, top: 1200, view: 600 });
  // The reader scrolled up: not stuck to the bottom, anchors recorded.
  fireEvent.scroll(box);
  // A finished turn folded 300px of work above: every surviving row moved up.
  tops.c = -260; tops.d = -160;
  await act(async () => { resize(); });
  expect(box.scrollTop).toBe(900);
});

it("keeps a reader at the bottom at the bottom when the content changes size", async () => {
  set({ view: "session" as const, task: "t1", tasks: { t1: task }, items: { t1: ["a", "b"].map(reply) }, settings: { show_thinking: false } as never });
  render(<Timeline task={task} />);
  const scroll = { height: 2000, top: 1400, view: 600 };
  const box = layout({ a: 0, b: 300 }, scroll);
  fireEvent.scroll(box);
  scroll.height = 1500;
  await act(async () => { resize(); });
  expect(box.scrollTop).toBe(1500);
});
