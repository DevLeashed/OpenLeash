// A chat's status in the store must never move backwards.
//
// The store learns a chat's state two ways: pushed as a live `task` event, or
// read back as a *snapshot* — `app_boot`'s list at startup, `task_open` when a
// chat is opened, `tasks_list` when the page comes back into view. A snapshot is
// built at one instant and applied whenever it lands, so it races every event
// published while its request was in flight. Applying it unconditionally put the
// chat back to what it had been a moment before: a run that finished while you
// were opening it showed "running" (with a spinner that never stopped), a
// question you had already answered showed "waiting" — and nothing published
// that chat again, because a settled chat publishes nothing more. A reload ran
// the same race, so it came back intermittently and sometimes outlived it.
//
// These are the store-side regression tests. The backend half is `rev`, stamped
// on every summary it builds, and no test here needs a real backend: the revs
// are the whole contract.
// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { TaskSummary } from "./api";
import { boot, disposeBoot, get, go, mergeTaskSnapshot, newerSummary, resyncTasks, set, watchVisibility } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

afterEach(() => { disposeBoot(); vi.restoreAllMocks(); });

/** A summary of chat `t1`, as the backend would serialize one. */
const chat = (over: Partial<TaskSummary> = {}): TaskSummary =>
  ({ id: "t1", title: "chat one", status: "idle", waiting_kind: null, rev: 1, ...over }) as TaskSummary;

const handlers: Record<string, (event: { payload: unknown }) => void> = {};

function serve(routes: Record<string, (args: Record<string, unknown>) => unknown>) {
  (listen as unknown as ReturnType<typeof vi.fn>).mockImplementation(async (name: string, cb: (event: { payload: unknown }) => void) => {
    handlers[name] = cb;
    return () => { delete handlers[name]; };
  });
  (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
    const route = routes[cmd];
    return route ? route(args ?? {}) : null;
  });
}

/** Push a live `task` event the way the backend's bus does. */
const publish = (payload: Partial<TaskSummary>) =>
  handlers["ol://event"]?.({ payload: { task_id: "t1", kind: "task", payload } });

// ── the rule itself ────────────────────────────────────────────────

it("holds the newer summary and ignores a strictly older one", () => {
  const older = chat({ status: "running", rev: 7 });
  const newer = chat({ status: "done", rev: 8 });
  expect(newerSummary(older, newer).status).toBe("done");
  expect(newerSummary(newer, older).status).toBe("done");
});

it("applies a summary with no rev as before, so an older backend still works", () => {
  const held = chat({ status: "running", rev: 7 });
  const bare = { id: "t1", status: "done" } as TaskSummary;
  expect(newerSummary(held, bare).status).toBe("done");
  expect(newerSummary(undefined, bare).status).toBe("done");
});

it("folds a list in additively, keeping chats the list does not mention", () => {
  const held = { t1: chat({ status: "done", rev: 9 }), t2: chat({ id: "t2", rev: 2 }) };
  const merged = mergeTaskSnapshot(held, [chat({ status: "running", rev: 4 })]);
  expect(merged.t1!.status, "an older snapshot does not undo a newer event").toBe("done");
  expect(merged.t2, "a chat created while the snapshot was in flight survives it").toBeDefined();
});

// ── the race, through the real call sites ──────────────────────────

it("does not let boot's snapshot undo an event that landed while it was in flight", async () => {
  serve({
    // The snapshot is read by the backend *before* the run finishes below, and
    // arrives after it — exactly the ordering that used to lose the event.
    app_boot: () => {
      publish({ status: "done", step: "Done", rev: 11 });
      return { settings: {}, tasks: [chat({ status: "running", step: "Thinking", rev: 10 })] };
    },
  });
  await boot();
  expect(get().tasks.t1!.status).toBe("done");
  expect(get().tasks.t1!.step).toBe("Done");
});

it("does not let a chat's snapshot undo an event that landed while it was opening", async () => {
  serve({
    app_boot: () => ({ settings: {}, tasks: [chat({ status: "running", rev: 20 })] }),
    task_open: () => {
      publish({ status: "stopped", step: "Stopped", rev: 22 });
      return { summary: chat({ status: "running", step: "Thinking", rev: 21 }), items: [], bg: [], cursor: null, total: 0 };
    },
  });
  await boot();
  go("session", { task: "t1" });
  await vi.waitFor(() => expect(get().items.t1).toBeDefined());
  expect(get().tasks.t1!.status, "the chat was stopped while its page loaded").toBe("stopped");
});

it("reconciles the chat list when the page comes back into view", async () => {
  serve({
    app_boot: () => ({ settings: {}, tasks: [chat({ status: "running", rev: 30 })] }),
    // The page was frozen while this finished; the event was published to nobody.
    tasks_list: () => [chat({ status: "failed", step: "boom", rev: 44 })],
  });
  await boot();
  const stop = watchVisibility();
  try {
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.waitFor(() => expect(get().tasks.t1!.status).toBe("failed"));
  } finally {
    stop();
  }
  expect(invoke).toHaveBeenCalledWith("tasks_list");
});

it("never lets a resync undo a fresher event", async () => {
  serve({
    app_boot: () => ({ settings: {}, tasks: [chat({ status: "running", rev: 50 })] }),
    tasks_list: () => [chat({ status: "running", step: "Thinking", rev: 51 })],
  });
  await boot();
  publish({ status: "done", step: "Done", rev: 60 });
  await resyncTasks();
  expect(get().tasks.t1!.status, "the event is minutes newer than the list").toBe("done");
  set({ tasks: get().tasks });
  document.dispatchEvent(new Event("visibilitychange"));
});
