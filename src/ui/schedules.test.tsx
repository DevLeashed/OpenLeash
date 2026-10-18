// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("./primitives", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./primitives")>();
  return { ...actual, MorphText: ({ children }: { children: React.ReactNode }) => <>{children}</> };
});

import type { Schedule, ScheduleDraft, SchedulesState, Settings, TaskSummary } from "../api";
import { get, set } from "../store";
import { Sidebar } from "./Chrome";
import { SettingsView } from "./Settings";
import { describeTiming, fmtWhen, newScheduleDraft, Schedules } from "./Schedules";

const PROJECT = "D:/work/app";
const baseSettings = (extra: Partial<Settings> = {}) => ({
  perm: "auto", model: "openai/gpt-x", effort: 2, recent_models: [], projects: [PROJECT], project: PROJECT,
  saved_prompts: [], allow: [], worktree: false, ...extra,
}) as unknown as Settings;

const sched = (over: Partial<Schedule> = {}): Schedule => ({
  id: "s1", name: "Nightly deps", enabled: true, prompt: "Check dependencies", project: PROJECT,
  model: "openai/gpt-x", effort: 2, perm: "allowlist", plan: false, workspace: "local", base_branch: "",
  timing: { kind: "daily", time: "09:00" }, catch_up: true, created_at: "2026-10-01T00:00:00Z",
  next_run_at: "2026-10-18T09:00:00Z", last_run_at: new Date(Date.now() - 5 * 60_000).toISOString(),
  last_status: "started", last_message: "Started a chat", last_task_id: "t1", history: [], ...over,
});

const mockInvoke = vi.mocked(invoke) as unknown as ReturnType<typeof vi.fn>;
let state: SchedulesState;

function permShown() {
  return screen.getByRole("combobox", { name: "Permission mode" }).querySelector(".ddv")?.textContent;
}

beforeEach(() => {
  Object.defineProperty(Element.prototype, "getAnimations", { configurable: true, value: () => [] });
  window.matchMedia ??= (() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia;
  state = { revision: 1, paused: false, schedules: [sched()] };
  set({ sidebar: true, view: "schedules", task: null, tasks: {}, models: [], providers: [], schedules: null, settings: baseSettings({ show_schedules: true }), home: { ...get().home, model: "" } });
  mockInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "schedules_list") return state;
    if (cmd === "schedule_run_now") return sched({ last_status: "started", last_task_id: "t2" });
    if (cmd === "schedule_set_enabled") {
      const updated = sched({ enabled: args?.enabled as boolean });
      state = { ...state, revision: state.revision + 1, schedules: [updated] };
      return updated;
    }
    if (cmd === "schedules_set_paused") {
      state = { ...state, revision: state.revision + 1, paused: args?.paused as boolean };
      return state;
    }
    if (cmd === "schedule_save") return { ...sched(), ...(args?.schedule as ScheduleDraft), id: (args?.schedule as ScheduleDraft).id ?? "new1" };
    if (cmd === "settings_update") return { ...get().settings, ...(args?.patch as object) };
    return null;
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });

describe("describeTiming", () => {
  it("words intervals", () => {
    expect(describeTiming({ kind: "interval", every: 30, unit: "minutes" })).toBe("Every 30 minutes");
    expect(describeTiming({ kind: "interval", every: 1, unit: "minutes" })).toBe("Every minute");
    expect(describeTiming({ kind: "interval", every: 1, unit: "hours" })).toBe("Every hour");
    expect(describeTiming({ kind: "interval", every: 6, unit: "hours" })).toBe("Every 6 hours");
  });
  it("words daily and weekly timings", () => {
    expect(describeTiming({ kind: "daily", time: "09:00" })).toBe("Daily at 09:00");
    expect(describeTiming({ kind: "weekly", days: ["fri", "mon"], time: "18:30" })).toBe("Mon, Fri at 18:30");
    expect(describeTiming({ kind: "weekly", days: ["mon", "tue", "wed", "thu", "fri"], time: "08:00" })).toBe("Weekdays at 08:00");
    expect(describeTiming({ kind: "weekly", days: ["sun", "sat"], time: "10:00" })).toBe("Weekends at 10:00");
    expect(describeTiming({ kind: "weekly", days: ["mon", "tue", "wed", "thu", "fri", "sat", "sun"], time: "07:15" })).toBe("Daily at 07:15");
    expect(describeTiming({ kind: "weekly", days: [], time: "07:15" })).toContain("no days");
  });
});

describe("Schedules tab visibility", () => {
  it("is hidden in the sidebar by default", () => {
    set({ settings: baseSettings() });
    expect(renderToStaticMarkup(createElement(Sidebar))).not.toContain("Schedules");
  });
  it("is shown when show_schedules is on", () => {
    set({ settings: baseSettings({ show_schedules: true }) });
    expect(renderToStaticMarkup(createElement(Sidebar))).toContain("Schedules");
  });
  it("has a default-off switch in Settings → Tabs that persists", async () => {
    set({ settingsTab: "tabs", settings: baseSettings() });
    render(<SettingsView />);
    expect(screen.getByText("Schedules", { selector: ".settings-section-label" })).toBeTruthy();
    const toggle = screen.getByRole("switch", { name: "Show Schedules" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    await act(async () => { fireEvent.click(toggle); });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { show_schedules: true } });
    expect(get().settings?.show_schedules).toBe(true);
  });
  it("does not list schedules once the tab is switched off", () => {
    set({ settings: baseSettings({ show_schedules: false }) });
    render(<Schedules />);
    expect(screen.getByText(/Schedules tab is hidden/)).toBeTruthy();
    expect(invoke).not.toHaveBeenCalledWith("schedules_list");
  });
});

describe("Schedules list", () => {
  it("renders name, timing, next run, last result and a link to the last chat", async () => {
    set({ tasks: { t1: { id: "t1" } as TaskSummary } });
    render(<Schedules />);
    expect(await screen.findByText("Nightly deps")).toBeTruthy();
    expect(screen.getByText(/Daily at 09:00/)).toBeTruthy();
    expect(screen.getByText(`Next run ${fmtWhen("2026-10-18T09:00:00Z")}`)).toBeTruthy();
    expect(screen.getByText("Allowlist only")).toBeTruthy();
    expect(screen.getByText(/Last run 5m ago/)).toBeTruthy();
    expect(screen.getByText("Started a chat")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open chat" }));
    expect(get().view).toBe("session");
    expect(get().task).toBe("t1");
  });

  it("shows scheduled run chats in this tab, not in the ordinary sidebar list", async () => {
    set({ tasks: {
      scheduled: { id: "scheduled", title: "⏰ Nightly deps", archived: false, project: PROJECT } as TaskSummary,
      ordinary: { id: "ordinary", title: "Normal task", archived: false, project: PROJECT } as TaskSummary,
    } });
    render(<Schedules />);
    expect(await screen.findByText("⏰ Nightly deps")).toBeTruthy();
    expect(screen.getByText(/Scheduled chats/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Open chat" })).toBeTruthy();
  });

  it("hides the chat link when that chat is gone", async () => {
    render(<Schedules />);
    await screen.findByText("Nightly deps");
    expect(screen.queryByRole("button", { name: "Open chat" })).toBeNull();
  });

  it("runs now and toggles enabled through the backend", async () => {
    render(<Schedules />);
    await screen.findByText("Nightly deps");
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Run Nightly deps now" })); });
    expect(invoke).toHaveBeenCalledWith("schedule_run_now", { id: "s1" });
    await act(async () => { fireEvent.click(screen.getByRole("switch", { name: "Enable Nightly deps" })); });
    expect(invoke).toHaveBeenCalledWith("schedule_set_enabled", { id: "s1", enabled: false });
    await waitFor(() => expect(get().schedules?.schedules[0]?.enabled).toBe(false));
  });

  it("pauses all schedules", async () => {
    render(<Schedules />);
    await screen.findByText("Nightly deps");
    await act(async () => { fireEvent.click(screen.getByRole("switch", { name: "Pause all schedules" })); });
    expect(invoke).toHaveBeenCalledWith("schedules_set_paused", { paused: true });
  });

  it("deletes only after confirming", async () => {
    render(<Schedules />);
    await screen.findByText("Nightly deps");
    fireEvent.click(screen.getByRole("button", { name: "Delete Nightly deps" }));
    expect(invoke).not.toHaveBeenCalledWith("schedule_delete", { id: "s1" });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Delete" })); });
    expect(invoke).toHaveBeenCalledWith("schedule_delete", { id: "s1" });
  });

  it("explains the feature when there are none", async () => {
    state = { revision: 1, paused: false, schedules: [] };
    render(<Schedules />);
    expect(await screen.findByText(/No schedules yet/)).toBeTruthy();
  });
});

describe("Schedule form", () => {
  it.each([["auto", "Auto-edit"], ["allowlist", "Allowlist only"], ["reviewer", "Reviewer"]] as const)(
    "defaults the permission to the user's default (%s) and saves it", async (perm, name) => {
      set({ settings: baseSettings({ show_schedules: true, perm }) });
      render(<Schedules />);
      await screen.findByText("Nightly deps");
      fireEvent.click(screen.getByRole("button", { name: "New schedule" }));
      expect(permShown()).toBe(name);
      fireEvent.change(screen.getByLabelText("Schedule name"), { target: { value: "Weekly report" } });
      fireEvent.change(screen.getByLabelText("Schedule prompt"), { target: { value: "Summarise the week" } });
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Create schedule" })); });
      const call = mockInvoke.mock.calls.find(([cmd]) => cmd === "schedule_save");
      expect(call?.[1]).toEqual({ schedule: expect.objectContaining({ name: "Weekly report", prompt: "Summarise the week", perm, project: PROJECT, catch_up: true, enabled: true }) });
      expect((call?.[1] as { schedule: ScheduleDraft }).schedule.id).toBeUndefined();
    });

  it("shows an existing schedule's own permission, not the default", async () => {
    set({ settings: baseSettings({ show_schedules: true, perm: "turbo" }) });
    render(<Schedules />);
    await screen.findByText("Nightly deps");
    fireEvent.click(screen.getByRole("button", { name: "Edit Nightly deps" }));
    expect(permShown()).toBe("Allowlist only");
    expect((screen.getByLabelText("Schedule name") as HTMLInputElement).value).toBe("Nightly deps");
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save schedule" })); });
    expect(mockInvoke.mock.calls.find(([cmd]) => cmd === "schedule_save")?.[1]).toEqual({ schedule: expect.objectContaining({ id: "s1", perm: "allowlist" }) });
  });

  it("shows the backend's validation error inline", async () => {
    render(<Schedules />);
    await screen.findByText("Nightly deps");
    mockInvoke.mockImplementationOnce(async () => { throw new Error("Time must be HH:MM"); });
    fireEvent.click(screen.getByRole("button", { name: "Edit Nightly deps" }));
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save schedule" })); });
    expect((await screen.findByRole("alert")).textContent).toContain("Time must be HH:MM");
  });

  it("suggests a worktree for Full Access", async () => {
    set({ settings: baseSettings({ show_schedules: true, perm: "turbo" }) });
    render(<Schedules />);
    await screen.findByText("Nightly deps");
    fireEvent.click(screen.getByRole("button", { name: "New schedule" }));
    expect(screen.getByText(/A worktree keeps its changes/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Use Worktree" }));
    expect(screen.queryByText(/A worktree keeps its changes/)).toBeNull();
  });

  it("takes defaults from settings and the home composer", () => {
    const s = baseSettings({ perm: "allowlist", recent_models: ["anthropic/recent"], effort: 3, workspace: "worktree" });
    expect(newScheduleDraft(s, "")).toMatchObject({ perm: "allowlist", model: "anthropic/recent", effort: 3, workspace: "worktree", catch_up: true, enabled: true, project: PROJECT });
    expect(newScheduleDraft(s, "home/model").model).toBe("home/model");
    expect(newScheduleDraft(baseSettings({ perm: undefined as unknown as Settings["perm"] }), "").perm).toBe("turbo");
  });
});
