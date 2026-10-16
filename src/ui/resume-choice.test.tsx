// @vitest-environment jsdom
// The resume menu after a stop offers three choices, and the middle one is easy
// to get wrong.
//
// "Wrap up" and "Continue" are two ends of one decision — carry on, or don't.
// "Summarise and continue" is the case in between: the user wants the status
// report *and* the work. It is not a softer "Wrap up": it has to reach the
// backend as its own mode, because the backend drives subagent resumption and
// the agent's instruction off that exact string. Sending "wrap" for it would
// report and stop; sending "continue" would carry on with no report at all.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type TaskSummary } from "../api";
import { set } from "../store";
import { Composer } from "./Composer";

/** A chat that was stopped, with nothing typed, so the composer shows the
 *  resume choice rather than the send button. */
const stopped = (extra: Partial<TaskSummary> = {}) => ({
  id: "t1", title: "chat", status: "stopped", step: "", project: ".", cwd: ".", branch: "", base_branch: "",
  worktree: false, model: "anthropic/claude-opus-5", effort: 2, perm: "auto", plan: false, ultra: false,
  ultra_wt: false, ultra_x: null, subagents: true, goal: null, assist: "default", agents: [],
  pending: {}, paused: null, serving: "", route: "", unpaused: false, busy: 0, archived: false, hidden: false,
  forked_from: null, pinned: false, order: 0, todos: [], subs: [],
  usage: { input: 0, output: 0, cache_read: 0, cache_write: 0, cost: 0, last_context: 0 },
  context_window: 200_000, created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-01T00:00:00Z",
  touched_at: "2026-09-01T00:00:00Z", waiting_kind: null, ...extra,
} as TaskSummary);

const mount = () => {
  set({
    view: "session", task: "t1", tasks: { t1: stopped() }, sessionDrafts: {}, notes: {},
    attach: {}, attachFiles: {}, settings: null,
  });
  render(<Composer mode="session" />);
};

/** Open the menu and click the row whose headline is `label`. */
const choose = async (label: string) => {
  await act(async () => { screen.getByRole("button", { name: /^Resume ·/ }).click(); });
  const row = [...document.querySelectorAll(".umenu .mrow")]
    .find((r) => r.textContent?.startsWith(label));
  expect(row, `the menu offers ${label}`).toBeTruthy();
  await act(async () => { (row as HTMLElement).click(); });
};

describe("the resume choice after a stop", () => {
  beforeEach(() => { vi.spyOn(api, "resume").mockResolvedValue(undefined); });
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  it("offers summarise and continue between the two existing choices", async () => {
    mount();
    await act(async () => { screen.getByRole("button", { name: /^Resume ·/ }).click(); });
    const heads = [...document.querySelectorAll(".umenu .mrow")].map((r) => r.querySelector("span")?.textContent);
    expect(heads).toEqual(["Wrap up", "Summarise and continue", "Continue"]);
  });

  it("sends its own mode, not one of the two it sits between", async () => {
    mount();
    await choose("Summarise and continue");
    expect(api.resume).toHaveBeenCalledWith("t1", undefined, "summarise");
  });

  it("still sends wrap and continue as themselves", async () => {
    mount();
    await choose("Wrap up");
    expect(api.resume).toHaveBeenLastCalledWith("t1", undefined, "wrap");
  });
});
