// @vitest-environment jsdom
// Where the router shows up outside Settings: the transcript chip, the audit
// view, the route menu and the composer chip's tooltip.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { readFileSync } from "node:fs";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import type { Item, RouteEvent, Settings, TaskSummary } from "../api";
import { get, set } from "../store";
import { FeedRows, RouterNotice } from "./Session";
import { newestFirst, openRouterLog, RouterActivity } from "./RouterActivity";
import { Overlays } from "./Overlays";
import { SETTINGS_TABS, HIDDEN_SETTINGS_TABS, settingsTabDef } from "./settingsTabs";

const task = { id: "t1", title: "Work", status: "running", model: "anthropic/opus", effort: 2, cwd: "D:/p", subs: [], archived: false, hidden: false, pending: {}, route: "smart", plan: false, perm: "auto", assist: "default", agents: [] } as unknown as TaskSummary;
const routerItem: Item = { id: "n1", kind: "notice", text: "Router: escalated to Opus 5 — Stuck: `bash` failed the same way 2× · for 3 turns", ts: "2026-10-08T00:00:00Z", data: { level: "router", action: "escalate", from: "anthropic/sonnet", to: "anthropic/opus", rule: "Stuck", reason: "x" } };
const ev = (at: number, to: string): RouteEvent => ({ at, agent: "main", agent_kind: "main", from: "anthropic/sonnet", to, effort_from: 2, effort_to: 2, action: "escalate", rule_id: "rule-1", rule_name: "Stuck", reason: `Stuck: to ${to}` });

beforeEach(() => {
  (window as unknown as { matchMedia?: unknown }).matchMedia ??= () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  (Element.prototype as unknown as { getAnimations?: unknown }).getAnimations ??= () => [];
  invoke.mockImplementation(async () => null);
  set({ items: {}, tasks: { t1: task }, task: "t1", view: "session", notices: [], routerLog: null, menu: null,
    settings: { routes: [{ id: "smart", name: "Smart", heads: [], all: true, steps: ["openai/gpt-5"], on_exhausted: "pause", rules: [
      { id: "rule-1", name: "a", enabled: true, match: "all", when: [], then: { kind: "stay" } },
      { id: "rule-2", name: "b", enabled: true, match: "all", when: [], then: { kind: "stay" } },
    ] }], mcp: [], projects: [] } as unknown as Settings });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });

describe("router notices in the transcript", () => {
  it("renders a Router chip with the switch, without repeating the prefix", () => {
    render(<RouterNotice it={routerItem} />);
    const chip = document.querySelector(".router-chip")!;
    expect(chip.textContent).toBe("Router");
    expect(screen.getByText(/^escalated to Opus 5 — Stuck/)).toBeTruthy();
    expect(document.body.textContent).not.toContain("Router: escalated");
  });

  it("rides along in a tool group and is counted in its summary", () => {
    render(<FeedRows task={task} items={[routerItem]} showThinking={false} bgs={[]} />);
    expect(document.body.textContent).toContain("The router switched model");
  });
});

describe("router activity", () => {
  it("orders events newest first", () => {
    expect(newestFirst([ev(1, "a"), ev(3, "c"), ev(2, "b")]).map((e) => e.to)).toEqual(["c", "b", "a"]);
  });

  it("lists the chat's route_log, newest first", async () => {
    invoke.mockImplementation(async (cmd: string) => (cmd === "route_log" ? [ev(100, "anthropic/haiku"), ev(200, "anthropic/opus")] : null));
    render(<RouterActivity />);
    await act(async () => { openRouterLog("t1"); });
    expect(invoke).toHaveBeenCalledWith("route_log", { id: "t1" });
    await waitFor(() => expect(document.querySelectorAll(".router-log-row")).toHaveLength(2));
    const rows = [...document.querySelectorAll(".router-log-row")].map((r) => r.textContent ?? "");
    expect(rows[0]).toContain("Stuck: to anthropic/opus");
    expect(rows[1]).toContain("Stuck: to anthropic/haiku");
    expect(get().menu).toBeNull();
  });

  it("says so when the log can't be read", async () => {
    invoke.mockImplementation(async (cmd: string) => { if (cmd === "route_log") throw "gone"; return null; });
    set({ routerLog: "t1" });
    render(<RouterActivity />);
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("gone"));
  });

  it("is offered from the route menu", async () => {
    set({ menu: "route", menuAnchor: new DOMRect(10, 10, 10, 10) });
    render(<Overlays z={1} />);
    expect(document.body.textContent).toContain("Edit routers…");
    fireEvent.click(screen.getByText("Router activity…"));
    expect(get().routerLog).toBe("t1");
  });

  it("names the router and its rules in the composer chip's tooltip", () => {
    const src = readFileSync("src/ui/Composer.tsx", "utf8");
    expect(src).toMatch(/`Router: \$\{r\.name\} · \$\{routeSummary\(r\)\}`/);
  });
});

describe("the Routers settings page", () => {
  it("is in the nav again, labelled Routers, under its old id", () => {
    expect(HIDDEN_SETTINGS_TABS).not.toContain("routing");
    expect(settingsTabDef("routing").label).toBe("Routers");
    expect(SETTINGS_TABS.filter((t) => t.id === "routing")).toHaveLength(1);
  });

  it("is reachable from the palette as Routers and fallbacks", () => {
    expect(readFileSync("src/ui/Overlays.tsx", "utf8")).toContain('label: "Routers and fallbacks", run: () => go("settings", { settingsTab: "routing" })');
  });
});
