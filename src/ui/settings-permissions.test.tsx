// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
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

import { DEFAULT_PERM, PERMS, permLabel, type Settings } from "../api";
import { get, set } from "../store";
import { SettingsView } from "./Settings";
import { setOpts, expandCustomCommand } from "./Composer";

const settings = (perm: Settings["perm"]) => ({ perm, allow: [] } as unknown as Settings);

function permissionRow(label: string) {
  return [...document.querySelectorAll<HTMLElement>('[role="radio"]')]
    .find((row) => row.textContent?.includes(label))!;
}

beforeEach(() => {
  set({
    settings: settings("auto"),
    settingsTab: "perms",
    home: { ...get().home, perm: "auto" },
  });
  vi.mocked(invoke).mockImplementation(async (command: string, args?: Record<string, any>) => {
    if (command === "settings_update") return { ...settings(DEFAULT_PERM), ...args?.patch } as never;
    return null as never;
  });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("Full Access permission mode", () => {
  it("expands a user prompt template and arguments without interpreting them", () => {
    expect(expandCustomCommand("/review src/a.ts", [{ name: "review", prompt: "Review {{args}}; preserve {{args}}" }])).toBe("Review src/a.ts; preserve src/a.ts");
    expect(expandCustomCommand("/plan", [{ name: "plan", prompt: "custom" }])).toBe("/plan");
  });

  it("shows the default-off Needs you setting and persists enabling it", async () => {
    set({ settingsTab: "general", settings: { ...settings("auto"), needs_you: false } as Settings });
    render(<SettingsView />);
    const toggle = screen.getByRole("switch", { name: "Show Needs you inbox" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    await act(async () => { fireEvent.click(toggle); });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { needs_you: true } });
    expect(get().settings?.needs_you).toBe(true);
  });

  it("renders the Commands settings page and persists prompt templates", async () => {
    Object.defineProperty(Element.prototype, "getAnimations", { configurable: true, value: () => [] });
    window.matchMedia ??= (() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia;
    set({ settingsTab: "commands", settings: { ...settings("auto"), custom_commands: [] } as Settings });
    render(<SettingsView />);
    fireEvent.change(document.querySelector<HTMLInputElement>('[aria-label="Command name"]')!, { target: { value: "review-pr" } });
    fireEvent.change(document.querySelector<HTMLInputElement>('[aria-label="Command description"]')!, { target: { value: "Review pull request" } });
    fireEvent.change(document.querySelector<HTMLTextAreaElement>('[aria-label="Command prompt"]')!, { target: { value: "Review {{args}}" } });
    await act(async () => { fireEvent.click([...document.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === "Add command")!); });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { custom_commands: [{ name: "review-pr", description: "Review pull request", prompt: "Review {{args}}" }] } });
  });

  it("uses Full Access when no new-chat mode has been recorded", () => {
    expect(DEFAULT_PERM).toBe("turbo");
    expect(permLabel(undefined)).toBe("Full Access");
    expect(permLabel("")).toBe("Full Access");
    expect(permLabel("future-mode")).toBe("Allowlist only");
  });

  it("persists new-chat permission choices for startup restoration", async () => {
    set({ view: "home", task: null });
    expect(await setOpts({ perm: "turbo" })).toEqual({ ok: true, task: null });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { perm: "turbo" } });
    expect(get().settings?.perm).toBe("turbo");
    expect(get().home.perm).toBe("turbo");
  });

  it("does not change the new-chat permission when persistence fails", async () => {
    set({ view: "home", task: null });
    vi.mocked(invoke).mockRejectedValueOnce(new Error("write failed"));
    expect(await setOpts({ perm: "turbo" })).toEqual({ ok: false, task: null });
    expect(get().settings?.perm).toBe("auto");
    expect(get().home.perm).toBe("auto");
  });

  it("keeps the turbo id while displaying Full Access", () => {
    expect(PERMS.find((perm) => perm.name === "Full Access")?.id).toBe("turbo");
    expect(permLabel("turbo")).toBe("Full Access");
  });

  it("shows the selected default, saves a choice, then updates the new-chat default on success", async () => {
    set({
      settings: { ...settings(DEFAULT_PERM), perm: DEFAULT_PERM } as Settings,
      home: { ...get().home, perm: DEFAULT_PERM },
    });
    render(<SettingsView />);
    expect(permissionRow("Full Access").getAttribute("aria-checked")).toBe("true");
    expect(permissionRow("Auto-edit").getAttribute("aria-checked")).toBe("false");

    await act(async () => { fireEvent.click(permissionRow("Auto-edit")); });
    await waitFor(() => expect(get().home.perm).toBe("auto"));
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { perm: "auto" } });
    expect(permissionRow("Auto-edit").getAttribute("aria-checked")).toBe("true");
  });

  it("keeps the old default selected when saving fails", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("write failed"));
    render(<SettingsView />);

    await act(async () => { fireEvent.click(permissionRow("Full Access")); });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { perm: "turbo" } }));
    expect(get().home.perm).toBe("auto");
    expect(permissionRow("Auto-edit").getAttribute("aria-checked")).toBe("true");
    expect(permissionRow("Full Access").getAttribute("aria-checked")).toBe("false");
  });
});
