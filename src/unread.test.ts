// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { TaskSummary } from "./api";
import { boot, disposeBoot, get, go, set } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

const chat = { id: "t1", title: "chat one", status: "waiting", waiting_kind: "question" } as TaskSummary;

afterEach(() => { disposeBoot(); vi.restoreAllMocks(); });

it("keeps an unanswered blocking chat marked until the backend ends the wait", async () => {
  const handlers: Record<string, (event: { payload: any }) => void> = {};
  (listen as unknown as ReturnType<typeof vi.fn>).mockImplementation(async (name: string, cb: (event: { payload: any }) => void) => {
    handlers[name] = cb;
    return () => { delete handlers[name]; };
  });
  (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(async (cmd: string) => {
    if (cmd === "app_boot") return { settings: {}, tasks: [chat], project: "" };
    if (cmd === "task_get") return { summary: chat, items: [], sub_items: {}, bg: [] };
    return null;
  });
  await boot();
  set({ unread: { t1: true } });

  go("session", { task: "t1" });
  expect(get().unread.t1, "opening only dismisses the transient notice").toBe(true);
  go("home", { task: null });
  expect(get().unread.t1, "leaving without answering preserves attention").toBe(true);

  handlers["ol://event"]?.({ payload: { task_id: "t1", kind: "task", payload: { status: "running", waiting_kind: null } } });
  expect(get().unread.t1, "the backend ending the wait acknowledges it").toBe(false);
});
