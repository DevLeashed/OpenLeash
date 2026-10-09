// @vitest-environment jsdom
// A frozen chat must look frozen, everywhere, the moment it is frozen.
//
// The field bug: paused chats appeared blue until you pressed pause on them
// directly, and only then turned yellow. Two causes, and they are independent:
//
//  1. A chat frozen by "Pause all" carries no `paused` of its own — the global
//     flag is what holds it. So `swarmState()` asked the task alone, said
//     "live", and the sidebar dot took the running blue, the header underline
//     kept breathing, and the badge counted agents that were not running. The
//     fix is `swarmState(task, heldByGlobal(task, pausedAll))`: the swarm now
//     gets the same global verdict the rest of the app already used.
//
//  2. The pause you pressed emitted an attention event, which marked the chat
//     unread. The unread dot outranks the status colour, so the row turned the
//     unread blue *after* the pause. The dot is cleared by opening the chat, and
//     a manual pause/stop is not news — `describe()` already said so for the
//     notice and the OS toast, and now the dot follows the same rule.
//
// Both are pinned here, because both present as the same symptom: blue, then
// yellow once the user pressed again.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { act, cleanup, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Settings, SubInfo, TaskSummary } from "../api";
import { boot, disposeBoot, get, set } from "../store";
import { Sidebar } from "./Chrome";
import { Home } from "./Home";
import { Timeline } from "./Session";
import { heldByGlobal, isPaused } from "./Paused";
import { swarmGlow, swarmState } from "./session/Swarm";

const chat = (extra: Partial<TaskSummary> = {}): TaskSummary => ({
  id: "t1", title: "chat one", status: "running", paused: null, unpaused: false, busy: 0,
  model: "anthropic/claude-opus-5", branch: "main", project: "D:/work/app",
  ultra: true, subs: [], archived: false,
  updated_at: "2026-09-26T00:00:00Z", touched_at: "2026-09-26T00:00:00Z",
  ...extra,
} as TaskSummary);

const frozen = (extra: Partial<TaskSummary> = {}) =>
  set({
    tasks: { t1: chat(extra) },
    // The home list only shows chats of the open folder, so the project has to
    // be the chat's own or there is no row to look at.
    settings: { project: "D:/work/app", paused_all: true, paused_reason: "Paused everything" } as unknown as Settings,
  });

/** The dot's colour, as the sidebar wrote it into the row. */
const dotColor = () => document.querySelector<HTMLElement>(".srow .dot")?.style.background ?? "";

beforeEach(() => {
  (globalThis as any).window ??= { setTimeout: (f: () => void, ms: number) => setTimeout(f, ms) };
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("a chat frozen by Pause all reads as paused, not live", () => {
  it("the swarm is not live — which is what made the row blue", () => {
    // The exact shape the bug had: still `running`, no pause of its own, held
    // only by the global flag.
    const t = chat();
    expect(t.paused, "nothing about the task itself says frozen").toBeNull();
    expect(isPaused(t, true), "only the global flag freezes it").toBe(true);
    expect(heldByGlobal(t, true)).toBe(true);
    // This is the assertion the bug turns on: asked about the task alone, the
    // swarm still called itself live and painted the row running-blue.
    expect(swarmState(t)).toBe("live");
    expect(swarmState(t, heldByGlobal(t, true)), "given the global verdict it is paused").toBe("paused");
    expect(swarmGlow(swarmState(t, heldByGlobal(t, true))), "a frozen swarm must not glow").toBe(false);
  });

  it("gives the sidebar dot the paused colour, not the running one", () => {
    frozen();
    render(createElement(Sidebar));
    expect(dotColor(), "the row's own freeze verdict picks the colour").toBe("var(--st-pause)");
    // The running blue is the thing being reported; nothing may be left of it.
    expect(dotColor()).not.toBe("var(--st-run)");
    expect(dotColor()).not.toBe("var(--st-unread)");
  });

  it("leaves a chat the user opted out of on its own status", () => {
    // `unpaused` is what resuming one chat under "Pause all" sets. That chat is
    // running again and must look it — the fix is not "global flag means paused".
    frozen({ unpaused: true });
    render(createElement(Sidebar));
    expect(dotColor()).not.toBe("var(--st-pause)");
  });

  it("gives the Home list the same answer the sidebar gives", () => {
    // The home row and the icon beside it used to ask two different questions:
    // the label said "Paused" from the global flag, the icon said "running"
    // because it only ever looked at `t.paused`. One verdict, both.
    frozen();
    const html = renderToStaticMarkup(createElement(Home));
    expect(html).toContain("Paused");
    // The pause glyph is `lucide-pause`; a running chat draws the spinner instead.
    expect(html).not.toContain("lucide-loader");
  });
});

/**
 * A frozen chat must not *animate*. The spinner and the shimmer are driven by a
 * running turn; over a pause they keep moving, and a user opening a paused chat
 * sees a chat that looks mid-request with nothing behind it. The squircle is
 * still, so the claim it makes is one the app can keep.
 */
describe("a frozen chat stops animating", () => {
  const session = (extra: Partial<TaskSummary> = {}) => {
    set({
      tasks: { t1: chat({ subs: [{ id: "s1", role: "explore", task: "sweep", status: "running", meta: "reading", report: "", model: "anthropic/claude-opus-5", serving: "", started: null, background: false } as SubInfo], ...extra }) },
      settings: { paused_all: true, paused_reason: "Paused everything" } as unknown as Settings,
      view: "session", task: "t1", items: { t1: [] },
    });
  };

  it("draws the still mark instead of the agent spinner", () => {
    session();
    const html = renderToStaticMarkup(createElement(Timeline, { task: chat() }));
    expect(html, "no trace of the animated agent glyph in a frozen chat").not.toContain("ld-trace-spin");
  });

  it("still animates a chat that opted out and is really running", () => {
    // The other half: `unpaused` is what resuming one chat under "Pause all"
    // sets. That chat is running again, and freezing the glyph for it would be
    // the same bug pointed the other way.
    set({ tasks: { t1: chat({ unpaused: true }) }, settings: {} as unknown as Settings, view: "session", task: "t1", items: { t1: [] } });
    const html = renderToStaticMarkup(createElement(Timeline, { task: chat({ unpaused: true }) }));
    expect(html, "a chat that really is working still animates").toContain("ld-trace-spin");
  });
});

describe("pausing a chat yourself does not mark it unread", () => {
  /** Drive a real `ol://attention` event, the way the backend sends one. */
  const bootWith = async (t: TaskSummary) => {
    (listen as unknown as ReturnType<typeof vi.fn>).mockClear();
    (listen as unknown as ReturnType<typeof vi.fn>).mockImplementation(async (name: string, cb: any) => {
      (listen as any).handlers = { ...((listen as any).handlers ?? {}), [name]: cb };
      return () => { delete (listen as any).handlers[name]; };
    });
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(async (cmd: string, args: any) => {
      if (cmd === "app_boot") return { settings: {}, tasks: [t], project: "" };
      if (cmd === "settings_update") return { ...args.patch };
      return null;
    });
    await boot();
  };
  const attention = (payload: Record<string, unknown>) =>
    (listen as any).handlers["ol://attention"]({ payload: { task_id: "t1", ...payload } });

  beforeEach(() => { set({ view: "home", task: null, unread: {}, notices: [] }); });
  afterEach(() => disposeBoot());

  it("leaves the dot off for a pause you pressed", async () => {
    // The user is standing in the chat pressing pause. `describe()` calls a
    // manual pause not-news for the notice and the OS toast; the dot is the
    // third copy of that same report and it has to agree, or the row they just
    // paused lights up the unread blue and stays that way.
    await bootWith(chat());
    // Not viewing this chat, so nothing else would clear it.
    set({ view: "home", task: null });
    await act(async () => { attention({ kind: "paused", pause: "manual", reason: "Paused by you" }); });
    expect(get().unread.t1, "you already read this — you did it").toBeFalsy();
    expect(get().notices, "and it raises no notice either").toHaveLength(0);
  });

  it("still marks it for a pause the app raised on its own", () => {
    // The other half: an exhausted route is genuinely news. Suppressing only
    // the user's own keystrokes must not silence the agent asking for help.
    return bootWith(chat()).then(() => {
      set({ view: "home", task: null });
      attention({ kind: "paused", pause: "exhausted", reason: "Every model in the chain failed" });
      expect(get().unread.t1, "a chat the agent stopped itself is news").toBe(true);
    });
  });

  it("does not take the dot for a chat the user is reading", async () => {
    await bootWith(chat());
    set({ view: "session", task: "t1" });
    await act(async () => { attention({ kind: "done" }); });
    expect(get().unread.t1).toBeFalsy();
  });
});
