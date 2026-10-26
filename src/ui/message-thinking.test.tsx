// @vitest-environment jsdom
// Per-message thinking, from a reply's right-click menu.
//
// Reading one answer's reasoning is a different decision from turning thinking
// on for the whole chat, so the reply menu offers it per message. Two things
// have to hold: the switch reveals the thinking that *belongs to that reply*, and
// it leaves every other turn's reasoning folded. The menu itself must also be
// portalled to the document — rendered inside the reply it picked up the app's
// zoom twice and sat under the feed's clipping, which is why it could open far
// from the pointer or not appear at all.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { type Item, type TaskSummary } from "../api";
import { get, set } from "../store";
import { FeedRows } from "./Session";

const task = { id: "t1", title: "chat", subs: [], cwd: "D:/temporary", model: "", effort: 2, status: "running", busy: 0, usage: { cost: 0, last_context: 0, parts: {} }, context_window: 0 } as unknown as TaskSummary;

const item = (id: string, kind: string, text: string, data: Item["data"] = {}) => ({ id, kind, text, ts: "2026-10-26T10:00:00Z", data }) as Item;

/** Two turns, each with reasoning of its own, so "that message only" is testable. */
const items = () => [
  item("th1", "thinking", "FIRST-REASONING"),
  item("tx1", "text", "First answer", { model: "anthropic/claude-opus-5" }),
  item("th2", "thinking", "SECOND-REASONING"),
  item("tx2", "text", "Second answer", { model: "anthropic/claude-opus-5" }),
];

beforeEach(() => {
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener() {}, removeEventListener() {} })));
  set({ thinkFor: {}, settings: {} as never });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  set({ thinkFor: {} });
});

describe("a reply's own thinking", () => {
  it("is hidden until its message menu reveals it, and then only for that message", () => {
    render(<FeedRows items={items()} task={task} showThinking={false} />);
    expect(screen.queryByText("FIRST-REASONING")).toBeNull();
    expect(screen.queryByText("SECOND-REASONING")).toBeNull();

    fireEvent.contextMenu(screen.getByText("First answer"));
    fireEvent.click(screen.getByText("Show thinking"));

    // The reply's own reasoning is on screen; the other turn's is not.
    expect(screen.getByText("FIRST-REASONING")).toBeTruthy();
    expect(screen.queryByText("SECOND-REASONING")).toBeNull();
    expect(get().thinkFor.tx1).toBe(true);
  });

  it("turns back off from the same menu, and says which way the switch goes", () => {
    render(<FeedRows items={items()} task={task} showThinking={false} />);
    fireEvent.contextMenu(screen.getByText("First answer"));
    fireEvent.click(screen.getByText("Show thinking"));
    expect(screen.getByText("FIRST-REASONING")).toBeTruthy();

    fireEvent.contextMenu(screen.getByText("First answer"));
    fireEvent.click(screen.getByText("Hide thinking"));
    expect(screen.queryByText("FIRST-REASONING")).toBeNull();
    expect(get().thinkFor.tx1).toBe(false);
  });

  it("offers no switch on a reply that has no reasoning to show", () => {
    render(<FeedRows items={[item("tx3", "text", "Bare answer", { model: "anthropic/claude-opus-5" })]} task={task} showThinking={false} />);
    fireEvent.contextMenu(screen.getByText("Bare answer"));
    // The model and the copy rows are there; the thinking switch is not.
    expect(screen.getByText("Copy message")).toBeTruthy();
    expect(screen.queryByText("Show thinking")).toBeNull();
  });

  it("opens the menu in the document, not inside the reply that was clicked", () => {
    render(<FeedRows items={items()} task={task} showThinking={false} />);
    const reply = screen.getByText("First answer").closest(".reply")!;
    fireEvent.contextMenu(reply, { clientX: 40, clientY: 60 });
    const menu = document.querySelector(".pop.ctx")!;
    expect(menu).toBeTruthy();
    // A portal puts the layer outside the transcript; inside it, the app's zoom
    // and the feed's clipping both applied to it.
    expect(reply.contains(menu)).toBe(false);
    expect(menu.closest(".app")).toBeNull();
  });
});
