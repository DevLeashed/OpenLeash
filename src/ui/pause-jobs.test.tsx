// @vitest-environment jsdom
// The "Everything is paused" banner has to name the background commands that are
// still alive.
//
// A pause is not a freeze of the machine: a dev server, a watcher or a build
// keeps running through it, and that is deliberate — a job that never exits by
// itself would hold "Pausing… waiting on N commands to finish" up forever, so
// the backend leaves them out of that count (see `refresh_busy` in
// `src-tauri/src/agent/mod.rs`). The price is a banner reading "Everything is
// paused" while a port is still being served and a build is still rebuilding on
// every save, which is the one reading that makes a user quit the app on top of
// their work. A chat that has finished next to a live dev server is the worst
// case: nothing about it is frozen at all.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type Bg, type Settings, type TaskSummary } from "../api";
import { set } from "../store";
import { Home } from "./Home";

/** A background command that is still up. `started` is relative to now, so the
 *  "up" column does not depend on the day the suite runs. */
const job = (over: Partial<Bg> = {}): Bg => ({
  id: "bg1", cmd: "npm run dev", started: new Date(Date.now() - 12 * 60_000).toISOString(),
  last_line: "ready in 320ms", running: true, exit: null, ...over,
});

/** `bg_now` is empty unless a case says otherwise: the backend publishes only
 *  jobs that are alive, and a summary carries nothing for a chat with none. */
const t = (id: string, extra: Partial<TaskSummary> = {}): TaskSummary => ({
  id, title: `chat ${id}`, status: "running", paused: null, unpaused: false, busy: 0, bg_now: [],
  model: "anthropic/claude-opus-5", branch: "main", project: "D:/work/app", updated_at: "2026-09-26T00:00:00Z",
  ...extra,
} as TaskSummary);

const pausedAll = (tasks: Record<string, TaskSummary>) =>
  set({ tasks, settings: { paused_all: true, paused_reason: "Paused everything" } as unknown as Settings });

/** The job rows as they read on screen: command, chat and time in one string. */
const rows = () => [...document.querySelectorAll(".pb-job")].map((el) => el.textContent!);

describe("the background jobs a pause does not stop", () => {
  beforeEach(() => {
    (globalThis as unknown as { window: { setTimeout: typeof setTimeout } }).window ??= { setTimeout };
    vi.spyOn(api, "bgKill").mockResolvedValue(undefined);
  });
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  it("names each live job, the chat it belongs to, and how long it has been up", () => {
    pausedAll({ a: t("a", { bg_now: [job()] }) });
    render(<Home />);
    expect(document.body.textContent).toContain("Everything is paused");
    expect(rows()).toHaveLength(1);
    expect(rows()[0]).toContain("npm run dev");
    expect(rows()[0], "a command with no chat named is one nobody can go and look at").toContain("chat a");
    expect(rows()[0]).toContain("up 12m");
    expect(document.querySelector(".pb-jobs-h")?.textContent).toContain("1 background command · not stopped by the pause");
  });

  it("names a job in a chat that has finished, where nothing is frozen at all", () => {
    // The global pause holds chats that are *working*; a finished chat with a
    // dev server under it is held by nothing, which is exactly why the server is
    // worth naming.
    pausedAll({ a: t("a"), b: t("b", { status: "done", bg_now: [job({ id: "bg2", cmd: "node watch.js" })] }) });
    render(<Home />);
    expect(rows()).toHaveLength(1);
    expect(rows()[0]).toContain("node watch.js");
    expect(rows()[0]).toContain("chat b");
  });

  it("never lists a job that has exited", () => {
    // The summary only ever carries live ones, and this pins the rendering side
    // of that too: a dead server left on the list is the same lie in a smaller
    // font — the user would stop looking for the one that is really running.
    pausedAll({ a: t("a", { bg_now: [job({ running: false, exit: "exit 0" })] }) });
    render(<Home />);
    expect(rows()).toEqual([]);
  });

  it("stays out of the way when nothing is paused", () => {
    set({ tasks: { a: t("a", { bg_now: [job()] }) }, settings: { paused_all: false, paused_reason: "" } as unknown as Settings });
    render(<Home />);
    const html = document.body.textContent!;
    expect(html).not.toContain("Everything is paused");
    expect(html, "no pause, so no pause section and nothing to explain").not.toContain("npm run dev");
  });

  it("stops a job from the list", async () => {
    pausedAll({ a: t("a", { bg_now: [job()] }) });
    render(<Home />);
    await act(async () => { document.querySelector<HTMLElement>('[aria-label="Stop npm run dev"]')!.click(); });
    expect(api.bgKill).toHaveBeenCalledWith("a", "bg1");
  });
});
