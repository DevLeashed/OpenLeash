// @vitest-environment jsdom
/// <reference types="node" />
// A question card whose text runs long used to arrive with its top above the
// window.
//
// The transcript is pinned to the bottom (`el.scrollTop = el.scrollHeight`)
// under a floating composer, and the timeline pads past that composer, so the
// usable band is the whole pane minus a good 150px. A question with a long
// title, intro or description is taller than that band: the pin is not wrong,
// it is just the wrong anchor for a card this shape, and it put the *start* of
// the question above the top edge. What the user saw was the last option and
// the answer buttons, with nothing on the card to say there was text above.
//
// The fix re-anchors on the card when its top has gone negative. Two things
// have to hold for it to be worth anything: it has to fire, and it must not
// fire on a card that already fits — moving that one would throw away the
// position the reader was at, which is a worse bug than the one being fixed.
//
// The timeline is exercised directly rather than through `Session`: the fix is
// entirely in here, and standing up the whole session pulls the composer and
// the details panel in for a scroll position that has nothing to do with them.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import { get, set } from "../store";
import { Timeline } from "./Session";
import type { Item, TaskSummary } from "../api";

const task = {
  id: "t1", title: "chat", status: "waiting", model: "anthropic/claude-opus-5", branch: "main",
  subs: [], usage: {}, todos: [], pending: {}, ...({} as Record<string, never>),
} as unknown as TaskSummary;

const question = (extra: Record<string, unknown> = {}): Item =>
  ({
    id: "q1",
    kind: "question",
    text: "",
    ts: "2026-09-01T00:00:00Z",
    data: {
      title: "A title long enough to matter",
      questions: [{ question: "Which database?", type: "single", options: [{ label: "Postgres" }, { label: "SQLite" }] }],
      ...extra,
    },
  }) as Item;

const chatter = (n: number): Item =>
  ({ id: `t${n}`, kind: "text", text: "working", ts: "2026-09-01T00:00:00Z" }) as Item;

/** Stand the store up with a transcript whose last item is `last`. */
const open = (last: Item) => {
  set({
    view: "session" as const,
    task: "t1",
    tasks: { t1: task },
    items: { t1: [last] },
    settings: { show_thinking: false } as never,
  });
};

/**
 * More transcript. This is what the effect is keyed on (`tail` is the item
 * count and the length of the last item's text), so it is also the only way to
 * make the effect re-run the way it does in the app: on a growing transcript.
 *
 * `set` notifies outside React's batching, so the commit is not done when this
 * returns — the async `act` is what waits for it, and the effect is a layout
 * effect, so reading `scrollTop` before it has run reads a stale value.
 */
const grow = async (...extra: Item[]) => {
  await act(async () => { set({ items: { t1: get().items.t1!.concat(extra) } }); });
};

/**
 * jsdom does no layout, so `offsetTop` is always 0 and the effect's condition
 * (`offsetTop < 0`) is never true. Stating the card's position is the whole
 * setup: the effect reads that one number and writes `scrollTop`.
 *
 * The property is on the card's DOM node, which React keeps across a re-render
 * of the same item, so a position set once survives the transcript growing.
 */
const placeCardAt = (top: number) => {
  const card = document.querySelector<HTMLElement>("[data-pending-question]")!;
  Object.defineProperty(card, "offsetTop", { get: () => top, configurable: true });
};

const timeline = () => document.querySelector<HTMLElement>(".timeline")!;

describe("a pending question card is anchored so its top is on screen", () => {
  beforeEach(() => {
    (globalThis as unknown as { window: { setTimeout: typeof setTimeout } }).window ??= { setTimeout };
  });
  afterEach(() => cleanup());

  it("scrolls the card into view when the bottom pin pushed its top off screen", async () => {
    open(question());
    render(<Timeline task={task} />);
    // The pin has left the card's top 120px above the top of the pane.
    placeCardAt(-120);
    await grow(chatter(1));

    // It scrolled back to the card's own top (less the 12px inset), not to the
    // bottom of the pane: the point is to make the *start* of the question
    // visible again, which is the part the pin had eaten.
    expect(timeline().scrollTop).toBe(-132);
  });

  it("leaves a card that already fits exactly where the reader is", async () => {
    open(question());
    render(<Timeline task={task} />);
    // Already in view: the top is on screen, so there is nothing to fix.
    placeCardAt(40);
    await grow(chatter(1));

    expect(timeline().scrollTop, "a card that fits must not be scrolled").toBe(0);
  });

  // The regression risk in the fix itself: the effect is gated on the
  // transcript changing, and the bottom pin runs on exactly the same trigger.
  // If the pin ran last it would drag the card's top straight back off screen,
  // and the question would be unreadable again for as long as the agent kept
  // talking. So the anchoring has to win, every time, for as long as the
  // question is unanswered.
  it("keeps the card anchored as the transcript keeps growing", async () => {
    open(question());
    render(<Timeline task={task} />);
    placeCardAt(-120);
    await grow(chatter(1));
    expect(timeline().scrollTop).toBe(-132);

    // More transcript: the pin would move, and the card is off the top again.
    placeCardAt(-200);
    await grow(chatter(2));
    expect(timeline().scrollTop, "re-anchored, not dragged back to the bottom").toBe(-212);
  });

  it("does not fight the reader once the question is answered", async () => {
    // After the answer the card collapses to a one-line summary and stops
    // carrying the attribute. A resolved card must not count as pending, or
    // scrolling back through history would keep re-anchoring on an old answer.
    open(question({ answers: ["Postgres"] }));
    render(<Timeline task={task} />);
    expect(document.querySelector("[data-pending-question]"), "a resolved card is not pending").toBeNull();
    await grow(chatter(1));
    expect(timeline().scrollTop).toBe(0);
  });
});
