// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { BURNER_DEFAULTS, type BurnerStatus, type Settings } from "../api";
import { get, set } from "../store";
import { allowedPerms, burnerOf, BurnerSection } from "./Burner";

const mocked = invoke as unknown as ReturnType<typeof vi.fn>;
const base = (extra: Partial<Settings> = {}) => ({ perm: "auto", project: "/repo/app", projects: ["/repo/app"], allow: [], ...extra } as unknown as Settings);
const status: BurnerStatus = {
  accounts: [{ account_id: "a1", label: "work", kind: "claude", head: true, window: { label: "5h", used: 30, resets_at: Math.floor(Date.now() / 1000) + 600 }, reason: "" }],
  runs: [{ task_id: "t1", item_id: "q0", project: "/repo/app", prompt: "audit deps", model: "claude/x", perm: "auto", account_id: "a1", account_label: "work", kind: "claude", window: "5h", resets_at: Math.floor(Date.now() / 1000) + 600, started_at: 1, ended_at: 0, end_reason: "" }],
};

beforeEach(() => {
  set({ settings: base(), providers: [], models: [] });
  mocked.mockImplementation(async (command: string, args?: { patch?: Record<string, unknown> }) => {
    if (command === "settings_update") return { ...get().settings, ...args?.patch } as never;
    if (command === "burner_status") return status as never;
    return null as never;
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });

describe("quota burner settings", () => {
  it("fills defaults for settings saved before the burner existed", () => {
    expect(burnerOf(undefined)).toEqual(BURNER_DEFAULTS);
    expect(burnerOf(undefined).enabled).toBe(false);
    expect(burnerOf({ enabled: true } as never).spend_cap).toBe(90);
  });

  it("never offers a permission mode stronger than the default", () => {
    expect(allowedPerms("auto").map((p) => p.id)).not.toContain("turbo");
    expect(allowedPerms("auto").map((p) => p.id)).toContain("auto");
    expect(allowedPerms("disabled").map((p) => p.id)).toEqual(["disabled"]);
    expect(allowedPerms("turbo").map((p) => p.id)).toContain("turbo");
  });

  it("is off by default and switching it on saves the burner config", async () => {
    render(<BurnerSection />);
    const toggle = screen.getByRole("switch", { name: "Quota burner" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    await act(async () => { fireEvent.click(toggle); });
    expect(mocked).toHaveBeenCalledWith("settings_update", { patch: { burner: { ...BURNER_DEFAULTS, enabled: true } } });
  });

  it("adds a backlog item for the current project with the default permission", async () => {
    render(<BurnerSection />);
    fireEvent.change(screen.getByLabelText("Backlog prompt"), { target: { value: "sweep TODOs" } });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Add to backlog" })); });
    const call = mocked.mock.calls.find((c) => c[0] === "settings_update");
    const queue = (call?.[1] as { patch: { burner: { queue: { prompt: string; project: string; perm: string; model: string }[] } } }).patch.burner.queue;
    expect(queue).toHaveLength(1);
    expect(queue[0]).toMatchObject({ prompt: "sweep TODOs", project: "/repo/app", perm: "", model: "" });
  });

  it("removes an item and offers a retry for one that failed to start", async () => {
    set({ settings: base({ burner: { ...BURNER_DEFAULTS, queue: [{ id: "q1", project: "/repo/app", prompt: "audit deps", model: "", perm: "", created_at: 1, error: "not a git repository" }] } }) });
    render(<BurnerSection />);
    expect(screen.getByText(/Couldn't start: not a git repository/)).toBeTruthy();
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Retry" })); });
    let call = mocked.mock.calls.filter((c) => c[0] === "settings_update").pop();
    expect((call?.[1] as { patch: { burner: { queue: { error: string }[] } } }).patch.burner.queue[0]!.error).toBe("");
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Remove audit deps" })); });
    call = mocked.mock.calls.filter((c) => c[0] === "settings_update").pop();
    expect((call?.[1] as { patch: { burner: { queue: unknown[] } } }).patch.burner.queue).toEqual([]);
  });

  it("shows which chats the burner started and what each account is waiting on", async () => {
    render(<BurnerSection />);
    await waitFor(() => expect(screen.getAllByTestId("burner-run")).toHaveLength(1));
    expect(screen.getByText("audit deps")).toBeTruthy();
    expect(screen.getByTestId("burner-run").textContent).toContain("running");
    expect(screen.getByTestId("burner-account").textContent).toContain("Ready: the 5h window");
  });
});
