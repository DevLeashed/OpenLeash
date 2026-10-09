// @vitest-environment jsdom
/// <reference types="node" />
// The sidebar dot promises to say which model a chat is on: it takes the
// provider's brand colour (`modelColor`), which is the whole reason swapping a
// model in a running chat is legible at a glance — the dot moves with the pick.
//
// Two things stopped it doing that, and both were invisible in the field:
//
//  1. A live swarm outranked it. `ultraDot` was asked first, so an ultrathread
//     chat *while its agents were working* — which is most of its life — painted
//     the mode's violet and never showed the model at all. A chat on Codex
//     (`#10a37f`, green) sat there violet after the swap, and there was no
//     colour left on the row that could say what it was running on.
//
//  2. It read `t.model` rather than `chatModel(t)`. A swap made from the model
//     picker *during* a turn lands in `pending` and waits for the next message,
//     which is what the composer's `*` badge promises; the dot kept painting the
//     outgoing model. `chatModel` already existed for this and every other
//     surface that names a chat's model used it.
//
// The swarm colour is gone from the dot entirely, live or settled — the mode is
// still on the row in its badge and the header, and taking the dot for it is
// what hid the model. Nothing here moves the dot off the status colours: a
// frozen chat is still paused-yellow (see pause-color.test.tsx), and unread still
// wins unless the chat is waiting for a blocking answer — that wait itself is
// what the red status colour marks.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { cleanup, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import type { ProviderView, Settings, SubInfo, TaskSummary } from "../api";
import { chatModel, modelColor, set } from "../store";
import { Sidebar } from "./Chrome";

/** Codex's brand colour, from `providers::PROVIDERS` — the green the swap was
 *  meant to turn the dot into. */
const CODEX = "#10a37f";
const ANTHROPIC = "#e8967a";

/** jsdom rewrites a hex in `style.background` to `rgb()`, so the expectation has
 *  to be written the way the DOM reports it or it matches nothing. */
const asDot = (hex: string) =>
  `rgb(${parseInt(hex.slice(1, 3), 16)}, ${parseInt(hex.slice(3, 5), 16)}, ${parseInt(hex.slice(5, 7), 16)})`;

const prov = (id: string, color: string): ProviderView => ({ id, color }) as ProviderView;

const sub = (extra: Partial<SubInfo> = {}): SubInfo =>
  ({ id: "s1", role: "general", task: "build", status: "running", meta: "editing", model: "", serving: "", started: null, report: "", background: false, depth: 1, ...extra }) as SubInfo;

const chat = (extra: Partial<TaskSummary> = {}): TaskSummary => ({
  id: "t1", title: "jelly", status: "running", paused: null, unpaused: false, busy: 0,
  model: "codex/gpt-6-luna", pending: {}, branch: "main", project: "D:/work/app",
  ultra: true, subs: [], archived: false,
  updated_at: "2026-09-26T00:00:00Z", touched_at: "2026-09-26T00:00:00Z",
  ...extra,
} as TaskSummary);

/** One chat, in the project the sidebar is filtered to, with both providers
 *  connected so `modelColor` resolves a real brand colour. */
const listed = (t: TaskSummary) => set({
  tasks: { t1: t },
  providers: [prov("codex", CODEX), prov("anthropic", ANTHROPIC)],
  models: [],
  settings: { project: "D:/work/app" } as unknown as Settings,
});

/** The dot's colour, as the sidebar wrote it into the row. */
const dotColor = () => document.querySelector<HTMLElement>(".srow .dot")?.style.background ?? "";

beforeEach(() => {
  // The store is module-level, so an unread dot set by one case would outrank the
  // colour every later case is asserting on.
  set({ unread: {} });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("the sidebar dot follows the model a chat is on", () => {
  it("shows the model colour while an ultrathread swarm is live", () => {
    // The field bug, exactly as it was saved: an ultrathread chat mid-swarm,
    // whose dot read violet whatever it was running on.
    listed(chat({ ultra: true, subs: [sub()] }));
    render(createElement(Sidebar));
    // Live agents means "the model owns the dot": the user just picked this
    // model and the row has to be the thing that confirms it.
    expect(dotColor(), "the model's colour, not the mode's violet").toBe(asDot(CODEX));
    expect(dotColor()).not.toBe("var(--ultra)");
  });

  it("shows the model colour for an ordinary running chat too", () => {
    // The non-swarm half: this one worked, and the swarm branch is not allowed
    // to be the reason a colour only appears in some rows.
    listed(chat({ ultra: false, subs: [sub()] }));
    render(createElement(Sidebar));
    expect(dotColor()).toBe(asDot(CODEX));
  });

  it("gives a settled swarm the same status colour any other row gets", () => {
    // Not a regression on the mode: it is still on the row, in the badge and the
    // header underline. But the dot has one job — naming what the chat is
    // running on — and a swarm that has stopped is spending nothing, so it reads
    // as stopped exactly as a non-ultrathread chat would. What it may not do is
    // hold the dot while agents are live, which is what hid the model.
    listed(chat({ ultra: true, status: "idle", subs: [sub({ status: "done" })] }));
    render(createElement(Sidebar));
    expect(dotColor(), "a settled swarm is not a mode-coloured dot").toBe("var(--st-idle)");
  });

  it("paints the model you are switching to, not the one being left", () => {
    // The picker defers a mid-turn swap to the next message, and says so with
    // the `*` badge. `t.model` is still the outgoing model at this point, so the
    // dot followed the outgoing one and named a model the chat was leaving.
    const t = chat({ model: "anthropic/claude-opus-5", pending: { model: "codex/gpt-6-luna" } });
    expect(chatModel(t), "what the dot is asked to paint").toBe("codex/gpt-6-luna");
    listed(t);
    render(createElement(Sidebar));
    expect(dotColor(), "the queued swap's destination is the model now").toBe(asDot(CODEX));
    expect(dotColor()).not.toBe(asDot(ANTHROPIC));
  });

  it("follows a swap that has already landed", () => {
    // The other half: once the swap applies, `pending` is empty and `model` is
    // the new one. Both paths have to land on the same colour, or the dot jumps
    // once on the turn boundary instead of when the user picked.
    listed(chat({ model: "codex/gpt-6-luna", pending: {} }));
    render(createElement(Sidebar));
    expect(dotColor()).toBe(asDot(modelColor("codex/gpt-6-luna")));
  });
});

describe("what the dot is not allowed to become", () => {
  it("still reads as paused, because a freeze is the more urgent fact", () => {
    // The colour this bug was reported against is not the one that outranks the
    // model: a frozen chat is paused-yellow whichever model it is on, and a
    // swarm that is held still does not glow (see pause-color.test.tsx).
    set({
      tasks: { t1: chat({ status: "running", paused: { reason: "Paused by you", kind: "manual", since: "2026-09-26T00:00:00Z" } }) },
      providers: [prov("codex", CODEX)],
      settings: { project: "D:/work/app" } as unknown as Settings,
    });
    render(createElement(Sidebar));
    expect(dotColor()).toBe("var(--st-pause)");
  });

  it("still lets an unread chat outrank the model colour", () => {
    // Unread is the other thing that has to survive: the dot is how you notice
    // a chat that raised its hand while you were somewhere else.
    set({ unread: { t1: true } });
    listed(chat({ ultra: true, subs: [sub()] }));
    render(createElement(Sidebar));
    expect(dotColor()).toBe("var(--st-unread)");
  });

  it.each(["question", "approval"] as const)("keeps an unanswered %s red instead of unread blue", (waiting_kind) => {
    // The wait itself is the ongoing attention marker. Unread blue used to mask
    // the red status color, even though this chat still needs an answer.
    listed(chat({ status: "waiting", waiting_kind }));
    set({ unread: { t1: true } });
    render(createElement(Sidebar));
    expect(dotColor()).toBe("var(--st-wait)");
  });

  it("leaves a chat that is not running on its status colour", () => {
    // The model colour is for a chat that is working. A finished one is not
    // spending anything, so it reads as finished — the same answer `statusOf`
    // gives everywhere else.
    listed(chat({ status: "done", ultra: false, subs: [] }));
    render(createElement(Sidebar));
    expect(dotColor()).toBe("var(--st-done)");
  });
});