// @vitest-environment jsdom
// The per-chat Resume controls must never dispatch the global resume.
//
// The field bug: with "Pause all" on, opening a frozen chat and hitting Resume
// woke *every* chat. The per-chat controls called `resume_all` whenever the
// chat had no pause of its own — which is exactly the shape of a chat frozen
// only by the global flag — and `resume_all` drops the flag and resumes
// everything. So the one action a user takes to unstick *one* chat was the one
// action that released all of them.
//
// `task_resume` opts a single chat out of the global pause; clearing the flag
// stays the exclusive business of the controls that say "all".
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type Settings, type TaskSummary } from "../api";
import { set } from "../store";
import { Paused } from "./Paused";

const t = (id: string, extra: Partial<TaskSummary> = {}): TaskSummary => ({
  id, title: `chat ${id}`, status: "running", paused: null, unpaused: false, busy: 0, model: "anthropic/claude-opus-5",
  branch: "main", project: "D:/work/app", updated_at: "2026-09-26T00:00:00Z", ...extra,
} as TaskSummary);

/** Everything frozen by the global flag, so every row is the risky shape. */
const allPaused = () => {
  set({
    tasks: { a: t("a"), b: t("b") },
    settings: { paused_all: true, paused_reason: "Paused everything" } as unknown as Settings,
  });
};

describe("per-chat resume under a global pause", () => {
  beforeEach(() => {
    (globalThis as unknown as { window: { setTimeout: typeof setTimeout } }).window ??= { setTimeout };
    vi.spyOn(api, "resume").mockResolvedValue(undefined);
    vi.spyOn(api, "resumeAll").mockResolvedValue(undefined);
  });
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  it("lifts one row without touching the global resume", async () => {
    allPaused();
    render(<Paused />);
    await act(async () => {
      document.querySelector<HTMLElement>('[aria-label="Resume chat a"]')!.click();
    });
    expect(api.resume, "the per-chat command is what opts one chat out").toHaveBeenCalledWith("a");
    expect(api.resumeAll, "nothing here may release every other chat").not.toHaveBeenCalled();
  });

  it("still offers Resume all as the only way to lift everything", async () => {
    allPaused();
    render(<Paused />);
    // The button is there; it is the one that clears the global flag.
    const all = [...document.querySelectorAll("button")].find((b) => b.textContent?.includes("Resume all"));
    expect(all).toBeTruthy();
    await act(async () => { all!.click(); });
    expect(api.resumeAll).toHaveBeenCalled();
  });
});

/**
 * "Dismiss all" is the other half of "Resume all", and it was missing.
 *
 * A screen full of frozen chats is usually a screen full of chats the user no
 * longer wants, and without it that meant pressing the trash on every row in
 * turn. It is deliberately the same shape as the per-row discard — it *arms*
 * rather than fires, because the frozen work is cancelled and there is no way
 * back into the frozen state.
 */
describe("dismissing from the paused list", () => {
  beforeEach(() => {
    (globalThis as unknown as { window: { setTimeout: typeof setTimeout } }).window ??= { setTimeout };
    vi.spyOn(api, "tasksDismissPause").mockResolvedValue(2);
    vi.spyOn(api, "resumeAll").mockResolvedValue(undefined);
  });
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  const armButton = () => document.querySelector<HTMLElement>('[aria-label="Dismiss all"]')!;
  const confirmButton = () => document.querySelector<HTMLElement>('[aria-label="Confirm dismissing"]')!;

  it("arms on the first press and discards every chat on the second", async () => {
    allPaused();
    render(<Paused />);

    await act(async () => { armButton().click(); });
    expect(api.tasksDismissPause, "arming must not throw work away on its own").not.toHaveBeenCalled();
    expect(confirmButton(), "the second press is what discards").toBeTruthy();

    await act(async () => { confirmButton().click(); });
    expect(api.tasksDismissPause).toHaveBeenCalledWith(["a", "b"]);
    // Never the resume path: this is the control that stops work.
    expect(api.resumeAll).not.toHaveBeenCalled();
  });

  it("discards only the ticked chats when some are ticked", async () => {
    allPaused();
    render(<Paused />);
    // Tick one row: the tickbox is aria-hidden, so it is found by class.
    await act(async () => { document.querySelectorAll<HTMLElement>(".pickbox")[0]!.click(); });
    await act(async () => { armButton().click(); });
    await act(async () => { confirmButton().click(); });
    expect(api.tasksDismissPause, "a selection narrows the blast radius").toHaveBeenCalledWith(["a"]);
  });
});
