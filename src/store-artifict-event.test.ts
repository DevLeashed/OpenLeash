// @vitest-environment jsdom
// The artifict event is a wire contract between `Harness::emit_artifict` in Rust
// and this store. A rename on either side that is not mirrored here would leave
// an open editor silently stale — the artifict would still save, and nothing
// would look broken until someone noticed the preview never moved.
import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen, type EventCallback } from "@tauri-apps/api/event";
import { boot, disposeBoot, evictTranscripts, forgetTask, get, set } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ isFocused: async () => true, isVisible: async () => true, isMinimized: async () => false, onFocusChanged: async () => () => {} }) }));

afterEach(() => { disposeBoot(); vi.resetAllMocks(); set({ artificts: {} }); });

type Event = { task_id: string; kind: string; payload: unknown };

it("records the latest published version per artifict from the task event", async () => {
  let callback!: EventCallback<Event>;
  vi.mocked(listen).mockImplementation(async (event, handler) => {
    if (event === "ol://event") callback = handler as EventCallback<Event>;
    return () => {};
  });
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "app_boot") return { settings: { model: "", perm: "turbo", effort: 2, project: "", routes: [] }, tasks: [], providers: [], models: [], accounts: [], agents: [] };
    if (command === "task_get") return { summary: {}, items: [], sub_items: {}, bg: [] };
    if (command === "draft_get") return "";
    return null;
  });
  await boot();

  callback({ event: "ol://event", id: 1, payload: { task_id: "t1", kind: "artifict", payload: { artifict_id: "a1", version_id: "v1" } } });
  expect(get().artificts.t1?.a1).toBe("v1");

  // A later revision replaces it, and a second artifict is tracked separately:
  // the map is "what is the newest for this artifict", not a growing log.
  callback({ event: "ol://event", id: 2, payload: { task_id: "t1", kind: "artifict", payload: { artifict_id: "a1", version_id: "v2" } } });
  callback({ event: "ol://event", id: 3, payload: { task_id: "t1", kind: "artifict", payload: { artifict_id: "a2", version_id: "w1" } } });
  expect(get().artificts.t1).toEqual({ a1: "v2", a2: "w1" });

  // Another chat's artifict must not leak into this one.
  callback({ event: "ol://event", id: 4, payload: { task_id: "t2", kind: "artifict", payload: { artifict_id: "a1", version_id: "v9" } } });
  expect(get().artificts.t1?.a1).toBe("v2");
  expect(get().artificts.t2?.a1).toBe("v9");
});

it("drops an artifict map that is no longer reachable", () => {
  // Without this the map is a slow leak: one entry per artifict of every chat
  // the user has ever opened, held for the life of the process. A chat that is
  // neither open nor on a nav stack is gone, so its map goes with it.
  type ForgetArg = Parameters<typeof forgetTask>[0];
  type EvictArg = Parameters<typeof evictTranscripts>[0];
  const state = (artificts: Record<string, Record<string, string>>, hist: unknown[] = []) => ({ tasks: {}, items: {}, subItems: {}, bg: {}, hist, fwd: [], task: null, artificts });

  const forgotten = forgetTask(state({ t1: { a1: "v1" }, t2: { a2: "v2" } }) as unknown as ForgetArg, "t1") as { artificts?: Record<string, unknown> };
  expect(forgotten.artificts).toEqual({ t2: { a2: "v2" } });

  // `t2` is open, so its map stays; `t9` is reachable with Back, so it stays;
  // `t1` is neither and is dropped.
  const evicted = evictTranscripts(state({ t1: { a1: "v1" }, t2: { a2: "v2" }, t9: { a9: "v9" } }, [{ view: "session", task: "t9" }]) as unknown as EvictArg, "t2") as { artificts?: Record<string, unknown> };
  expect(evicted.artificts).toEqual({ t2: { a2: "v2" }, t9: { a9: "v9" } });

  // Nothing reachable to drop means no state churn at all.
  expect(evictTranscripts(state({ t2: { a2: "v2" } }) as unknown as EvictArg, "t2")).toEqual({});
});
