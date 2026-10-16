import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { get, set } from "../store";
import type { Settings, TaskSummary } from "../api";

const tasks = (kind: "approval" | "question"): Record<string, TaskSummary> => ({ [kind]: { id: kind, title: kind, status: "waiting", waiting_kind: kind, paused: null, archived: false, project: "" } as TaskSummary });

vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn() }) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { Sidebar, openSidebarShortcut } from "./Chrome";

const settingsWith = (extra: Partial<Settings> = {}) => ({ needs_you: true, saved_prompts: [{ title: "Existing prompt" }], ...extra }) as unknown as Settings;

beforeEach(() => set({ sidebar: true, view: "home", task: null, settings: settingsWith(), tasks: {} }));

describe("sidebar attention and Accounts shortcut", () => {
  it("hides the optional Needs you row by default", () => {
    set({ settings: { saved_prompts: [] } as unknown as Settings, tasks: {} });
    const html = renderToStaticMarkup(createElement(Sidebar));
    expect(html).not.toContain("Needs you");
  });

  it.each(["approval", "question"] as const)("shows a Needs you entry for a pending %s when enabled", (kind) => {
    set({ tasks: tasks(kind) });
    const html = renderToStaticMarkup(createElement(Sidebar));
    expect(html).toContain("Needs you");
    expect(html).toContain("Accounts");
    expect(html).toContain("Saved prompts");
  });

  it("uses the footer shortcut to navigate directly to Accounts when nothing else is chosen", () => {
    set({ view: "home", settingsTab: "general" });
    openSidebarShortcut();
    expect(get().view).toBe("settings");
    expect(get().settingsTab).toBe("accounts");
  });

  it("sends the footer shortcut to the page chosen in Settings", () => {
    set({ view: "home", settingsTab: "general", settings: settingsWith({ sidebar_button_tab: "perms" }) });
    openSidebarShortcut();
    expect(get().view).toBe("settings");
    expect(get().settingsTab).toBe("perms");
  });

  // A settings file can name a page this build does not have — one removed since,
  // or one from a newer build. Opening a blank settings screen is worse than
  // landing on the page the button has always opened.
  it("falls back to Accounts for a page this build does not have", () => {
    set({ view: "home", settingsTab: "general", settings: settingsWith({ sidebar_button_tab: "gone" }) });
    openSidebarShortcut();
    expect(get().settingsTab).toBe("accounts");
  });

  it("draws an Accounts shortcut in the footer instead of the saved-prompts bookmark", () => {
    const html = renderToStaticMarkup(createElement(Sidebar));
    expect(html).toContain('aria-label="Accounts"');
    expect(html).not.toContain('aria-label="Saved prompts"');
    // Saved prompts remain available as a separate list row while the bottom icon becomes Accounts.
    expect(html).toContain("Saved prompts");
    expect(html).toContain("lucide-user-round");
  });

  // The button names where it goes, glyph included, so a footer shortcut aimed at
  // Chats does not still read as Accounts.
  it("labels the footer shortcut with the chosen page", () => {
    set({ settings: settingsWith({ sidebar_button_tab: "chats" }) });
    const html = renderToStaticMarkup(createElement(Sidebar));
    expect(html).toContain('aria-label="Chats"');
    expect(html).toContain("lucide-archive");
    expect(html).not.toContain('aria-label="Accounts"');
  });
});
