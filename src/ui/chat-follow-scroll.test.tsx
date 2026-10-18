// @vitest-environment jsdom
// Scrolling up in a live chat shook the screen and kept the reader at the bottom.
//
// The transcript follows the live end while the reader is at it: every time the
// transcript grows, the view is pinned to the bottom. Whether the reader was
// following used to be recomputed on every scroll event as "within 80px of the
// bottom". The first frames of a mouse wheel's smooth scroll, and each small step
// of a touchpad, land inside those 80px, so the follow came straight back on, the
// next streamed token pinned the view down and cancelled the scroll, and the
// screen shook in place without ever getting away.
//
// Now only the reader turns the follow off, by moving up at all, and only
// reaching the end again turns it back on.
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

const task = {
  id: "t1", title: "chat", status: "running", model: "anthropic/claude-opus-5", branch: "main",
  subs: [], usage: {}, todos: [], pending: {}, ...({} as Record<string, never>),
} as unknown as TaskSummary;

const chatter = (n: number): Item =>
  ({ id: `t${n}`, kind: "text", text: "working", ts: "2026-09-01T00:00:00Z" }) as Item;

const open = (items: Item[]) => {
  set({
    view: "session" as const,
    task: "t1",
    tasks: { t1: task },
    items: { t1: items },
    settings: { show_thinking: false } as never,
  });
};

const grow = async (...extra: Item[]) => {
  await act(async () => { set({ items: { t1: get().items.t1!.concat(extra) } }); });
};

const timeline = () => document.querySelector<HTMLElement>(".timeline")!;

/** jsdom does no layout: give a scroller a content and view height, and a
 *  `scrollTop` clamped to them the way a browser clamps it. Returns a setter for
 *  the content height, for a transcript that grows or shrinks. */
const size = (el: HTMLElement, content: number, view: number) => {
  let top = 0;
  Object.defineProperty(el, "scrollHeight", { get: () => content, configurable: true });
  Object.defineProperty(el, "clientHeight", { get: () => view, configurable: true });
  const clamp = (v: number) => Math.max(0, Math.min(v, content - view));
  Object.defineProperty(el, "scrollTop", { get: () => top, set: (v: number) => { top = clamp(v); }, configurable: true });
  return (next: number) => { content = next; top = clamp(top); };
};

/** The reader moving the view: the browser moves it, then fires the event. */
const scrollTo = (el: HTMLElement, top: number) => { el.scrollTop = top; fireEvent.scroll(el); };

/** A live chat, long enough to scroll, following its end (bottom = 1400). */
const following = async () => {
  open([chatter(1)]);
  render(<Timeline task={task} />);
  const el = timeline();
  const setContent = size(el, 2000, 600);
  await grow(chatter(2));
  expect(el.scrollTop).toBe(1400);
  return { el, setContent };
};

describe("a reader scrolling up in a live chat is not pulled back to the bottom", () => {
  beforeEach(() => {
    (globalThis as unknown as { window: { setTimeout: typeof setTimeout } }).window ??= { setTimeout };
  });
  afterEach(() => cleanup());

  it("stops following on a small step up, the way a touchpad or scrollbar moves", async () => {
    const { el } = await following();
    // 10px up: inside the 80px that used to count as "still at the bottom".
    scrollTo(el, 1390);
    await grow(chatter(3));
    expect(el.scrollTop, "a streamed token must not pin the reader back down").toBe(1390);
    // And it keeps not following as the reader carries on up in small steps.
    scrollTo(el, 1380);
    await grow(chatter(4));
    expect(el.scrollTop).toBe(1380);
  });

  it("keeps a wheel tick whose smooth scroll has only just started", async () => {
    const { el } = await following();
    fireEvent.wheel(el, { deltaY: -100 });
    // First frame of the animation: 15px of the 100.
    scrollTo(el, 1385);
    await grow(chatter(3));
    expect(el.scrollTop).toBe(1385);
  });

  it("keeps the view where a wheel tick put it, even before the scroll event lands", async () => {
    const { el } = await following();
    // The browser has moved the view, but has not dispatched the scroll event.
    fireEvent.wheel(el, { deltaY: -120 });
    el.scrollTop = 1280;
    // A streamed token commits in that gap.
    await grow(chatter(3));
    expect(el.scrollTop, "the wheel tick must win over the bottom pin").toBe(1280);
  });

  it("stops following on a swipe upward, the touch equivalent of a wheel tick", async () => {
    const { el } = await following();
    fireEvent.touchStart(el, { touches: [{ clientY: 500 }] });
    // A finger moving down the screen moves the content up.
    fireEvent.touchMove(el, { touches: [{ clientY: 560 }] });
    el.scrollTop = 1300;
    await grow(chatter(3));
    expect(el.scrollTop).toBe(1300);
  });

  it("keeps following when the wheel scrolls a code block inside the transcript", async () => {
    const { el, setContent } = await following();
    const inner = document.createElement("pre");
    el.querySelector(".timeline-in")!.appendChild(inner);
    // jsdom keeps scrollTop on any element; a browser only on one that scrolls,
    // which is exactly the block that takes this wheel tick instead.
    Object.defineProperty(inner, "scrollTop", { get: () => 40, configurable: true });
    fireEvent.wheel(inner, { deltaY: -120 });
    // More output arrives: a follower is taken to the new end.
    setContent(2300);
    await grow(chatter(3));
    expect(el.scrollTop).toBe(1700);
  });

  it("still follows a chat that fits on screen, where there is nowhere to scroll up to", async () => {
    open([chatter(1)]);
    render(<Timeline task={task} />);
    const el = timeline();
    const setContent = size(el, 500, 600);
    fireEvent.wheel(el, { deltaY: -120 });
    // It has grown past the screen: a follower is taken to the new end.
    setContent(900);
    await grow(chatter(2));
    expect(el.scrollTop).toBe(300);
  });

  it("keeps following when the transcript shrinks under a reader at the end", async () => {
    // A finished turn folds its work away: the browser pulls the view up with
    // it. That is not the reader moving, and the chat must keep following.
    const { el, setContent } = await following();
    setContent(1500);
    fireEvent.scroll(el);
    expect(el.scrollTop).toBe(900);
    setContent(1800);
    await grow(chatter(3));
    expect(el.scrollTop).toBe(1200);
  });

  it("follows again once the reader scrolls back down to the end", async () => {
    const { el } = await following();
    fireEvent.wheel(el, { deltaY: -120 });
    scrollTo(el, 1280);
    await grow(chatter(3));
    expect(el.scrollTop).toBe(1280);
    // Back down to the end: following again.
    scrollTo(el, 1400);
    await grow(chatter(4));
    expect(el.scrollTop).toBe(1400);
  });
});
