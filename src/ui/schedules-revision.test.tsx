// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type EventCallback } from "@tauri-apps/api/event";
import type { Schedule, SchedulesState } from "../api";
import { boot, disposeBoot, get, loadSchedules, set } from "../store";
import { ScheduleForm, toDraft, Schedules } from "./Schedules";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ isFocused: async () => true, isVisible: async () => true, isMinimized: async () => false, onFocusChanged: async () => () => {} }) }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => { resolve = r; });
  return { promise, resolve };
}
const schedule: Schedule = {
  id: "s1", name: "Report", enabled: true, prompt: "Report changes", project: "D:/dummy/project",
  model: "dummy/model", effort: 2, perm: "auto", plan: false, workspace: "local", base_branch: "",
  timing: { kind: "daily", time: "09:00" }, catch_up: true, created_at: "2026-10-01T00:00:00Z",
  next_run_at: null, last_run_at: null, last_status: "", last_message: "", last_task_id: null, history: [],
};
const initial = { revision: 1, paused: false, schedules: [schedule] };
let callback: EventCallback<SchedulesState>;
const emit = (snapshot: SchedulesState) => callback({ event: "ol://schedules", id: 1, payload: snapshot });

beforeEach(async () => {
  set({ schedules: null, tasks: {} });
  vi.mocked(listen).mockImplementation(async (event, handler) => {
    if (event === "ol://schedules") callback = handler as EventCallback<SchedulesState>;
    return () => {};
  });
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "app_boot") return { settings: { model: "dummy/model", perm: "auto", effort: 2, project: "", projects: [], routes: [], show_schedules: true }, tasks: [], providers: [], models: [], accounts: [], agents: [] };
    if (command === "schedules_list") return initial;
    if (command === "draft_get") return "";
    return null;
  });
  await boot();
});
afterEach(() => { cleanup(); disposeBoot(); vi.resetAllMocks(); });

it("keeps a newer schedules event when an older deferred list resolves", async () => {
  const oldList = deferred<SchedulesState>();
  vi.mocked(invoke).mockImplementationOnce(() => oldList.promise);
  const loading = loadSchedules();
  const newer = { ...initial, revision: 2, paused: true };
  emit(newer);
  expect(get().schedules).toEqual(newer);
  oldList.resolve(initial);
  await loading;
  expect(get().schedules).toEqual(newer);
});

it("does not resurrect a deleted schedule from a stale deferred save or its stale refresh", async () => {
  emit(initial);
  const oldSave = deferred<Schedule>();
  const oldList = deferred<SchedulesState>();
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "schedule_save") return oldSave.promise;
    if (command === "schedules_list") return oldList.promise;
    throw new Error(`Unexpected ${command}`);
  });
  render(<ScheduleForm initial={toDraft(schedule)} onClose={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Save schedule" }));
  expect(invoke).toHaveBeenCalledWith("schedule_save", { schedule: toDraft(schedule) });
  const deleted = { revision: 3, paused: true, schedules: [] };
  await act(async () => { emit(deleted); oldSave.resolve(schedule); });
  // A per-row result has no global revision: even before refreshing it must
  // not recreate a row the newer event removed, or reset global pause.
  expect(get().schedules).toEqual(deleted);
  expect(invoke).toHaveBeenCalledWith("schedules_list");
  await act(async () => { oldList.resolve(initial); });
  expect(get().schedules).toEqual(deleted);
});

it("rejects a stale pause command snapshot after a newer unpause event", async () => {
  emit(initial);
  render(<Schedules />);
  await act(async () => {});
  const pauseResult = deferred<SchedulesState>();
  vi.mocked(invoke).mockImplementationOnce(() => pauseResult.promise);
  fireEvent.click(screen.getByRole("switch", { name: "Pause all schedules" }));
  const unpaused = { ...initial, revision: 3, paused: false };
  await act(async () => { emit(unpaused); pauseResult.resolve({ ...initial, revision: 2, paused: true }); });
  expect(get().schedules).toEqual(unpaused);
  expect(screen.getByRole("switch", { name: "Pause all schedules" }).getAttribute("aria-checked")).toBe("false");
});

it("accepts initial, equal and newer revisions but rejects older events", () => {
  emit(initial);
  expect(get().schedules).toEqual(initial);
  const newer = { ...initial, revision: 4, paused: true };
  emit(newer);
  emit(initial);
  expect(get().schedules).toBe(newer);
  const equal = { ...newer };
  emit(equal);
  expect(get().schedules).toBe(equal);
});
