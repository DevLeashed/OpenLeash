// @vitest-environment jsdom
// Settings → Routers: the editor keeps a draft, Save writes the whole list, and
// the test panel sends the route *as edited* to the backend's dry run.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import type { Route, RouteDecision, RouteRule, Settings } from "../api";
import { get, set } from "../store";
import { RoutersTab } from "./Routers";

const oldRoute: Route = { id: "chain", name: "Fallbacks", heads: [], all: true, steps: ["openai/gpt-5"], on_exhausted: "pause" };
const rule = (id: string, name: string): RouteRule => ({ id, name, enabled: true, match: "all", when: [{ kind: "plan" }], then: { kind: "use", model: "anthropic/opus", steps: 1, effort: null, turns: 0 }, note: "" });

const decision: RouteDecision = {
  model: "anthropic/opus", effort: 2, action: "use", rule_id: "rule-1", rule_name: "Plan", reason: "Plan: plan mode is on", hold: null, consume_stuck: false,
  trace: [
    { rule_id: "rule-1", rule_name: "Plan", enabled: true, matched: true, skipped: "", conditions: [{ kind: "plan", matched: true, detail: "plan mode is on" }] },
    { rule_id: "rule-2", rule_name: "Later", enabled: true, matched: false, skipped: "an earlier rule already matched", conditions: [] },
  ],
};

function store(routes: Route[]) {
  set({ settings: { routes, agents: [], paused_all: false, paused_reason: "" } as unknown as Settings, models: [], providers: [], accounts: [], agents: [], task: null, tasks: {}, settingsTab: "routing" });
}

/** The routes the last settings write carried. */
function savedRoutes(): Route[] {
  const call = [...invoke.mock.calls].reverse().find(([cmd]) => cmd === "settings_update");
  return (call?.[1] as { patch: { routes: Route[] } }).patch.routes;
}

const click = (name: string | RegExp) => fireEvent.click(screen.getByRole("button", { name }));
const save = async () => { await act(async () => { click("Save"); }); };

beforeEach(() => {
  (window as unknown as { matchMedia?: unknown }).matchMedia ??= () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  (Element.prototype as unknown as { getAnimations?: unknown }).getAnimations ??= () => [];
  invoke.mockImplementation(async (cmd: string, args?: { patch?: Partial<Settings> }) => {
    if (cmd === "settings_update") return { ...get().settings, ...args?.patch };
    if (cmd === "route_dry_run") return decision;
    return null;
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });

describe("the Routers page", () => {
  it("renders an old chain-only route without crashing", () => {
    store([oldRoute]);
    render(<RoutersTab />);
    expect((screen.getByRole("textbox", { name: "Router name" }) as HTMLInputElement).value).toBe("Fallbacks");
    expect(screen.getByText("No rules")).toBeTruthy();
    expect(screen.getByText("route/chain")).toBeTruthy();
    // Nothing is written just by looking.
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });

  it("adds a rule and saves it with the route's existing fields intact", async () => {
    store([oldRoute]);
    render(<RoutersTab />);
    click(/Add rule/);
    expect(screen.getByText("Unsaved changes")).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox", { name: "Rule name" }), { target: { value: "Plan on Opus" } });
    await save();
    const [r] = savedRoutes();
    expect(r).toMatchObject({ id: "chain", name: "Fallbacks", steps: ["openai/gpt-5"], on_exhausted: "pause", ladder: [], description: "" });
    expect(r!.rules).toEqual([{ id: "rule-1", name: "Plan on Opus", enabled: true, match: "all", when: [], then: { kind: "use", model: "", steps: 1, effort: null, turns: 0 }, note: "" }]);
    expect(screen.queryByText("Unsaved changes")).toBeNull();
  });

  it("duplicates, reorders, disables and deletes rules", async () => {
    store([{ ...oldRoute, rules: [rule("rule-1", "First"), rule("rule-2", "Second")] }]);
    render(<RoutersTab />);
    const first = () => document.querySelector<HTMLElement>('[data-rule="rule-1"]')!;

    fireEvent.click(within(first()).getByRole("button", { name: "Duplicate rule" }));
    await save();
    expect(savedRoutes()[0]!.rules!.map((r) => r.name)).toEqual(["First", "First copy", "Second"]);
    expect(new Set(savedRoutes()[0]!.rules!.map((r) => r.id)).size).toBe(3);

    fireEvent.click(within(first()).getByRole("button", { name: "Move rule down" }));
    await save();
    expect(savedRoutes()[0]!.rules!.map((r) => r.name)).toEqual(["First copy", "First", "Second"]);

    fireEvent.click(within(first()).getByRole("switch", { name: "Disable rule First" }));
    await save();
    expect(savedRoutes()[0]!.rules!.find((r) => r.id === "rule-1")!.enabled).toBe(false);

    // Delete is two-step, like everywhere else on the settings pages.
    fireEvent.click(within(first()).getByRole("button", { name: "Delete rule" }));
    expect(screen.queryByText("Unsaved changes")).toBeNull();
    fireEvent.click(within(first()).getByRole("button", { name: "Delete" }));
    await save();
    expect(savedRoutes()[0]!.rules!.map((r) => r.name)).toEqual(["First copy", "Second"]);
  });

  it("keeps unknown actions visible and clamps integer fields to Rust u32", async () => {
    store([{ ...oldRoute, rules: [{ ...rule("rule-1", "Future action"), then: { kind: "quantum" } }] }]);
    render(<RoutersTab />);
    fireEvent.click(screen.getByRole("button", { name: "Expand rule Future action" }));
    expect(screen.getByText(/doesn't know the action “quantum”/)).toBeTruthy();
    expect(screen.getByRole("radio", { name: "Unknown: quantum" })).toBeTruthy();
  });

  it("builds a condition and an action in the expanded rule", async () => {
    store([{ ...oldRoute, rules: [] }]);
    render(<RoutersTab />);
    click(/Add rule/);
    fireEvent.click(screen.getByRole("combobox", { name: "Add condition…" }));
    fireEvent.click(screen.getAllByRole("option").find((o) => o.textContent?.includes("Stuck on a failing call"))!);
    fireEvent.change(screen.getByRole("spinbutton", { name: "Count" }), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "bash" }));
    fireEvent.click(screen.getByRole("radio", { name: "Escalate" }));
    fireEvent.change(screen.getByRole("spinbutton", { name: "Hold turns" }), { target: { value: "2" } });
    expect(screen.getByText(/When stuck \(same call fails 3× in bash\) → escalate 1 step for 2 turns/)).toBeTruthy();
    await save();
    const r = savedRoutes()[0]!.rules![0]!;
    expect(r.when).toEqual([{ kind: "stuck", count: 3, tools: ["bash"] }]);
    expect(r.then).toMatchObject({ kind: "escalate", steps: 1, turns: 2 });
  });

  it("flags an invalid regex inline", () => {
    store([{ ...oldRoute, rules: [{ ...rule("rule-1", "Kw"), when: [{ kind: "keyword", pattern: "(oops", regex: true }] }] }]);
    render(<RoutersTab />);
    fireEvent.click(screen.getByRole("button", { name: "Expand rule Kw" }));
    expect(screen.getByRole("alert").textContent).toMatch(/Invalid regex/);
  });

  it("creates, duplicates and deletes routers", async () => {
    store([oldRoute]);
    render(<RoutersTab />);
    click(/New router/);
    await save();
    expect(savedRoutes().map((r) => r.id)).toEqual(["chain", "router"]);
    click("Duplicate router");
    await save();
    expect(savedRoutes().map((r) => r.id)).toEqual(["chain", "router", "router-2"]);
    click("Delete router");
    click("Delete");
    await save();
    expect(savedRoutes().map((r) => r.id)).toEqual(["chain", "router"]);
  });

  it("applies a preset to the current router", async () => {
    store([oldRoute]);
    render(<RoutersTab />);
    click(/Presets/);
    const row = screen.getByText("Plan with the strongest model").closest(".router-preset") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: /Add to/ }));
    await save();
    expect(savedRoutes()[0]!.rules!.map((r) => r.when[0]!.kind)).toEqual(["plan"]);
  });

  it("imports pasted JSON with fresh ids, and rejects garbage", async () => {
    store([oldRoute]);
    render(<RoutersTab />);
    click(/Import/);
    const box = screen.getByRole("textbox", { name: "Router JSON" });
    fireEvent.change(box, { target: { value: "[1]" } });
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Import" }));
    expect(screen.getByRole("alert").textContent).toMatch(/must be a JSON object/);
    fireEvent.change(box, { target: { value: JSON.stringify(oldRoute) } });
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Import" }));
    await save();
    expect(savedRoutes().map((r) => r.id)).toEqual(["chain", "chain-2"]);
  });

  it("dry-runs the edited, unsaved route and shows the decision and trace", async () => {
    store([{ ...oldRoute, rules: [rule("rule-1", "Plan"), rule("rule-2", "Later")] }]);
    render(<RoutersTab />);
    // An unsaved edit: the dry run must see it.
    fireEvent.click(within(document.querySelector<HTMLElement>('[data-rule="rule-2"]')!).getByRole("switch", { name: "Disable rule Later" }));
    fireEvent.click(screen.getByText("Test this router"));
    fireEvent.click(screen.getByRole("switch", { name: "Plan mode" }));
    fireEvent.change(screen.getByRole("spinbutton", { name: "Context tokens" }), { target: { value: "50000" } });
    await act(async () => { click("Run"); });
    const call = invoke.mock.calls.find(([cmd]) => cmd === "route_dry_run")!;
    const args = call[1] as { route: Route; situation: { plan: boolean; context_tokens: number; agent: string } };
    expect(args.route.rules!.find((r) => r.id === "rule-2")!.enabled).toBe(false);
    expect(args.situation).toMatchObject({ plan: true, context_tokens: 50000, agent: "main" });
    // Nothing was saved to run it.
    expect(invoke.mock.calls.some(([cmd]) => cmd === "settings_update")).toBe(false);
    const out = screen.getByLabelText("Router decision");
    expect(out.textContent).toContain("Plan: plan mode is on");
    expect(out.textContent).toContain("High");
    expect(out.textContent).toContain("matched");
    expect(out.textContent).toContain("skipped · an earlier rule already matched");
    expect(out.textContent).toContain("✓ plan mode is on");
  });

  it("shows a dry-run failure instead of a decision", async () => {
    invoke.mockImplementation(async (cmd: string) => { if (cmd === "route_dry_run") throw "backend says no"; return null; });
    store([oldRoute]);
    render(<RoutersTab />);
    fireEvent.click(screen.getByText("Test this router"));
    await act(async () => { click("Run"); });
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("backend says no"));
    expect(screen.queryByLabelText("Router decision")).toBeNull();
  });

  it("loads the situation from the open chat", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "route_situation") return { agent: "main", sub: false, turn: true, base_model: "openai/gpt-5", base_effort: 1, base_context: 400000, plan: false, images: true, context_tokens: 123456, user_text: "fix it", repeat_failures: 2, failing_tool: "bash", errors_in_row: 0, last_error_tool: "", no_progress_turns: 0, turns: 4, headroom: {}, budget_used_pct: null, minute_of_day: 60, weekday: 2, hold: null };
      return null;
    });
    store([oldRoute]);
    set({ task: "t1" });
    render(<RoutersTab />);
    fireEvent.click(screen.getByText("Test this router"));
    await act(async () => { click("Load from current chat"); });
    expect(invoke).toHaveBeenCalledWith("route_situation", { id: "t1", agent: null });
    expect((screen.getByRole("spinbutton", { name: "Context tokens" }) as HTMLInputElement).value).toBe("123456");
    expect((screen.getByRole("textbox", { name: "Failing tool" }) as HTMLInputElement).value).toBe("bash");
  });
});
