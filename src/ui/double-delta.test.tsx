// @vitest-environment jsdom
// Streamed text must land in the transcript exactly once.
//
// The field bug: every reply came out doubled from the first flush boundary
// on — "I'll look at how presentation and UI are currently built, and at what
// the original looked like, before giving you an opinion. at how presentation
// and UI are currently built, and at what the original looked like, before
// giving you an opinion." The tail is a verbatim replay of the head, cut at a
// word boundary, which is what a second copy of the same deltas looks like.
//
// The cause is in `boot()`, not the model and not the provider:
//
//   `listenersAttached` is set synchronously, but `attachListeners()` is
//   awaited. `disposeBoot()` (App's effect cleanup — StrictMode's remount, an
//   HMR reload, or a retry) resets the flag and nulls `bootInflight` while that
//   await is still in flight. The next `boot()` therefore sees
//   `listenersAttached === false`, attaches a *second* "ol://event" listener,
//   and every subsequent event is handled twice.
//
// Why it shows up as this shape and not as "every word doubled": the two event
// kinds are not equally idempotent. `item` replaces by id (`list[i] =
// payload`), so the second listener re-applying it is invisible. `delta`
// *appends* (`text: x.text + chunk`), so the second listener doubles the text
// — but only for deltas, and the first text of a block arrives as an `item`,
// not a `delta` (see `StreamUi::delta` in runner.rs, which emits the opening
// chunk with the item). Hence: first chunk single, everything after it twice.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import type { Item, Settings, TaskSummary } from "../api";
import { boot, disposeBoot, flushDeltas, get, set } from "../store";

const chat = (): TaskSummary => ({
  id: "t1", title: "chat one", status: "running", paused: null, unpaused: false, busy: 0,
  model: "anthropic/claude-opus-5", branch: "main", project: "D:/work/app",
  ultra: true, subs: [], archived: false,
  updated_at: "2026-10-04T00:00:00Z", touched_at: "2026-10-04T00:00:00Z",
} as unknown as TaskSummary);

/**
 * A `listen` that records every registration per event name, so a test can count
 * how many live handlers an event has. The mock in the other suites keeps one
 * handler per name, which hides exactly the stacking this file is about.
 */
const liveHandlers = (name: string) => (listen as any).live[name] ?? [];

const mockListen = () => {
  (listen as any).live = {};
  (listen as unknown as ReturnType<typeof vi.fn>).mockImplementation(async (name: string, cb: any) => {
    const all: any[] = ((listen as any).live[name] ??= []);
    all.push(cb);
    return () => {
      const i = all.indexOf(cb);
      if (i >= 0) all.splice(i, 1);
    };
  });
  (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(async (cmd: string) =>
    cmd === "app_boot" ? { settings: {}, tasks: [chat()], project: "" } : null,
  );
};

/** Fires one `ol://event` the way the backend sends it, to every live listener. */
const fire = (payload: Record<string, unknown>) => {
  for (const cb of liveHandlers("ol://event")) cb({ payload: { task_id: "t1", ...payload } });
};

const replyItem = (text: string): Item => ({ id: "a", kind: "text", text, ts: "2026-10-04T00:00:00Z", data: {} } as Item);

beforeEach(() => {
  mockListen();
  (globalThis as any).window ??= { setTimeout: (f: () => void, ms: number) => setTimeout(f, ms) };
  set({
    view: "session", task: "t1", items: {}, subItems: {}, tasks: { t1: chat() },
    settings: {} as unknown as Settings,
  });
});
afterEach(() => { flushDeltas(); disposeBoot(); vi.restoreAllMocks(); });

describe("one boot attaches one event listener", () => {
  it("does not stack a second ol://event listener when the app remounts mid-boot", async () => {
    // StrictMode mounts <App/>, unmounts, remounts. If cleanup runs while
    // attachListeners() is still awaiting, the second boot must not attach a
    // second set.
    const first = boot();
    disposeBoot();
    await first;
    await boot();
    expect(liveHandlers("ol://event"), "one live listener, whatever the remount did").toHaveLength(1);
  });

  it("streams a reply in once, not twice", async () => {
    const first = boot();
    disposeBoot();
    await first;
    await boot();
    expect(liveHandlers("ol://event")).toHaveLength(1);

    // The exact shape from the field: an opening `item` carrying the first
    // chunk, then `delta`s carrying the rest.
    fire({ kind: "item", payload: replyItem("I'll look at ") });
    flushDeltas();
    fire({ kind: "delta", payload: { item_id: "a", text: "how it's built." } });
    fire({ kind: "delta", payload: { item_id: "a", text: " Before that," } });
    flushDeltas();

    expect(get().items.t1?.map((x) => x.text)).toEqual(["I'll look at how it's built. Before that,"]);
  });
});