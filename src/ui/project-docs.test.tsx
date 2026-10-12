// @vitest-environment jsdom
// The per-project doc selection in Settings → General. The backend test
// (`reviewer_docs.rs`) proves the prompt actually drops a switched-off file;
// these cover the UI contract: which files are offered, that a toggle writes the
// whole rule list, and that toggling back on removes the rule rather than
// leaving an empty one behind (which would still suppress nothing but is churn).
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

import type { ProjectDocRule, Settings } from "../api";
import { get, loadProject, set } from "../store";
import { SettingsView } from "./Settings";

const PROJECT = "c:/work/app";

function projectInfo(docs: string[], disabled_docs: string[] = []) {
  return { path: PROJECT, exists: true, git: true, branch: "main", branches: [], memory: docs, docs, disabled_docs, trusted: true, decided: true };
}

const base = (over: Partial<Settings> = {}) => ({ perm: "turbo", allow: [], project: PROJECT, projects: [PROJECT], project_docs: [], ...over } as unknown as Settings);

beforeEach(() => {
  set({ settingsTab: "general", projectInfo: projectInfo(["AGENTS.md", "CLAUDE.md"]), settings: base() });
  vi.mocked(invoke).mockImplementation(async (command: string, args?: unknown) => {
    if (command === "settings_update") return { ...get().settings, ...(args as { patch: object }).patch } as never;
    return null as never;
  });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("Project docs selection", () => {
  it("offers every instruction file the project ships, all on by default", () => {
    render(<SettingsView />);
    expect(screen.getByRole("switch", { name: "Inject AGENTS.md" }).getAttribute("aria-checked")).toBe("true");
    expect(screen.getByRole("switch", { name: "Inject CLAUDE.md" }).getAttribute("aria-checked")).toBe("true");
  });

  it("switching CLAUDE.md off records a rule for this project", async () => {
    render(<SettingsView />);
    await act(async () => { fireEvent.click(screen.getByRole("switch", { name: "Inject CLAUDE.md" })); });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { project_docs: [{ path: PROJECT, disabled: ["CLAUDE.md"] }] } });
    expect(get().settings?.project_docs).toEqual([{ path: PROJECT, disabled: ["CLAUDE.md"] }]);
  });

  it("switching it back on removes the rule instead of storing an empty one", async () => {
    set({ settings: base({ project_docs: [{ path: PROJECT, disabled: ["CLAUDE.md"] }] as ProjectDocRule[] }), projectInfo: projectInfo(["AGENTS.md", "CLAUDE.md"], ["CLAUDE.md"]) });
    render(<SettingsView />);
    expect(screen.getByRole("switch", { name: "Inject CLAUDE.md" }).getAttribute("aria-checked")).toBe("false");
    await act(async () => { fireEvent.click(screen.getByRole("switch", { name: "Inject CLAUDE.md" })); });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { project_docs: [] } });
  });

  it("keeps another project's rule untouched", async () => {
    set({ settings: base({ project_docs: [{ path: "c:/other/repo", disabled: ["AGENTS.md"] }] as ProjectDocRule[] }) });
    render(<SettingsView />);
    await act(async () => { fireEvent.click(screen.getByRole("switch", { name: "Inject CLAUDE.md" })); });
    const rules = get().settings?.project_docs ?? [];
    expect(rules).toContainEqual({ path: "c:/other/repo", disabled: ["AGENTS.md"] });
    expect(rules).toContainEqual({ path: PROJECT, disabled: ["CLAUDE.md"] });
  });

  it("says so when the project has no instruction files", () => {
    set({ projectInfo: projectInfo([]) });
    render(<SettingsView />);
    expect(screen.getByText(/no instruction files of its own/i)).toBeTruthy();
    expect(screen.queryByRole("switch", { name: "Inject AGENTS.md" })).toBeNull();
    expect(screen.queryByRole("switch", { name: "Inject CLAUDE.md" })).toBeNull();
  });

  it("ignores a project_info answer for a folder that is no longer selected", async () => {
    // Two switches in flight can resolve out of order. The inventory describes
    // one folder's files while a toggle would write a rule for whichever project
    // is current, so a stale answer must not be applied at all.
    set({ settings: base({ project: "c:/work/other" }), projectInfo: null });
    vi.mocked(invoke).mockImplementation(async (command: string) =>
      command === "project_info" ? projectInfo(["AGENTS.md"]) : (null as never));
    await loadProject(PROJECT);
    expect(get().projectInfo).toBeNull();
  });

  it("applies an answer for the selected folder", async () => {
    set({ projectInfo: null });
    vi.mocked(invoke).mockImplementation(async (command: string) =>
      command === "project_info" ? projectInfo(["AGENTS.md"]) : (null as never));
    await loadProject(PROJECT);
    expect(get().projectInfo?.docs).toEqual(["AGENTS.md"]);
  });

  it("tells the user an untrusted folder is not reading any of them yet", () => {
    // The switches still record a choice (it applies the moment the folder is
    // trusted), but nothing here should read as "this is being injected now".
    set({ projectInfo: { ...projectInfo(["AGENTS.md"]), trusted: false } });
    render(<SettingsView />);
    expect(screen.getByText(/not trusted/i)).toBeTruthy();
    expect(screen.getByRole("switch", { name: "Inject AGENTS.md" })).toBeTruthy();
  });
});
