// @vitest-environment jsdom
// The per-project "keep memory in the project" switch in Settings → General.
// The backend (`memory.rs`, `memory_in_project_dispatch_tests` in runner.rs)
// proves where notes land and that plan mode / folder trust are respected;
// these cover the UI contract: the switch is per project, writes the whole
// list, normalizes nothing it did not own, and is absent where it cannot work.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
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

import type { Settings } from "../api";
import { get, set } from "../store";
import { SettingsView } from "./Settings";

const PROJECT = "c:/work/app";
const SWITCH = "Keep this project's memory in the project";

const base = (over: Partial<Settings> = {}) => ({ perm: "turbo", allow: [], project: PROJECT, projects: [PROJECT], memory: true, ...over } as unknown as Settings);

beforeEach(() => {
  set({ settingsTab: "general", projectInfo: null, settings: base() });
  vi.mocked(invoke).mockImplementation(async (command: string, args?: unknown) => {
    if (command === "settings_update") return { ...get().settings, ...(args as { patch: object }).patch } as never;
    return null as never;
  });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("Memory in the project", () => {
  it("is off by default and switching it on records this project only", async () => {
    render(<SettingsView />);
    expect(screen.getByRole("switch", { name: SWITCH }).getAttribute("aria-checked")).toBe("false");
    await act(async () => { fireEvent.click(screen.getByRole("switch", { name: SWITCH })); });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { memory_in_project: [PROJECT] } });
  });

  it("switching it off removes this project and keeps the others", async () => {
    set({ settings: base({ memory_in_project: ["c:/other/repo", "C:\\Work\\App\\"] }) });
    render(<SettingsView />);
    // A differently spelled path for the same folder reads as on.
    expect(screen.getByRole("switch", { name: SWITCH }).getAttribute("aria-checked")).toBe("true");
    await act(async () => { fireEvent.click(screen.getByRole("switch", { name: SWITCH })); });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { memory_in_project: ["c:/other/repo"] } });
  });

  it("is hidden with no project, for a remote project, and while memory is off", () => {
    set({ settings: base({ project: "" }) });
    const { unmount } = render(<SettingsView />);
    expect(screen.queryByRole("switch", { name: SWITCH })).toBeNull();
    unmount();
    set({ settings: base({ project: "ssh://me@host/srv/app" }) });
    const second = render(<SettingsView />);
    expect(screen.queryByRole("switch", { name: SWITCH })).toBeNull();
    second.unmount();
    set({ settings: base({ memory: false }) });
    render(<SettingsView />);
    expect(screen.queryByRole("switch", { name: SWITCH })).toBeNull();
  });
});
