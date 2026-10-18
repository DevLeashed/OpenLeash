// Routers: ready-made rule sets. Pure functions of the models the user has, so
// they can be unit-tested and so the editor can preview what a preset adds
// before it adds it. A preset only ever pre-fills: every model it picks is an
// ordinary field the user can change afterwards, and a pick it cannot make is
// left empty for the user rather than guessed at.
import type { ModelInfo, Route, RouteRule } from "../api";
import { newAction, newCondition, reId, uniqueId } from "./routerModel";

export interface PresetCtx {
  /** The models the user can pick from (enabled, on connected providers). */
  models: ModelInfo[];
  /** The model of the chat that is open, or the new-chat default. */
  current: string;
  /** The ladder of the router being edited, weakest → strongest; may be empty. */
  ladder: string[];
  /** Subscription pool kinds (providers backed by accounts), e.g. "claude". */
  subscriptions: string[];
}

export interface PresetResult { rules: RouteRule[]; ladder?: string[] }

export interface RouterPreset { id: string; name: string; description: string; build: (ctx: PresetCtx) => PresetResult }

const price = (m: ModelInfo) => (m.input_price || 0) + (m.output_price || 0);
const usable = (ctx: PresetCtx) => ctx.models.filter((m) => m.enabled !== false);
const CHEAP_NAME = /(haiku|mini|flash|nano|lite|small|fast)/i;

/** The cheapest model: the ladder's first rung, else the lowest-priced model,
 *  else one whose name says it is the small one. */
export function cheapModel(ctx: PresetCtx): string {
  if (ctx.ladder.length) return ctx.ladder[0]!;
  const priced = usable(ctx).filter((m) => price(m) > 0).sort((a, b) => price(a) - price(b));
  return priced[0]?.id ?? usable(ctx).find((m) => CHEAP_NAME.test(m.id))?.id ?? "";
}

/** The strongest model: the ladder's top rung, else the highest-priced model,
 *  else the current one. */
export function strongModel(ctx: PresetCtx): string {
  if (ctx.ladder.length) return ctx.ladder[ctx.ladder.length - 1]!;
  const priced = usable(ctx).filter((m) => price(m) > 0).sort((a, b) => price(b) - price(a));
  return priced[0]?.id ?? ctx.current;
}

/** The model with the largest context window, if it is larger than the current one's. */
export function longContextModel(ctx: PresetCtx): string {
  const cur = ctx.models.find((m) => m.id === ctx.current)?.context ?? 0;
  const best = [...usable(ctx)].sort((a, b) => b.context - a.context)[0];
  return best && best.context > cur ? best.id : "";
}

/** A model that reads images: the current one when it does, else the first that does. */
export function visionModel(ctx: PresetCtx): string {
  const sees = (m: ModelInfo) => m.input_types?.includes("image");
  const cur = ctx.models.find((m) => m.id === ctx.current);
  if (cur && sees(cur)) return cur.id;
  return usable(ctx).find(sees)?.id ?? "";
}

/** A pay-per-token model for when subscriptions run low: the same model name on
 *  an API provider when there is one, else the cheapest API model. */
export function apiModel(ctx: PresetCtx): string {
  const api = usable(ctx).filter((m) => !ctx.subscriptions.includes(m.provider));
  const tail = (id: string) => id.split("/").slice(1).join("/");
  return api.find((m) => tail(m.id) === tail(ctx.current))?.id
    ?? [...api].sort((a, b) => price(a) - price(b))[0]?.id ?? "";
}

/** The ladder a preset proposes when the router has none: cheap → current → strong. */
export function proposedLadder(ctx: PresetCtx): string[] {
  if (ctx.ladder.length) return ctx.ladder;
  return [...new Set([cheapModel(ctx), ctx.current, strongModel(ctx)].filter(Boolean))];
}

const rule = (i: number, r: Omit<RouteRule, "id" | "enabled" | "note"> & { note?: string }): RouteRule => ({ id: `rule-${i}`, enabled: true, note: "", ...r });

export const ROUTER_PRESETS: RouterPreset[] = [
  {
    id: "cheap-escalate",
    name: "Cheap by default, escalate when stuck",
    description: "Choose the cheapest ladder model as your chat's base model; these rules only escalate for a few turns when the agent keeps failing",
    build: (ctx) => {
      const ladder = proposedLadder(ctx);
      return {
        ladder,
        rules: [
          rule(1, { name: "Stuck → escalate", match: "all", when: [{ ...newCondition("stuck"), count: 2 }], then: { ...newAction("escalate"), steps: 1, turns: 3 } }),
          rule(2, { name: "Error streak → escalate", match: "all", when: [{ ...newCondition("errors_in_row"), count: 4 }], then: { ...newAction("escalate"), steps: 1, turns: 3 } }),
        ],
      };
    },
  },
  {
    id: "long-context",
    name: "Big context → long-context model",
    description: "Switch when the prompt fills most of the model's window",
    build: (ctx) => ({ rules: [rule(1, { name: "Big context", match: "all", when: [{ ...newCondition("context"), min_pct: 60 }], then: { ...newAction("use"), model: longContextModel(ctx) } })] }),
  },
  {
    id: "cheap-subagents",
    name: "Subagents on a cheap model",
    description: "Subagents use the cheap model; Explore also runs at Low effort",
    build: (ctx) => {
      const cheap = cheapModel(ctx);
      return {
        rules: [
          rule(1, { name: "Explore: cheap, Low effort", match: "all", when: [{ kind: "agent", agents: ["explore"] }], then: { ...newAction("use"), model: cheap, effort: 4 } }),
          rule(2, { name: "Subagents on cheap", match: "all", when: [{ kind: "agent", agents: ["subagent"] }], then: { ...newAction("use"), model: cheap } }),
        ],
      };
    },
  },
  {
    id: "plan-strong",
    name: "Plan with the strongest model",
    description: "Plan mode runs on the strongest model at High effort",
    build: (ctx) => ({ rules: [rule(1, { name: "Plan on strongest", match: "all", when: [newCondition("plan")], then: { ...newAction("use"), model: strongModel(ctx), effort: 2 } })] }),
  },
  {
    id: "images-vision",
    name: "Images → vision model",
    description: "Requests with images go to a model that reads them",
    build: (ctx) => ({ rules: [rule(1, { name: "Images → vision", match: "all", when: [newCondition("images")], then: { ...newAction("use"), model: visionModel(ctx) } })] }),
  },
  {
    id: "save-headroom",
    name: "Save subscription headroom",
    description: "Below 15% left on the subscription, use a pay-per-token model",
    build: (ctx) => ({ rules: [rule(1, { name: "Low headroom → API", match: "all", when: [{ ...newCondition("headroom"), below_pct: 15 }], then: { ...newAction("use"), model: apiModel(ctx) } })] }),
  },
  {
    id: "cheap-system",
    name: "Cheap summaries",
    description: "Compaction and status summaries use the cheap model",
    build: (ctx) => ({ rules: [rule(1, { name: "Cheap housekeeping", match: "all", when: [{ kind: "agent", agents: ["compactor", "summary"] }], then: { ...newAction("use"), model: cheapModel(ctx) } })] }),
  },
  {
    id: "off-hours",
    name: "Off-hours",
    description: "Overnight (22:00–07:00), use a cheaper model",
    build: (ctx) => ({ rules: [rule(1, { name: "Off-hours → cheaper", match: "all", when: [newCondition("time")], then: { ...newAction("use"), model: cheapModel(ctx) } })] }),
  },
];

/** Append a preset's rules to a router (ids made unique), adopting its ladder
 *  only when the router has none — a ladder the user built is never replaced. */
export function applyPreset(route: Route, preset: RouterPreset, ctx: PresetCtx): Route {
  const out = preset.build({ ...ctx, ladder: route.ladder?.length ? route.ladder : ctx.ladder });
  const rules = route.rules ?? [];
  return {
    ...route,
    ladder: route.ladder?.length ? route.ladder : out.ladder ?? route.ladder ?? [],
    rules: [...rules, ...reId(rules, out.rules)],
  };
}

/** A new router made of one preset, with an id not in `taken`. */
export function routerFromPreset(preset: RouterPreset, ctx: PresetCtx, taken: readonly string[], all = false): Route {
  const base: Route = { id: uniqueId(preset.id, taken), name: preset.name, heads: [], all, steps: [], on_exhausted: "pause", description: preset.description, ladder: [], rules: [] };
  return applyPreset(base, preset, ctx);
}
