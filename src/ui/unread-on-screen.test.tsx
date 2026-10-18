// @vitest-environment jsdom
// A chat you are looking at is already read, even when it finishes under you.
//
// The field bug: a chat open on screen finished, and the backend marked it
// unread (the blue dot). The sidebar hides the dot while the chat is on screen,
// so nothing looked wrong; but the mark stayed on the backend, and switching to
// another chat revealed a "done" dot on a chat the user had already watched
// finish. Only opening a chat cleared it. The window now acknowledges the
// attention itself when the chat is on screen (`api.taskSeen`), and nothing else.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Settings, TaskSummary } from "../api";
import { boot, disposeBoot, get, set } from "../store";

const chat = (extra: Partial<TaskSummary> = {}): TaskSummary => ({
  id: "t1", title: "chat one", status: "running", paused: null, unpaused: false, busy: 0,
  model: "anthropic/claude-opus-5", branch: "main", project: "D:/work/app",
  ultra: false, subs: [], archived: false,
  updated_at: "2026-09-26T00:00:00Z", touched_at: "2026-09-26T00:00:00Z",
  ...extra,
} as TaskSummary);

describe("a finished chat you are looking at is acknowledged", () => {
  const listenMock = listen as unknown as ReturnType<typeof vi.fn> & { handlers?: Record<string, (event: { payload: unknown }) => void> };
  const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;

  const bootWith = async (tasks: TaskSummary[]) => {
    listenMock.mockClear();
    invokeMock.mockClear();
    listenMock.mockImplementation(async (name: string, cb: (event: { payload: unknown }) => void) => {
      listenMock.handlers = { ...(listenMock.handlers ?? {}), [name]: cb };
      return () => { delete listenMock.handlers![name]; };
    });
    invokeMock.mockImplementation(async (cmd: string, args: { patch?: Record<string, unknown> } = {}) => {
      if (cmd === "app_boot") return { settings: {}, tasks, project: "" };
      if (cmd === "settings_update") return { ...args.patch };
      return null;
    });
    await boot();
  };
  const attention = (payload: Record<string, unknown>) =>
    listenMock.handlers!["ol://attention"]!({ payload: { task_id: "t1", kind: "done", ...payload } });
  const seenCalls = () => invokeMock.mock.calls.filter(([cmd]) => cmd === "task_seen");

  beforeEach(() => { set({ view: "home", task: null, notices: [], settings: { project: "D:/work/app" } as unknown as Settings }); });
  afterEach(() => disposeBoot());

  it("acknowledges a chat that finishes while it is on screen", async () => {
    await bootWith([chat()]);
    set({ view: "session", task: "t1", items: { t1: [] } });
    await act(async () => { attention({ kind: "done" }); });
    expect(seenCalls(), "the user is looking at it, so the backend must not keep it unread").toEqual([["task_seen", { id: "t1" }]]);
  });

  it("leaves a chat unread when it finishes while you are somewhere else", async () => {
    await bootWith([chat(), chat({ id: "t2", title: "chat two" })]);
    set({ view: "session", task: "t2", items: { t2: [] } });
    await act(async () => { attention({ kind: "done" }); });
    expect(seenCalls(), "t1 finished off screen, so it is still news").toEqual([]);
    expect(get().notices.some((n) => n.task_id === "t1"), "and it still gets its notice").toBe(true);
  });
});
