// @vitest-environment jsdom
// The permanent delete of an archived race: Settings › Chats is the only screen
// that lists one, so the flow is exercised there.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null), save: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import type { Race, Settings, TaskSummary } from "../api";
import { get, set } from "../store";
import { getRaces, resetRaces, setRaces } from "../race";
import { SettingsView } from "./Settings";

const chat = (id: string, over: Record<string, unknown> = {}) =>
  ({ id, title: id, archived: false, pinned: false, status: "stopped", touched_at: "", updated_at: "", project: "D:/proj", cwd: "D:/proj", ...over }) as unknown as TaskSummary;

const race = (over: Partial<Race> = {}): Race => ({
  id: "r1", prompt: "Fix the flaky test", project: "D:/proj", base_branch: "main", created_at: "2026-01-01T00:00:00Z",
  attempts: [
    { task_id: "a", model: "m1", effort: 2, checks: [], checked: false },
    { task_id: "b", model: "m2", effort: 2, checks: [], checked: false },
  ],
  judge_model: "m1", judge_effort: 2, state: "judged", judge: null, winner: null, merged: null, error: null,
  archived: true, archived_chats: ["a", "b"], ...over,
});

const base = { paused_all: false } as Settings;

function chatsTab(settings: Settings = base, tasks: Record<string, TaskSummary> = {}) {
  set({ settings, settingsTab: "chats", tasks });
  return render(<SettingsView />);
}

/** The archived-race row, matched by its own title rather than by a button
 *  label: "Restore" and "Delete…" are used by the chat rows below too. */
function raceRow() {
  return [...document.querySelectorAll<HTMLElement>(".srowx")]
    .find((row) => row.textContent?.startsWith("Race · Fix the flaky test"))!;
}

beforeEach(() => {
  resetRaces();
  setRaces({ races: { r1: race() } });
});
afterEach(() => {
  cleanup();
  vi.mocked(invoke).mockReset();
  set({ toast: null });
});

describe("deleting an archived race for good", () => {
  it("offers a delete on an archived race and asks before it does anything", async () => {
    chatsTab();
    const row = raceRow();
    expect(row.textContent).toContain("2 attempts");
    fireEvent.click(within(row).getByRole("button", { name: "Delete…" }));
    // The first click only arms it: nothing is deleted until the confirm.
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === "race_delete")).toBe(false);
    await act(async () => { fireEvent.click(within(raceRow()).getByRole("button", { name: "Delete forever" })); });
    const dialog = document.querySelector('[role="dialog"]')!;
    expect(dialog.getAttribute("aria-label")).toBe("Delete this race for good?");
    expect(dialog.textContent).toContain("2 attempt chats");
    expect(dialog.textContent).toContain("cannot be undone");
  });

  it("deletes the race and its chats, and says what it did", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "race_delete"
      ? { id: "r1", deleted_chats: ["a"], kept_chats: [], kept_worktrees: 1 }
      : null));
    chatsTab(base, { a: chat("a", { archived: true }), b: chat("b", { archived: true }) });
    fireEvent.click(within(raceRow()).getByRole("button", { name: "Delete…" }));
    await act(async () => { fireEvent.click(within(raceRow()).getByRole("button", { name: "Delete forever" })); });
    await act(async () => { fireEvent.click(within(document.querySelector('[role="dialog"]')!).getByRole("button", { name: "Delete forever" })); });
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("race_delete", { id: "r1", removeWorktree: false });
    expect(getRaces().races.r1).toBeUndefined();
    expect(get().tasks.a).toBeUndefined();
    expect(get().toast).toContain("Race deleted");
  });

  it("asks the backend to remove the worktrees when that switch is on", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "race_delete"
      ? { id: "r1", deleted_chats: ["a", "b"], kept_chats: [], kept_worktrees: 0 }
      : null));
    chatsTab();
    fireEvent.click(within(raceRow()).getByRole("button", { name: "Delete…" }));
    await act(async () => { fireEvent.click(within(raceRow()).getByRole("button", { name: "Delete forever" })); });
    const dialog = document.querySelector<HTMLElement>('[role="dialog"]')!;
    await act(async () => { fireEvent.click(within(dialog).getByRole("switch", { name: /worktree/i })); });
    await act(async () => { fireEvent.click(within(document.querySelector('[role="dialog"]')!).getByRole("button", { name: "Delete forever" })); });
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("race_delete", { id: "r1", removeWorktree: true });
  });

  it("leaves the race alone when the confirm is cancelled", async () => {
    chatsTab();
    fireEvent.click(within(raceRow()).getByRole("button", { name: "Delete…" }));
    await act(async () => { fireEvent.click(within(raceRow()).getByRole("button", { name: "Delete forever" })); });
    await act(async () => { fireEvent.click(within(document.querySelector('[role="dialog"]')!).getByRole("button", { name: "Cancel" })); });
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === "race_delete")).toBe(false);
    expect(getRaces().races.r1).toBeTruthy();
  });
});
