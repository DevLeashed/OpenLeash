import { describe, expect, it } from "vitest";
import type { RouteRule } from "../api";
import { actionSummary, conditionSummary, CONDITION_KINDS, isHhmm, moveItem, newCondition, newRule, nextRuleId, regexError, reId, routeMenuSummary, routeSummary, ROUTER_SYSTEM_AGENTS, ruleSummary, uniqueId, withRouterDefaults } from "./routerModel";

const rule = (over: Partial<RouteRule> = {}): RouteRule => ({ id: "rule-1", name: "r", enabled: true, match: "all", when: [], then: { kind: "use", model: "" }, ...over });

describe("router summaries", () => {
  it("does not offer title routing, but still describes imported title conditions", () => {
    expect(ROUTER_SYSTEM_AGENTS.map((a) => a.id)).not.toContain("title");
    expect(conditionSummary({ kind: "agent", agents: ["title"] })).toBe("agent is title");
  });
  it("reads a stuck → escalate rule as one sentence", () => {
    const r = rule({ when: [{ kind: "stuck", count: 2 }], then: { kind: "escalate", steps: 1, turns: 3 } });
    expect(ruleSummary(r)).toBe("When stuck (same call fails 2×) → escalate 1 step for 3 turns");
  });

  it("joins conditions by the match mode and shows negation, effort and models", () => {
    const r = rule({ match: "any", when: [{ kind: "plan" }, { kind: "agent", agents: ["explore"], not: true }], then: { kind: "use", model: "anthropic/claude-opus-5", effort: 2 } });
    expect(ruleSummary(r, (m) => m.toUpperCase())).toBe("When plan mode is on or not agent is explore → use ANTHROPIC/CLAUDE-OPUS-5 at High");
  });

  it("uses the backend's default counts when a count is unset", () => {
    expect(conditionSummary({ kind: "errors_in_row" })).toBe("3 errors in a row");
    expect(conditionSummary({ kind: "no_progress", count: 0 })).toBe("no successful edit for 5 rounds");
    expect(conditionSummary({ kind: "stuck", tools: ["bash"] })).toBe("stuck (same call fails 2× in bash)");
  });

  it("describes every condition kind without throwing, and flags unknown ones", () => {
    for (const k of CONDITION_KINDS) expect(conditionSummary(newCondition(k))).not.toContain("unknown");
    expect(conditionSummary({ kind: "moon_phase" })).toContain("unknown");
    expect(conditionSummary({ kind: "time", from: "22:00", to: "07:00", days: [5, 6] })).toBe("between 22:00 and 07:00 on Sat, Sun");
  });

  it("describes actions", () => {
    expect(actionSummary({ kind: "stay" })).toBe("stay on the base model");
    expect(actionSummary({ kind: "effort", effort: 4 })).toBe("set effort Low");
    expect(actionSummary({ kind: "deescalate", steps: 2 })).toBe("de-escalate 2 steps");
    expect(ruleSummary(rule())).toBe("Always → use (pick a model)");
  });

  it("counts enabled rules and fallbacks for the route menu and chip", () => {
    const r = { id: "a", name: "A", heads: [], all: true, steps: ["x/a", "x/b"], on_exhausted: "pause" as const, rules: [rule(), rule({ id: "rule-2" }), rule({ id: "rule-3", enabled: false })] };
    expect(routeSummary(r)).toBe("2 rules · 2 fallbacks, then pause");
    expect(routeSummary({ ...r, rules: undefined, steps: [], on_exhausted: "fail" })).toBe("then fail");
    expect(routeMenuSummary(r)).toBe("2 rules · a then b then pause");
    expect(routeMenuSummary({ ...r, rules: [] })).toBe("a then b then pause");
  });
});

describe("router ids and lists", () => {
  it("makes unique route and rule ids", () => {
    expect(uniqueId("My Router!", [])).toBe("my-router");
    expect(uniqueId("router", ["router", "router-2"])).toBe("router-3");
    expect(uniqueId("", [])).toBe("router");
    expect(nextRuleId([{ id: "rule-1" }, { id: "rule-2" }])).toBe("rule-3");
    expect(newRule([{ ...rule(), id: "rule-2" }]).id).toBe("rule-3");
  });

  it("re-ids incoming rules against existing ones and each other", () => {
    const out = reId([rule({ id: "rule-1" })], [rule({ id: "rule-1" }), rule({ id: "rule-1" }), rule({ id: "keep" })]);
    expect(out.map((r) => r.id)).toEqual(["rule-2", "rule-3", "keep"]);
  });

  it("moves an item and ignores out-of-range moves", () => {
    expect(moveItem(["a", "b", "c"], 0, 1)).toEqual(["b", "a", "c"]);
    expect(moveItem(["a", "b"], 0, -1)).toEqual(["a", "b"]);
  });

  it("fills router fields missing from an old chain-only route", () => {
    const r = withRouterDefaults({ id: "chain", name: "Chain", heads: [], all: true, steps: [], on_exhausted: "pause" });
    expect(r.rules).toEqual([]);
    expect(r.ladder).toEqual([]);
    expect(r.description).toBe("");
  });

  it("validates regexes and times the way the backend will read them", () => {
    expect(regexError("refactor|rewrite")).toBeNull();
    expect(regexError("(unclosed")).toBeTruthy();
    expect(regexError("foo(?=bar)")).toMatch(/Look-around/);
    expect(isHhmm("07:30")).toBe(true);
    expect(isHhmm("24:00")).toBe(false);
  });
});
