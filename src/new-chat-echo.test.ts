// @vitest-environment jsdom
// A new chat opens on the message the user sent (a stand-in until the backend's
// own `user` row arrives), and opening a chat only fetches older history when
// the first page is too short to scroll to it.
import { afterEach, expect, it, vi } from "vitest";
import { api, type Item, type TaskSummary } from "./api";
import { NEW_CHAT_ECHO, openTask, set, settleNewChatEcho } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
afterEach(() => { vi.restoreAllMocks(); document.body.innerHTML = ""; });

const item = (id: string, kind: Item["kind"] = "text", data: Item["data"] = {}): Item => ({ id, kind, text: id, data, ts: "2026-10-17T10:00:00Z" });
const echo = item(`${NEW_CHAT_ECHO}t1`, "user");
const queued = item("pending-123-abc", "user");

it("keeps the stand-in while only startup progress arrives", () => {
  const s = { pendingMessages: { t1: [echo] } };
  expect(settleNewChatEcho(s, "t1", [item("n1", "notice", { level: "info", working: true, startup: true })])).toEqual({});
  expect(settleNewChatEcho(s, "t1", [item("x", "text")])).toEqual({});
});

it("retires the stand-in when the real user row arrives, keeping other pending sends", () => {
  const s = { pendingMessages: { t1: [echo, queued], t2: [queued] } };
  expect(settleNewChatEcho(s, "t1", [item("u1", "user")])).toEqual({ pendingMessages: { t1: [queued], t2: [queued] } });
  expect(settleNewChatEcho({ pendingMessages: { t1: [echo] } }, "t1", [item("u1", "user")])).toEqual({ pendingMessages: {} });
});

it("retires the stand-in when startup ends without sending", () => {
  const s = { pendingMessages: { t1: [echo] } };
  // The startup notice turned into a warning (stopped), an error notice, or a row was removed.
  expect(settleNewChatEcho(s, "t1", [item("n1", "notice", { level: "warning", working: false, startup: true })])).toEqual({ pendingMessages: {} });
  expect(settleNewChatEcho(s, "t1", [item("n2", "notice", { level: "error" })])).toEqual({ pendingMessages: {} });
  expect(settleNewChatEcho(s, "t1", null)).toEqual({ pendingMessages: {} });
});

it("never touches an ordinary in-flight send", () => {
  expect(settleNewChatEcho({ pendingMessages: { t1: [queued] } }, "t1", null)).toEqual({});
});

it("retires the stand-in when the opened page already holds the user row", async () => {
  vi.spyOn(api, "taskOpen").mockResolvedValue({ summary: { id: "t1", status: "running" } as unknown as TaskSummary, items: [item("u1", "user")], bg: [], cursor: null } as never);
  vi.spyOn(api, "draftGet").mockResolvedValue(null as never);
  set({ task: "t1", pendingMessages: { t1: [echo] } });
  await openTask("t1");
  const { get } = await import("./store");
  expect(get().pendingMessages.t1).toBeUndefined();
});

function mountTimeline(scrollHeight: number, clientHeight: number) {
  const el = document.createElement("div");
  el.className = "timeline";
  Object.defineProperty(el, "scrollHeight", { get: () => scrollHeight });
  Object.defineProperty(el, "clientHeight", { get: () => clientHeight });
  document.body.appendChild(el);
}

async function openPaged(id: string) {
  vi.spyOn(api, "taskOpen").mockResolvedValue({ summary: { id, status: "done" } as unknown as TaskSummary, items: [item("new")], bg: [], cursor: "older" } as never);
  const history = vi.spyOn(api, "taskHistory").mockResolvedValue({ items: [item("old")], cursor: null } as never);
  vi.spyOn(api, "draftGet").mockResolvedValue(null as never);
  set({ task: id });
  await openTask(id);
  await new Promise((resolve) => setTimeout(resolve, 0));
  return history;
}

it("does not prefetch older history for a page that already scrolls", async () => {
  mountTimeline(5000, 600);
  expect(await openPaged("long")).not.toHaveBeenCalled();
});

it("fills in older history when the first page is too short to scroll", async () => {
  mountTimeline(300, 600);
  expect(await openPaged("short")).toHaveBeenCalledWith("short", "older", 100);
});
