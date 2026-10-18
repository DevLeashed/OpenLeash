// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import type { Settings, TaskSummary } from "../api";
import { get, set } from "../store";
import { PAGES, Welcome } from "./Welcome";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: () => false }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

const settings = (over: Partial<Settings> = {}) => ({ project: "", projects: [], welcome_seen: false, ...over } as unknown as Settings);

beforeEach(() => {
  set({ ready: true, welcome: false, tasks: {}, providers: [], settings: settings() });
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    if (cmd === "settings_update") return { ...get().settings, ...(args as { patch: Partial<Settings> }).patch };
    return null;
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const title = () => screen.getByRole("heading").textContent?.replace(/\s+/g, " ").trim();

describe("welcome tour", () => {
  it("opens by itself on a genuine first run", () => {
    render(<Welcome />);
    expect(screen.getByRole("dialog", { name: "Welcome tour" })).toBeTruthy();
    expect(title()).toBe("Your coding agents, in one place.");
  });

  it("stays shut for an existing install that predates the flag", () => {
    // Upgraded installs load `welcome_seen` as false; their chats are what tell them apart.
    set({ tasks: { a: { id: "a", status: "idle" } as TaskSummary } });
    render(<Welcome />);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("stays shut once it has been seen", () => {
    set({ settings: settings({ welcome_seen: true }) });
    render(<Welcome />);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("pages forward and back with buttons, dots and arrow keys", () => {
    render(<Welcome />);
    fireEvent.click(screen.getByRole("button", { name: /Show me/ }));
    expect(title()).toBe("Say what you want. Approve what matters.");
    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(title()).toBe("Look before it leaps.");
    fireEvent.click(screen.getByRole("button", { name: "Previous page" }));
    expect(title()).toBe("Say what you want. Approve what matters.");
    fireEvent.keyDown(window, { key: "ArrowLeft" });
    fireEvent.keyDown(window, { key: "ArrowLeft" }); // already first: stays
    expect(title()).toBe("Your coding agents, in one place.");
    fireEvent.click(screen.getByRole("tab", { name: /Artificts/ }));
    expect(title()).toBe("Design it, then build it.");
  });

  it("teaches the controls that matter, each on its own page", () => {
    const ids = PAGES.map((p) => p.id);
    expect(ids).toEqual(["hello", "basics", "plan", "steer", "parallel", "delegate", "choices", "artificts", "evidence", "preferences", "keys"]);
  });

  it.each([
    ["choices", ["Best-of-N", "Settings → Routers", "Quota burner", "off by default", "Usage caps still win"]],
    ["evidence", ["proof clip", "record_start", "visual diff", "Find breaking edit…", "Run timeline…"]],
    ["preferences", ["30 options", "skip any question", "ntfy", "Save as skill…", "Nothing is saved automatically"]],
  ] as const)("introduces the discoverable controls on the %s page", (id, controls) => {
    const page = PAGES.find((p) => p.id === id)!;
    const { container } = render(<>{page.body}</>);
    for (const control of controls) expect(container.textContent).toContain(control);
  });

  it("finishing marks it seen, so it never auto-opens again", async () => {
    render(<Welcome />);
    for (let i = 0; i < PAGES.length - 1; i++) fireEvent.keyDown(window, { key: "ArrowRight" });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /Start building/ })); });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { welcome_seen: true } });
    expect(get().settings?.welcome_seen).toBe(true);
  });

  it("skip and Esc both close it and mark it seen", async () => {
    render(<Welcome />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Skip tour" })); });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { welcome_seen: true } });

    vi.mocked(invoke).mockClear();
    set({ settings: settings() });
    act(() => set({ welcome: true }));
    expect(screen.getByRole("dialog")).toBeTruthy();
    await act(async () => { fireEvent.keyDown(window, { key: "Escape" }); });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { welcome_seen: true } });
  });

  it("reopens from the store flag even after being seen", () => {
    set({ settings: settings({ welcome_seen: true }) });
    render(<Welcome />);
    act(() => set({ welcome: true }));
    expect(screen.getByRole("dialog", { name: "Welcome tour" })).toBeTruthy();
  });
});
