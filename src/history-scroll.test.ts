// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { api, type Item, type TaskSummary } from "./api";
import { loadPreviousHistory, openTask, set } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
afterEach(() => { vi.restoreAllMocks(); });

const item = (id: string): Item => ({ id, kind: "text", text: id, data: {}, ts: "2026-10-16T10:00:00Z" });
const summary = (id: string) => ({ id, title: "Chat", status: "done" } as unknown as TaskSummary);

/** A scroll box with no layout engine: `content` is how tall the transcript
 *  is, and it grows when older history is prepended. */
function timeline(content: number, top: number, view = 600) {
  const box = { content, scrollTop: top, clientHeight: view, isConnected: true };
  Object.defineProperty(box, "scrollHeight", { get: () => box.content });
  return box as typeof box & { scrollHeight: number };
}

async function openWithHistory(id: string) {
  vi.spyOn(api, "taskOpen").mockResolvedValue({ summary: summary(id), items: [item("new")], bg: [], cursor: "older" } as never);
  vi.spyOn(api, "taskHistory").mockResolvedValue({ items: [item("old-1"), item("old-2")], cursor: null } as never);
  vi.spyOn(api, "draftGet").mockResolvedValue(null as never);
  set({ task: id });
  await openTask(id);
}
const frame = () => new Promise((resolve) => requestAnimationFrame(() => resolve(null)));

it("keeps a reader at the live end when older history arrives, even when the transcript fit on screen", async () => {
  await openWithHistory("short-chat");
  // The feed fits (a finished turn folded its work away): top is the bottom.
  const box = timeline(400, 0);
  const load = loadPreviousHistory("short-chat", box as unknown as HTMLElement);
  box.content = 2400;
  await load; await frame();
  // Before the fix this kept the distance from the top (0 + growth = 2000 of
  // 2400 at best, 0 in the short case) and the chat showed older messages.
  expect(box.scrollTop).toBe(2400);
});

it("does not yank a reader back to where they were when they asked for older messages", async () => {
  await openWithHistory("moved-chat");
  const box = timeline(3000, 50);
  const load = loadPreviousHistory("moved-chat", box as unknown as HTMLElement);
  // The reader keeps scrolling while the page is in flight, up to the top.
  box.scrollTop = 0;
  box.content = 4000;
  await load; await frame();
  // Keep the content they are on, moved down by the 1000px of rows above it. The
  // old offset (50 + 1000) would have jumped them back down the transcript.
  expect(box.scrollTop).toBe(1000);
});

it("keeps the place of a reader who scrolled up to read older messages", async () => {
  await openWithHistory("long-chat");
  const box = timeline(3000, 50);
  const load = loadPreviousHistory("long-chat", box as unknown as HTMLElement);
  box.content = 4000;
  await load; await frame();
  expect(box.scrollTop).toBe(1050);
});
