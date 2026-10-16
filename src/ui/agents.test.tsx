// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import { api, type AgentDef, type Settings } from "../api";
import { get, set } from "../store";
import { AgentsTab } from "./Accounts";

const agent: AgentDef = {
  id: "docs", name: "Docs", description: "Write documentation", prompt: "Update docs",
  model: "", tools: "all", color: "#a78bfa", builtin: false, source: "",
  groups: ["read", { name: "edit", file_regex: "\\.md$", description: "docs only" }],
};

beforeEach(() => {
  set({ agents: [agent], models: [], providers: [], accounts: [], settings: {
    agents: [agent], default_agents: ["explore", "general"], routes: [],
  } as unknown as Settings });
  vi.spyOn(api, "settings").mockImplementation(async (patch) => ({ ...get().settings!, ...patch }));
  vi.spyOn(api, "agents").mockResolvedValue([agent]);
  (Element.prototype as unknown as { getAnimations?: unknown }).getAnimations ??= () => [];
  window.matchMedia ??= (() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia;
});

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("subagent settings without step ceilings", () => {
  it("ignores legacy step ceilings on rows while preserving file restriction labels", () => {
    // A stale boot payload can still contain the removed field at runtime.
    set({ agents: [{ ...agent, ...{ steps: 60 } }] });
    const view = render(<AgentsTab />);
    expect(view.container.textContent).toContain("edits docs only");
    expect(view.container.textContent).not.toMatch(/stops after|step limit/);
  });

  it("keeps tool permissions editable and saves without a step ceiling", async () => {
    const view = render(<AgentsTab />);
    fireEvent.click(view.container.querySelector(".agentrow")!);
    expect(view.queryByRole("spinbutton", { name: "Step limit" })).toBeNull();
    expect(view.getByRole("radiogroup", { name: "Subagent tools" })).toBeTruthy();
    expect(view.getByRole("textbox", { name: "fileRegex" }).getAttribute("value")).toBe("\\.md$");
    await act(async () => { fireEvent.click(view.getByRole("button", { name: "Save" })); });
    const saved = vi.mocked(api.settings).mock.calls[0]?.[0].agents?.[0];
    expect(saved).toEqual({ ...agent, groups: agent.groups });
    expect(saved).not.toHaveProperty("steps");
  });
});
