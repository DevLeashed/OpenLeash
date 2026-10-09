import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { get, set } from "../store";
import type { Settings, TaskSummary } from "../api";

const tasks = (kind: "approval" | "question"): Record<string, TaskSummary> => ({ [kind]: { id: kind, title: kind, status: "waiting", waiting_kind: kind, paused: null, archived: false, project: "" } as TaskSummary });

vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn() }) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { Sidebar, openAccountsSettings } from "./Chrome";

beforeEach(() => set({ sidebar: true, view: "home", task: null, settings: { needs_you: true, saved_prompts: [{ title: "Existing prompt" }] } as unknown as Settings, tasks: {} }));

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

  it("uses the footer shortcut to navigate directly to Accounts settings", () => {
    set({ view: "home", settingsTab: "general" });
    openAccountsSettings();
    expect(get().view).toBe("settings");
    expect(get().settingsTab).toBe("accounts");
  });

  it("uses an Accounts key shortcut in the footer instead of the saved-prompts bookmark", () => {
    const html = renderToStaticMarkup(createElement(Sidebar));
    expect(html).toContain('aria-label="Accounts"');
    expect(html).not.toContain('aria-label="Saved prompts"');
    // Saved prompts remain available as a separate list row while the bottom icon becomes Accounts.
    expect(html).toContain("Saved prompts");
    expect(html).toContain("lucide-key-round");
  });
});
