// @vitest-environment jsdom
// Deleting a chat has to drop everything the frontend kept for it. Each of these
// maps is keyed by task id and nothing else in the app can reach an entry once
// the chat is gone, so a missed one is memory held for the life of the process —
// and in the queue's case, the prompt text and any pasted image data URLs.
import { afterEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { forgetTask, loadPreviousHistory, openTask, set, type State } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { vi.resetAllMocks(); });

const state = (over: Record<string, unknown>) => ({
  tasks: {}, items: {}, subItems: {}, bg: {}, artificts: {}, sessionDrafts: {},
  attach: {}, attachFiles: {}, notes: {}, pendingMessages: {}, bisect: {}, studioView: {},
  hist: [], fwd: [], task: null, ...over,
}) as unknown as State;

describe("forgetting a chat", () => {
  it("drops every per-chat map, not just the transcript", () => {
    const before = state({
      tasks: { t1: { id: "t1" }, t2: { id: "t2" } },
      items: { t1: [], t2: [] },
      subItems: { t1: {} },
      bg: { t1: [] },
      artificts: { t1: {} },
      sessionDrafts: { t1: "half-written" },
      attach: { t1: [] },
      attachFiles: { t1: [] },
      notes: { t1: [] },
      // Queued messages: the text plus any image data URLs pasted with it.
      pendingMessages: { t1: [{ id: "p1", kind: "user", text: "queued", ts: "", data: {} }], t2: [] },
      bisect: { t1: { generation: 1 } },
      studioView: { t1: { artifictId: "a1", versionId: "v1", title: "T", number: 1, kind: "html" } },
    });
    const after = forgetTask(before, "t1") as Record<string, unknown>;
    expect(after.tasks).toEqual({ t2: { id: "t2" } });
    expect(after.pendingMessages).toEqual({ t2: [] });
    expect(after.bisect).toEqual({});
    expect(after.studioView).toEqual({});
    expect(after.items).toEqual({ t2: [] });
    expect(after.sessionDrafts).toEqual({});
    // The queue is the one that matters most: it holds prompt text.
    expect(JSON.stringify(after)).not.toContain("queued");
  });

  it("forgets the chat's place in its older history", async () => {
    const page = vi.fn(async () => ({ items: [], cursor: "c3" }));
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "task_open") {
        return {
          summary: { id: "t9", status: "done", subs: [], cwd: "", title: "t9" },
          items: [], sub_items: {}, bg: [], cursor: "c2",
        };
      }
      if (cmd === "task_history") return page();
      if (cmd === "draft_get") return "";
      return null;
    });
    const timeline = { scrollHeight: 900, scrollTop: 0, clientHeight: 200, isConnected: false } as unknown as HTMLElement;

    // Opening the chat is what seeds the paging cursor.
    await openTask("t9");
    await loadPreviousHistory("t9", timeline);
    expect(page).toHaveBeenCalledTimes(1);

    // With the cursor gone there is nothing left to page: a new chat that reuses
    // this id must not open halfway up someone else's history.
    set((s) => forgetTask(s, "t9"));
    await loadPreviousHistory("t9", timeline);
    expect(page).toHaveBeenCalledTimes(1);
  });
});
