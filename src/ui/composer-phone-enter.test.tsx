// @vitest-environment jsdom
// Enter in a phone browser, where the keyboard is the on-screen one.
//
// A phone's on-screen Return key cannot be relied on to say whether Shift is
// held, so Enter-to-send there has no way to ask for a newline: every line
// break someone typed on purpose comes out as a sent message, and the send
// button — the one control that is unambiguous — is what they should have been
// using. `phone_enter_sends` is the setting that decides, and off is the
// default: Enter starts a new line, and only the button sends. On, Enter sends
// and Shift Enter is the newline.
//
// These tests hold down the default: a settings.json written before the switch
// existed, or one whose owner never touched it, must not send on Enter. They
// also hold the window still, because `send_with` is what governs there and the
// phone switch must not leak into it.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, createEvent, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type Settings, type TaskSummary } from "../api";
import { set } from "../store";
import { Composer } from "./Composer";

/** A chat that is not running, with a draft in the box, so the composer shows
 *  the send button rather than a pause, stop or resume control. */
const chat = () => ({
  id: "t1", title: "chat", titled: false, status: "idle", step: "", project: ".", cwd: ".", branch: "", base_branch: "",
  worktree: false, model: "anthropic/claude-opus-5", effort: 2, perm: "auto", plan: false, ultra: false,
  ultra_wt: false, ultra_x: null, subagents: true, goal: null, assist: "default", agents: [],
  pending: {}, paused: null, serving: "", route: "", unpaused: false, busy: 0, bg_now: [], archived: false, hidden: false,
  unread: false,
  forked_from: null, pinned: false, artifict_open: false, order: 0, todos: [], subs: [],
  usage: { input: 0, output: 0, cache_read: 0, cache_write: 0, cost: 0, last_context: 0 },
  context_window: 200_000, created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-01T00:00:00Z",
  touched_at: "2026-09-01T00:00:00Z", waiting_kind: null, rev: 1,
} as TaskSummary);

const mount = (o: { phone?: boolean; send_with?: "enter" | "ctrl-enter"; phone_enter_sends?: boolean } = {}) => {
  // `shim.js` is what a paired browser publishes; `isPhoneClient` is that plus a
  // coarse pointer, which the `matchMedia` stub below provides.
  window.__OPENLEASH_REMOTE__ = o.phone ? { remote: true, host: "workstation" } : undefined;
  set({
    view: "session", task: "t1", tasks: { t1: chat() }, sessionDrafts: { t1: "hello there" }, notes: {},
    attach: {}, attachFiles: {}, pendingMessages: {},
    settings: { send_with: o.send_with ?? "enter", phone_enter_sends: o.phone_enter_sends } as unknown as Settings,
  });
  render(<Composer mode="session" />);
};

/** The new-task composer on a phone. The project is set so that a send that
 *  should not have happened has somewhere to go and is visible as an `api.create`
 *  call rather than being swallowed by the "pick a project folder first" guard. */
const mountHome = (phone_enter_sends?: boolean) => {
  window.__OPENLEASH_REMOTE__ = { remote: true, host: "workstation" };
  set({
    view: "home", task: null, draft: "hello there", notes: {}, attach: {}, attachFiles: {},
    settings: { project: "D:/work/app", send_with: "enter", phone_enter_sends } as unknown as Settings,
  });
  render(<Composer mode="home" />);
};

/** Press a key in the box and report what the browser does with it: `true` when
 *  the default was left alone (a line break is going in), `false` when the app
 *  took the key for itself. This is the assertion that matters — a composer
 *  that "does not send" by eating the key would strand the user on one line. */
const press = (init: KeyboardEventInit) => {
  const box = document.querySelector(".composer textarea") as HTMLTextAreaElement;
  const event = createEvent.keyDown(box, init);
  fireEvent(box, event);
  return !event.defaultPrevented;
};
const enter = (shift = false) => press({ key: "Enter", shiftKey: shift });

describe("Enter in a phone browser", () => {
  beforeEach(() => {
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: true, addEventListener() {}, removeEventListener() {} })));
    vi.spyOn(api, "send").mockResolvedValue(undefined);
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    window.__OPENLEASH_REMOTE__ = undefined;
  });

  it("starts a new line instead of sending when the setting is off", () => {
    mount({ phone: true });
    expect(enter()).toBe(true);
    expect(api.send, "a newline is not a send").not.toHaveBeenCalled();
    // The tooltip has to agree with the key: on a phone with no key sending,
    // "Send · Enter" would teach the opposite of what Enter does.
    expect(screen.getByRole("button", { name: "Send" })).toBeTruthy();
  });

  it("starts a new line on Shift Enter with the setting off", () => {
    mount({ phone: true });
    expect(enter(true)).toBe(true);
    expect(api.send).not.toHaveBeenCalled();
  });

  it("sends on Enter and starts a new line on Shift Enter once it is on", () => {
    mount({ phone: true, phone_enter_sends: true });
    expect(enter()).toBe(false);
    expect(api.send).toHaveBeenCalledWith("t1", "hello there", false, undefined, [], []);
    expect(screen.getByRole("button", { name: "Send · Enter" })).toBeTruthy();
  });

  it("keeps Shift Enter as the newline, so a long message can still be typed", () => {
    mount({ phone: true, phone_enter_sends: true });
    expect(enter(true)).toBe(true);
    expect(api.send).not.toHaveBeenCalled();
  });

  it("leaves the window's own Enter rule alone, whichever way the phone switch is set", () => {
    mount({ send_with: "enter", phone_enter_sends: true });
    expect(enter()).toBe(false);
    expect(api.send).toHaveBeenCalledWith("t1", "hello there", false, undefined, [], []);
  });

  it("still sends on Ctrl Enter in the window when Send message with says so", () => {
    mount({ send_with: "ctrl-enter" });
    expect(enter(), "plain Enter is the newline in this mode").toBe(true);
    expect(api.send).not.toHaveBeenCalled();
    expect(press({ key: "Enter", ctrlKey: true })).toBe(false);
    expect(api.send).toHaveBeenCalledWith("t1", "hello there", false, undefined, [], []);
  });

  it("holds the new-task composer to the same rule, where a send would start a run", () => {
    vi.spyOn(api, "create");
    mountHome();
    expect(enter()).toBe(true);
    expect(enter(true)).toBe(true);
    expect(api.create, "Enter starts the line, the button starts the task").not.toHaveBeenCalled();
  });
});
