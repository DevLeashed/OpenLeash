// @vitest-environment jsdom
// Messaging one subagent from its own panel, on a phone.
//
// The subagent's box is the app's second place a message is typed, and the one
// that was left behind when the composer learned about phones: it sent on a bare
// `Enter`, whatever the device. On a phone that is the one key that cannot be
// trusted — the on-screen Return key does not report a held `Shift`, so there is
// no way to ask for a newline, and every line break someone typed on purpose
// went out as a message to an agent that was reading it as an instruction.
//
// These tests hold it to the same rule as the composer (`messageSendKey`): a
// phone starts with no key sending and the button doing it, `phone_enter_sends`
// turns Enter-to-send on there, and the window keeps obeying *Send message with*.
// They also hold the two ways the key can lie: an `Enter` that is part of
// composing a character, and the button's own label, which must not promise a
// key that does not send.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, createEvent, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type Settings, type SubInfo, type TaskSummary } from "../api";
import { set } from "../store";
import { SubInput } from "./Session";

const chat = () => ({ id: "t1", title: "chat", subs: [] } as unknown as TaskSummary);
const runningSub = {
  id: "s1", role: "explore", task: "look around", status: "running", meta: "",
  model: "", serving: "", started: null, report: "", background: false,
} as SubInfo;

/** `shim.js` is what a paired browser publishes; `isPhoneClient` is that plus a
 *  coarse pointer, which the `matchMedia` stub below provides. The draft is
 *  typed into the box, because this one is local state rather than a store draft. */
const mount = (o: { phone?: boolean; send_with?: "enter" | "ctrl-enter"; phone_enter_sends?: boolean } = {}) => {
  window.__OPENLEASH_REMOTE__ = o.phone ? { remote: true, host: "workstation" } : undefined;
  set({ settings: { send_with: o.send_with ?? "enter", phone_enter_sends: o.phone_enter_sends } as unknown as Settings });
  render(<SubInput task={chat()} sub={runningSub} />);
  const box = document.querySelector(".subinput textarea") as HTMLTextAreaElement;
  fireEvent.change(box, { target: { value: "hello there" } });
  return box;
};

/** Press a key in the box and report what the browser does with it: `true` when
 *  the default was left alone (a line break is going in), `false` when the app
 *  took the key for itself. A box that "does not send" by eating the key would
 *  strand the user on one line, so this is the assertion that matters. */
const press = (box: HTMLTextAreaElement, init: KeyboardEventInit) => {
  const event = createEvent.keyDown(box, init);
  fireEvent(box, event);
  return !event.defaultPrevented;
};
const enter = (box: HTMLTextAreaElement, shift = false) => press(box, { key: "Enter", shiftKey: shift });

describe("messaging a subagent from a phone", () => {
  beforeEach(() => {
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: true, addEventListener() {}, removeEventListener() {} })));
    vi.spyOn(api, "subSend").mockResolvedValue(undefined);
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    window.__OPENLEASH_REMOTE__ = undefined;
  });

  it("starts a new line instead of sending when the setting is off", () => {
    const box = mount({ phone: true });
    expect(enter(box), "the newline goes in").toBe(true);
    expect(api.subSend, "a newline is not a message to an agent").not.toHaveBeenCalled();
  });

  it("sends on Enter and starts a new line on Shift Enter once it is on", () => {
    const box = mount({ phone: true, phone_enter_sends: true });
    expect(enter(box)).toBe(false);
    expect(api.subSend).toHaveBeenCalledWith("t1", "s1", "hello there", [], []);
    expect(enter(box, true), "Shift Enter is the newline").toBe(true);
  });

  it("leaves the button as the way to send when no key does", () => {
    mount({ phone: true });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(api.subSend).toHaveBeenCalledWith("t1", "s1", "hello there", [], []);
  });

  it("does not name a key the phone does not have", () => {
    mount({ phone: true });
    expect(screen.getByRole("button", { name: "Send" })).toBeTruthy();
    cleanup();
    mount({ phone: true, phone_enter_sends: true });
    expect(screen.getByRole("button", { name: "Send · Enter" })).toBeTruthy();
  });

  it("keeps the window's own rule, which the phone switch must not leak into", () => {
    const box = mount({});
    expect(enter(box)).toBe(false);
    expect(api.subSend).toHaveBeenCalledWith("t1", "s1", "hello there", [], []);
  });

  it("obeys Send message with in the window, so Ctrl Enter sends there too", () => {
    const box = mount({ send_with: "ctrl-enter" });
    expect(enter(box), "plain Enter is the newline in this mode").toBe(true);
    expect(api.subSend).not.toHaveBeenCalled();
    expect(press(box, { key: "Enter", ctrlKey: true })).toBe(false);
    expect(api.subSend).toHaveBeenCalledWith("t1", "s1", "hello there", [], []);
  });

  it("does not send an Enter that is still composing a character", () => {
    const box = mount({ phone: true, phone_enter_sends: true });
    expect(press(box, { key: "Enter", isComposing: true })).toBe(true);
    expect(api.subSend).not.toHaveBeenCalled();
  });
});
