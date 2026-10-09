// @vitest-environment jsdom
// The pause banner must never claim a pause is settled while it is still landing.
//
// "Pause all" flips the global flag optimistically, and the per-chat in-flight
// command counts arrive a beat later. In that gap the counts read zero, so the
// banner used to fall through to the flat "Everything is paused" — which reads
// as *nothing is happening, quit the app*. The user reported exactly that: a
// pause banner that made them think the app had hung.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import type { Settings, TaskSummary } from "../api";
import { set } from "../store";
import { Home } from "./Home";

const t = (extra: Partial<TaskSummary> = {}): TaskSummary => ({
  id: "a", title: "chat a", status: "running", paused: null, unpaused: false, busy: 0,
  model: "anthropic/claude-opus-5", branch: "main", project: "D:/work/app", updated_at: "2026-09-26T00:00:00Z",
  ...extra,
} as TaskSummary);

const pausedAll = (tasks: Record<string, TaskSummary> = { a: t() }) =>
  set({ tasks, settings: { paused_all: true, paused_reason: "Paused everything" } as unknown as Settings });

/** The pause button on the home page, which is what starts the settling gap. */
const pauseAllButton = () => [...document.querySelectorAll("button")].find((b) => b.textContent?.includes("Pause all"));

describe("the pause banner while a pause is landing", () => {
  beforeEach(() => {
    (globalThis as unknown as { window: { setTimeout: typeof setTimeout } }).window ??= { setTimeout };
  });
  afterEach(() => { cleanup(); vi.useRealTimers(); });

  it("says Loading while the counts are on their way, not 'Everything is paused'", async () => {
    // Pressed, flag not yet in: the banner is not up yet at all.
    set({ tasks: { a: t() }, settings: { paused_all: false } as unknown as Settings });
    render(<Home />);
    await act(async () => { pauseAllButton()!.click(); });

    // The flag has landed but the recount has not, which is the gap in question.
    await act(async () => { pausedAll({ a: t() }); });
    const mid = document.body.textContent!;
    expect(mid).toContain("Loading");
    expect(mid, "the flat claim is the exact thing that made people quit the app").not.toContain("Everything is paused");
  });

  it("settles to the real state once the counts arrive", async () => {
    set({ tasks: { a: t() }, settings: { paused_all: false } as unknown as Settings });
    render(<Home />);
    // Fake timers before the press: the settling window is a timer started by
    // the click, and a real one would be gone by the time we advanced.
    vi.useFakeTimers();
    await act(async () => { pauseAllButton()!.click(); });
    // The counts are known: two commands still finishing.
    await act(async () => { pausedAll({ a: t({ busy: 2 }) }); });
    await act(async () => { vi.advanceTimersByTime(600); });
    const settled = document.body.textContent!;
    expect(settled).toContain("Pausing… waiting on 2 commands to finish");
    expect(settled).not.toContain("Loading");
  });

  it("never says 'Everything is paused' while draining, once the pause is real", async () => {
    // With counts known and commands in flight, the banner names the wait.
    pausedAll({ a: t({ busy: 1 }) });
    render(<Home />);
    const html = document.body.textContent!;
    expect(html).toContain("Pausing… waiting on 1 command to finish");
    expect(html).not.toContain("Everything is paused");
  });

  it("says nothing at all when the flag is on but holding nothing", async () => {
    // The field report, verbatim: "it says everything is paused when literally
    // nothing is paused". This is its shape. `paused_all` is sticky — it is set by
    // the freeze a quit leaves behind and comes back off disk — but what it holds
    // is only chats that are *working*. Every chat here has finished or stopped,
    // so the flag holds nothing, and the paused list correctly reads "Nothing is
    // frozen right now". The banner was reading the raw flag anyway, so it
    // announced a global pause over a list saying there wasn't one.
    pausedAll({ a: t({ status: "done" }), b: t({ status: "stopped" }) });
    render(<Home />);
    const html = document.body.textContent!;
    expect(html, "nothing is paused, so nothing may say it is").not.toContain("Everything is paused");
    expect(html).not.toContain("Pausing…");
  });

  it("still shows a real global pause after a chat finished", async () => {
    // The other half of that fix: gating on what the flag holds must not throw
    // away a pause that is real. One working chat under the flag is enough, and
    // the finished one next to it does not cancel it.
    pausedAll({ a: t({ status: "done" }), b: t({ status: "running" }) });
    render(<Home />);
    expect(document.body.textContent!).toContain("Everything is paused");
  });
});
