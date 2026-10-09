/// <reference types="node" />
// @vitest-environment jsdom
// Two bugs with one shape: the UI said "paused" about work that was still running,
// and said nothing at all about a button the user had just pressed.
//
// 1. The Pause all chip. `pause_all` walks every chat, resets `unpaused`, saves each
//    to disk and recounts in flight commands before it flips the flag — seconds on a
//    machine with a few dozen chats, all of it after the IPC round trip. Nothing
//    local moved for any of it, so the chip sat there unchanged and fully enabled:
//    pressing it twice ran the whole sweep twice, and the press that did land gave
//    no sign it had been heard.
//
// 2. The tool row's glyph. A pause blocks at `wait_unpaused` at the *top* of the
//    bash handler, so a command that already started runs to completion whatever
//    the user does next. The banner counts exactly those ("waiting on N commands to
//    finish"), and the row drew the still frozen mark over them anyway — a live
//    `cargo build` under a row labelled paused, which reads as the pause having
//    broken it. The backend now marks the row `exec` while the process is alive.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { act, cleanup, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { invoke } from "@tauri-apps/api/core";
import type { Item, Settings, TaskSummary } from "../../api";
import { set } from "../../store";
import { FeedRows, ToolItem } from "../Session";
import { Home } from "../Home";

const chat = (extra: Partial<TaskSummary> = {}): TaskSummary => ({
  id: "t1", title: "chat one", status: "running", paused: null, unpaused: false, busy: 0,
  model: "anthropic/claude-opus-5", branch: "main", project: "D:/work/app",
  updated_at: "2026-09-26T00:00:00Z", touched_at: "2026-09-26T00:00:00Z",
  ...extra,
} as TaskSummary);

/** A `bash` row as the backend writes it: `exec` is set only while the process lives. */
const toolRow = (exec?: boolean): Item =>
  ({ id: "r1", kind: "tool", text: "Running cargo build", ts: "2026-09-26T00:00:00Z",
     data: { name: "bash", status: "running", input: { command: "cargo build" }, ...(exec === undefined ? {} : { exec }) } }) as Item;

const pausedChat = (extra: Partial<TaskSummary> = {}) =>
  chat({ paused: { reason: "Paused", kind: "manual", since: "2026-09-26T00:00:00Z" }, busy: 1, ...extra });

/** A frozen chat holding exactly one command, mid-flight. */
const frozen = (extra: Partial<TaskSummary> = {}) => {
  set({ tasks: { t1: pausedChat(extra) }, items: { t1: [] },
        settings: { project: "D:/work/app", paused_all: true, paused_reason: "Paused everything" } as unknown as Settings });
};

/** Calls are folded into a group line by default, so this is the one the user reads. */
const feed = (items: Item[]) =>
  renderToStaticMarkup(createElement(FeedRows, { items, task: pausedChat(), showThinking: false }));

/** The group says work is under way with a shimmer; frozen, it goes still. */
const claimsWork = (items: Item[]) => feed(items).includes("tg-now");

beforeEach(() => {
  (globalThis as any).window ??= { setTimeout: (f: () => void, ms: number) => setTimeout(f, ms) };
  // The home rows' `MorphText` reads the reduced-motion query on mount and asks
  // for live animations when its text changes or it is torn down. jsdom has none
  // of these, and the pause chip is one of the things it morphs.
  (window as any).matchMedia ??= () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  (Element.prototype as any).getAnimations ??= () => [];
  (Element.prototype as any).animate ??= () => ({ cancel() {}, finish() {}, addEventListener() {}, removeEventListener() {}, finished: Promise.resolve() });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("a command that is still executing keeps its live glyph", () => {
  it("does not freeze a command the backend says is running", () => {
    // The exact shape of the complaint: the chat is paused, the banner says
    // "waiting on 1 command to finish", and the row said paused anyway.
    frozen();
    expect(claimsWork([toolRow(true)]), "a live process is not a frozen agent").toBe(true);
  });

  it("still freezes the turn itself, which is what a pause actually stops", () => {
    // The other half, and the reason this is not "always animate when paused". A
    // pause blocks between calls, so a call waiting for its turn to be resumed has
    // nothing running behind it — the shimmer would be a claim nothing backs.
    frozen();
    expect(claimsWork([toolRow(false)]), "a frozen turn is not live work").toBe(false);
    // And an unmarked row — anything the backend did not tag — freezes as before.
    expect(claimsWork([toolRow()]), "an untagged row keeps the old frozen glyph").toBe(false);
  });

  it("agrees between the folded summary line and the row behind it", () => {
    // The group is folded by default, so its summary line is what the user reads
    // first, and it keys off the same `held`. It had the same blind spot, so both
    // have to be pinned: a live command reads the same either way.
    frozen();
    expect(feed([toolRow(true)]), "the folded summary shows work under way").toContain("tg-now");
    // And the row's own glyph, which only mounts once the fold is opened.
    const row = renderToStaticMarkup(createElement(ToolItem, { it: toolRow(true), cwd: "", held: true }));
    expect(row, "and so does the row it folds").toContain("ld-trace-spin");
  });
});

describe("Pause all acknowledges the press", () => {
  const button = () => document.querySelector<HTMLButtonElement>('button[aria-label="Pause all agents"]');
  const home = () => {
    set({ tasks: { t1: chat() }, settings: { project: "D:/work/app", paused_all: false } as unknown as Settings });
    render(<Home />);
  };

  it("changes and locks the moment it is pressed, before the backend answers", async () => {
    // A command that never settles, so nothing but the press itself can be observed.
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(() => new Promise(() => {}));
    home();
    await act(async () => { button()!.click(); });

    expect(document.body.textContent, "the press has to be visible in the same tick").toContain("Pausing…");
    const b = button()!;
    expect(b.disabled, "a second press would run the whole sweep again").toBe(true);
  });

  it("hands the button back when the pause lands, and when it fails", async () => {
    let refuse = true;
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(() => refuse ? Promise.reject(new Error("no")) : Promise.resolve(null));
    home();
    await act(async () => { button()!.click(); });
    // A refused pause must not leave a chip stuck mid-press with nothing behind it.
    expect(button()!.disabled).toBe(false);
    expect(document.body.textContent).toContain("Pause all");

    // Now let one through, and let the flag come back the way the backend sends it.
    refuse = false;
    await act(async () => { button()!.click(); });
    expect(button()!.disabled, "held until the flag lands, not for one frame").toBe(true);
    await act(async () => {
      set((s) => ({ settings: { ...s.settings!, paused_all: true } as Settings }));
    });
    expect(button(), "the flag is on, so the chip is gone entirely").toBeNull();
  });
});