/// <reference types="node" />
// The queue list is the only place a message the user hasn't sent yet is shown,
// so what it draws — and what it leaves out of the transcript — is worth pinning.
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import type { Item, TaskSummary } from "../../api";
import { set } from "../../store";
import { queueOf, QueueList } from "./QueueList";
import { FeedRows } from "../Session";

const task = { id: "t1" } as unknown as TaskSummary;

const queued = (id: string, text: string, data: Record<string, unknown> = {}): Item =>
  ({ id, kind: "user", text, data: { queued: true, seq: 0, ...data }, ts: "2026-09-01T00:00:00Z" }) as Item;

const sent = (id: string, text: string): Item =>
  ({ id, kind: "user", text, data: {}, ts: "2026-09-01T00:00:00Z" }) as Item;

function render(items: Item[]) {
  set({ items: { t1: items }, task: "t1", tasks: { t1: task } });
  return renderToStaticMarkup(createElement(QueueList, { task }));
}

describe("the queue list", () => {
  beforeEach(() => set({ items: {}, task: "t1", tasks: { t1: task } }));

  it("is only as long as the queue, and says what the rows are for", () => {
    const html = render([queued("a", "first", { seq: 0 }), queued("b", "second", { seq: 1 })]);
    expect(html).toContain("2 after this turn");
    expect(html).toContain("first");
    expect(html).toContain("second");
  });

  it("draws nothing at all when the agent is working and nothing is waiting", () => {
    // A stray empty panel above the composer reads as a broken composer.
    expect(render([sent("a", "already sent")])).toBe("");
    expect(render([])).toBe("");
  });

  it("orders the rows the way the agent will read them, not the way they were added", () => {
    // Sent first means read first: `seq` is the send order, so the list follows it.
    const html = render([queued("a", "added first", { seq: 7 }), queued("b", "sent first", { seq: 2 })]);
    expect(html.indexOf("sent first")).toBeLessThan(html.indexOf("added first"));
  });

  it("shows only what the user deferred, not everything typed mid-turn", () => {
    // A plain Enter steers on the spot and never queues, so the list is exactly
    // the Alt-Enter messages — the one thing about them the chat can't show.
    const rows = queueOf([queued("a", "deferred", { seq: 0 }), sent("b", "steered straight away")]);
    expect(rows.map((r) => r.text)).toEqual(["deferred"]);
  });

  it("offers a way to send one message now, and to clear the lot", () => {
    const html = render([queued("a", "first", { seq: 0 })]);
    expect(html).toContain("Send now");
    expect(html).toContain("Send all");
    expect(html).toContain("Discard");
    // Removing a row must not be reachable only through a hidden menu.
    expect(html).toContain("Remove from the queue");
  });

  it("keeps a message out of the transcript until it has actually been sent", () => {
    // The bug this list fixes: a queued message rendered as an ordinary bubble,
    // indistinguishable from one the agent had read.
    const html = renderToStaticMarkup(createElement(FeedRows, {
      items: [sent("s", "answered already"), queued("q", "still waiting")],
      task,
      showThinking: false,
    }));
    expect(html).toContain("answered already");
    expect(html).not.toContain("still waiting");
  });
});
