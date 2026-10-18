import { describe, expect, it } from "vitest";
import type { Route } from "../api";
import { exportRouters, normalizeRoute, parseRouters, RouterImportError } from "./routerIo";

const router: Route = {
  id: "smart", name: "Smart", heads: ["anthropic/opus"], all: false, steps: ["anthropic/sonnet"], on_exhausted: "fail",
  description: "cheap first", ladder: ["anthropic/haiku", "anthropic/opus"],
  rules: [
    { id: "rule-1", name: "Stuck", enabled: true, match: "all", when: [{ kind: "stuck", count: 2, tools: ["bash"] }], then: { kind: "escalate", model: "", steps: 1, effort: null, turns: 3 }, note: "" },
    { id: "rule-2", name: "Nights", enabled: false, match: "any", when: [{ kind: "time", from: "22:00", to: "07:00", days: [0, 6] }, { kind: "keyword", pattern: "quick", regex: false, case_sensitive: false, not: true }], then: { kind: "use", model: "anthropic/haiku", steps: 1, effort: 4, turns: 0 }, note: "n" },
  ],
};

describe("router import and export", () => {
  it("round-trips a router through export and import", () => {
    const { routes, renamed } = parseRouters(exportRouters([router]), []);
    expect(renamed).toEqual([]);
    expect(routes).toEqual([router]);
  });

  it("round-trips several routers through the { routes } shape", () => {
    const two = [router, { ...router, id: "other", name: "Other" }];
    const text = exportRouters(two);
    expect(JSON.parse(text)).toHaveProperty("routes");
    expect(parseRouters(text, []).routes).toEqual(two);
  });

  it("assigns fresh ids on conflict, with existing routers and within the paste", () => {
    const text = JSON.stringify([router, router]);
    const { routes, renamed } = parseRouters(text, ["smart"]);
    expect(routes.map((r) => r.id)).toEqual(["smart-2", "smart-3"]);
    expect(renamed).toEqual([{ from: "smart", to: "smart-2" }, { from: "smart", to: "smart-3" }]);
  });

  it("replaces ids that can't be a route/<id> and fills missing ones from the name", () => {
    const { routes } = parseRouters(JSON.stringify([{ id: "a/b c", name: "X" }, { name: "Night Owl" }]), []);
    expect(routes.map((r) => r.id)).toEqual(["a-b-c", "night-owl"]);
  });

  it("imports an old chain-only route with router defaults", () => {
    const old = { id: "chain", name: "Fallbacks", heads: [], all: true, steps: ["openai/gpt-5"], on_exhausted: "pause" };
    const { routes } = parseRouters(JSON.stringify(old), []);
    expect(routes[0]).toEqual({ ...old, description: "", ladder: [], rules: [] });
  });

  it("rejects garbage with a clear message", () => {
    const err = (t: string) => { try { parseRouters(t, []); return ""; } catch (e) { expect(e).toBeInstanceOf(RouterImportError); return (e as Error).message; } };
    expect(err("not json")).toMatch(/Not valid JSON/);
    expect(err("42")).toMatch(/must be a JSON object, not number/);
    expect(err("[1, {}]")).toMatch(/Item 1 must be a JSON object/);
    expect(err("null")).toMatch(/not null/);
    expect(err("[]")).toMatch(/no routers/);
    expect(err(JSON.stringify({ name: "R", rules: "nope" }))).toMatch(/"rules" must be a list/);
    expect(err(JSON.stringify({ name: "R", rules: ["x"] }))).toMatch(/a rule must be an object/);
    expect(err(JSON.stringify({ name: "R", rules: [{ when: [{}] }] }))).toMatch(/has no "kind"/);
  });

  it("clamps numbers to what the backend's unsigned fields accept", () => {
    const r = normalizeRoute({ id: "x", name: "X", rules: [{ id: "a", when: [{ kind: "stuck", count: -3.7 }, { kind: "time", days: [1, 9, -1, 2.5] }], then: { kind: "escalate", steps: 0, effort: 7, turns: 2.9 } }] });
    const rule = r.rules![0]!;
    expect(rule.when[0]!.count).toBe(0);
    expect(rule.when[1]!.days).toEqual([1]);
    expect(rule.then).toEqual({ kind: "escalate", model: "", steps: 1, effort: null, turns: 2 });
  });

  it("bounds huge imported integer fields to Rust u32 range", () => {
    const r = normalizeRoute({ rules: [{ when: [{ kind: "stuck", count: 1e100 }], then: { kind: "escalate", steps: 1e100, turns: 1e100 } }] });
    expect(r.rules![0]!.when[0]!.count).toBe(0xffffffff);
    expect(r.rules![0]!.then.steps).toBe(0xffffffff);
    expect(r.rules![0]!.then.turns).toBe(0xffffffff);
  });

  it("drops fields a kind doesn't read, keeps unknown kinds, and repairs rule ids", () => {
    const r = normalizeRoute({ id: "x", name: "X", rules: [
      { id: "dup", match: "weird", when: [{ kind: "plan", count: 4, pattern: "x" }, { kind: "future", level: 3, tags: ["a"], nested: { a: 1 } }], then: {} },
      { id: "dup", when: [] },
    ] });
    expect(r.rules![0]!.match).toBe("all");
    expect(r.rules![0]!.when).toEqual([{ kind: "plan" }, { kind: "future", level: 3, tags: ["a"] }]);
    expect(r.rules![0]!.then.kind).toBe("use");
    expect(r.rules!.map((x) => x.id)).toEqual(["dup", "rule-3"]);
    expect(r.rules![1]!.enabled).toBe(true);
  });
});
