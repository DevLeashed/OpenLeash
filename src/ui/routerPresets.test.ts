import { describe, expect, it } from "vitest";
import type { ModelInfo, Route } from "../api";
import { CONDITION_KINDS } from "./routerModel";
import { normalizeRoute } from "./routerIo";
import { apiModel, applyPreset, cheapModel, longContextModel, proposedLadder, ROUTER_PRESETS, routerFromPreset, strongModel, visionModel, type PresetCtx } from "./routerPresets";

const model = (id: string, over: Partial<ModelInfo> = {}): ModelInfo => ({
  id, name: id.split("/")[1] ?? id, provider: id.split("/")[0] ?? "", context: 200_000, output: 32_000, input_price: 1, output_price: 5, effort: true,
  input_types: ["text"], capabilities: [], reasoning_levels: [], reasoning_param: "none", custom: false, enabled: true, ...over,
});

const models = [
  model("anthropic/haiku", { input_price: 1, output_price: 5 }),
  model("anthropic/sonnet", { input_price: 3, output_price: 15, input_types: ["text", "image"] }),
  model("anthropic/opus", { input_price: 15, output_price: 75 }),
  model("google/gemini-pro", { input_price: 2, output_price: 10, context: 1_000_000 }),
  model("claude/sonnet", { input_price: 0, output_price: 0 }),
];
const ctx: PresetCtx = { models, current: "claude/sonnet", ladder: [], subscriptions: ["claude"] };

describe("preset model picks", () => {
  it("picks from the user's models", () => {
    expect(cheapModel(ctx)).toBe("anthropic/haiku");
    expect(strongModel(ctx)).toBe("anthropic/opus");
    expect(longContextModel(ctx)).toBe("google/gemini-pro");
    expect(visionModel(ctx)).toBe("anthropic/sonnet");
    // The same model on a pay-per-token provider beats a cheaper different one.
    expect(apiModel(ctx)).toBe("anthropic/sonnet");
    expect(proposedLadder(ctx)).toEqual(["anthropic/haiku", "claude/sonnet", "anthropic/opus"]);
  });

  it("prefers the router's own ladder for cheap and strong", () => {
    const l = { ...ctx, ladder: ["google/gemini-pro", "anthropic/sonnet"] };
    expect(cheapModel(l)).toBe("google/gemini-pro");
    expect(strongModel(l)).toBe("anthropic/sonnet");
    expect(proposedLadder(l)).toEqual(l.ladder);
  });

  it("leaves a pick empty rather than guessing when nothing fits", () => {
    const none: PresetCtx = { models: [], current: "x/y", ladder: [], subscriptions: [] };
    expect(cheapModel(none)).toBe("");
    expect(longContextModel(none)).toBe("");
    expect(visionModel(none)).toBe("");
    expect(apiModel(none)).toBe("");
    expect(strongModel(none)).toBe("x/y");
  });
});

describe("router presets", () => {
  it.each(ROUTER_PRESETS.map((p) => [p.id, p] as const))("%s builds valid, normalised rules", (_, p) => {
    const out = p.build(ctx);
    expect(out.rules.length).toBeGreaterThan(0);
    const ids = out.rules.map((r) => r.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const r of out.rules) {
      expect(r.name).toBeTruthy();
      expect(r.enabled).toBe(true);
      for (const c of r.when) expect(CONDITION_KINDS).toContain(c.kind);
      expect(["use", "escalate", "deescalate", "effort", "stay"]).toContain(r.then.kind);
    }
    // What a preset writes is exactly what the import/save normaliser keeps.
    const route: Route = { id: "x", name: "X", heads: [], all: true, steps: [], on_exhausted: "pause", description: "", ladder: out.ladder ?? [], rules: out.rules };
    expect(normalizeRoute(route)).toEqual(route);
  });

  it("covers the presets the editor promises", () => {
    const byId = Object.fromEntries(ROUTER_PRESETS.map((p) => [p.id, p.build(ctx)]));
    expect(byId["cheap-escalate"]!.rules.map((r) => [r.when[0]!.kind, r.when[0]!.count, r.then.kind, r.then.turns])).toEqual([["stuck", 2, "escalate", 3], ["errors_in_row", 4, "escalate", 3]]);
    expect(byId["long-context"]!.rules[0]!.when[0]!.min_pct).toBe(60);
    expect(byId["cheap-subagents"]!.rules.map((r) => r.when[0]!.agents)).toEqual([["explore"], ["subagent"]]);
    expect(byId["cheap-subagents"]!.rules[0]!.then.effort).toBe(4);
    expect(byId["plan-strong"]!.rules[0]!.then).toMatchObject({ kind: "use", model: "anthropic/opus", effort: 2 });
    expect(byId["save-headroom"]!.rules[0]!.when[0]).toMatchObject({ kind: "headroom", below_pct: 15 });
    expect(byId["cheap-system"]!.rules[0]!.when[0]!.agents).toEqual(["compactor", "summary"]);
    expect(ROUTER_PRESETS.find((p) => p.id === "cheap-system")!.name).toBe("Cheap summaries");
    expect(ROUTER_PRESETS.find((p) => p.id === "cheap-escalate")!.description).toContain("Choose the cheapest ladder model as your chat's base model");
    expect(byId["off-hours"]!.rules[0]!.when[0]!.kind).toBe("time");
  });

  it("appends to an existing router with fresh ids and keeps its ladder", () => {
    const base: Route = { id: "r", name: "R", heads: [], all: true, steps: [], on_exhausted: "pause", ladder: ["anthropic/sonnet", "anthropic/opus"], rules: [{ id: "rule-1", name: "Mine", enabled: true, match: "all", when: [], then: { kind: "stay" } }] };
    const out = applyPreset(base, ROUTER_PRESETS[0]!, ctx);
    expect(out.ladder).toEqual(base.ladder);
    expect(out.rules!.map((r) => r.id)).toEqual(["rule-1", "rule-2", "rule-3"]);
    expect(out.rules![0]!.name).toBe("Mine");
  });

  it("creates a new router with an id that doesn't clash", () => {
    const r = routerFromPreset(ROUTER_PRESETS[0]!, ctx, ["cheap-escalate"]);
    expect(r.id).toBe("cheap-escalate-2");
    expect(r.ladder).toEqual(["anthropic/haiku", "claude/sonnet", "anthropic/opus"]);
    expect(r.rules).toHaveLength(2);
  });
});
