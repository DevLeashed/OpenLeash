// @vitest-environment jsdom
// Leaving a chat and coming back threw the view up the transcript, even while
// the agent kept working in it.
//
// The pane is not remounted between chats, and as a chat opens its content
// grows under the view: rows trade their size estimate for their real height,
// the newest page lands, output streams in. A scroll event that arrived after
// such growth but before the next pin read as "far from the end", so the chat
// stopped following and stayed wherever the growth left it. Now growth under a
// view that has not moved keeps following, and each chat keeps its own place.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import { get, set } from "../store";
import { Timeline } from "./Session";
import type { Item, TaskSummary } from "../api";

const chat = (id: string) => ({
  id, title: id, status: "running", model: "anthropic/claude-opus-5", branch: "main",
  subs: [], usage: {}, todos: [], pending: {},
}) as unknown as TaskSummary;
const a = chat("a");
const b = chat("b");
// Fresh chats for the test that leaves and returns: where a chat was left is
// remembered for the app's lifetime, which is the point of it.
const c = chat("c");
const d = chat("d");

const rows = (prefix: string, n: number): Item[] =>
  Array.from({ length: n }, (_, i) => ({ id: `${prefix}${i}`, kind: "text", text: `reply ${i}`, ts: "2026-09-01T00:00:00Z" }) as Item);

const timeline = () => document.querySelector<HTMLElement>(".timeline")!;

/** jsdom does no layout. Rows are 100px apart in transcript order, the view is
 *  600px tall, and `scrollTop` is clamped the way a browser clamps it. */
let extra = 0;
const VIEW = 600;
function layout(el: HTMLElement) {
  let top = 0;
  const height = () => el.querySelectorAll("[data-item]").length * 100 + extra;
  const clamp = (v: number) => Math.max(0, Math.min(v, height() - VIEW));
  Object.defineProperty(el, "scrollHeight", { get: height, configurable: true });
  Object.defineProperty(el, "clientHeight", { get: () => VIEW, configurable: true });
  Object.defineProperty(el, "scrollTop", { get: () => top, set: (v: number) => { top = clamp(v); }, configurable: true });
  el.getBoundingClientRect = () => ({ top: 0, bottom: VIEW, left: 0, right: 800, width: 800, height: VIEW, x: 0, y: 0, toJSON: () => ({}) });
}
beforeEach(() => {
  extra = 0;
  HTMLElement.prototype.getBoundingClientRect = function (this: HTMLElement) {
    const list = [...timeline().querySelectorAll<HTMLElement>("[data-item]")];
    const y = list.indexOf(this) * 100 - timeline().scrollTop;
    return { top: y, bottom: y + 100, left: 0, right: 800, width: 800, height: 100, x: 0, y, toJSON: () => ({}) };
  };
});
afterEach(() => cleanup());

function openChats(items: Record<string, Item[]>, current: string) {
  set({ view: "session" as const, task: current, tasks: { a, b, c, d }, items, settings: { show_thinking: false } as never });
}

describe("leaving and entering a chat keeps the view where it belongs", () => {
  it("keeps following when the content grows under the view before it is pinned again", async () => {
    openChats({ a: rows("a", 20) }, "a");
    render(<Timeline task={a} />);
    const el = timeline();
    layout(el);
    await act(async () => { set({ items: { a: rows("a", 21) } }); });
    expect(el.scrollTop).toBe(1500);
    // Rows near the end grew to their real height; the scroll event from the
    // last pin lands before anything pins again.
    extra = 900;
    fireEvent.scroll(el);
    expect(el.scrollTop, "growth under a follower is not the reader leaving").toBe(2400);
    await act(async () => { set({ items: { a: rows("a", 22) } }); });
    expect(el.scrollTop).toBe(2500);
  });

  it("opens a chat at its end even when the previous chat was scrolled up", async () => {
    openChats({ a: rows("a", 30), b: rows("b", 30) }, "a");
    const view = render(<Timeline task={a} />);
    const el = timeline();
    layout(el);
    await act(async () => { set({ items: { ...get().items, a: rows("a", 31) } }); });
    fireEvent.wheel(el, { deltaY: -120 });
    el.scrollTop = 1000;
    fireEvent.scroll(el);
    await act(async () => { set({ task: "b" }); view.rerender(<Timeline task={b} />); });
    expect(el.scrollTop).toBe(2400);
    // The agent is still working in b: it keeps following.
    await act(async () => { set({ items: { ...get().items, b: rows("b", 31) } }); });
    expect(el.scrollTop).toBe(2500);
  });

  it("comes back to the end of a chat that was following, and to the reader's place in one that was not", async () => {
    openChats({ c: rows("c", 30), d: rows("d", 30) }, "c");
    const view = render(<Timeline task={c} />);
    const el = timeline();
    layout(el);
    await act(async () => { set({ items: { ...get().items, c: rows("c", 31) } }); });
    // Reading d from row 12.
    await act(async () => { set({ task: "d" }); view.rerender(<Timeline task={d} />); });
    fireEvent.wheel(el, { deltaY: -120 });
    el.scrollTop = 1200;
    fireEvent.scroll(el);
    // Back to c, which kept working while it was closed.
    await act(async () => { set({ task: "c", items: { ...get().items, c: rows("c", 35) } }); view.rerender(<Timeline task={c} />); });
    expect(el.scrollTop, "c was following: back at its new end").toBe(2900);
    // And to d: the reader's row is back at the top of the view.
    await act(async () => { set({ task: "d" }); view.rerender(<Timeline task={d} />); });
    expect(el.scrollTop).toBe(1200);
    await act(async () => { set({ items: { ...get().items, d: rows("d", 31) } }); });
    expect(el.scrollTop, "a reader who was up the transcript is not pulled down").toBe(1200);
  });
});
