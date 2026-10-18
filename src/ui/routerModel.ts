// Routers: the pure half of the editor. Defaults, normalisation, ids and the
// one-line summaries the editor, the route menu and the composer chip share.
//
// Kept free of React and the store so the presets, the import path and the
// tests can all lean on exactly the same rules about what a router looks like.
import { EFFORTS, type Route, type RouteAction, type RouteCondition, type RouteRule } from "../api";

/** Condition kinds in the order the editor offers them (mirrors
 *  `CONDITION_KINDS` in `src-tauri/src/agent/routerules.rs`). */
export const CONDITION_KINDS = ["stuck", "errors_in_row", "no_progress", "agent", "context", "images", "plan", "keyword", "headroom", "budget", "time", "model", "turns", "always"] as const;

export const CONDITION_LABELS: Record<string, string> = {
  stuck: "Stuck on a failing call",
  errors_in_row: "Errors in a row",
  no_progress: "No progress",
  agent: "Agent",
  context: "Context size",
  images: "Has images",
  plan: "Plan mode",
  keyword: "Message contains",
  headroom: "Subscription headroom",
  budget: "Budget spent",
  time: "Time of day",
  model: "Base model",
  turns: "After N turns",
  always: "Always",
};

/** Supported requests that are not an agent's own turn. A rule only routes
 *  these when an Agent condition names them. Titles are set by the main
 *  agent's set_title tool, not a separate routed request. */
export const ROUTER_SYSTEM_AGENTS: { id: string; label: string }[] = [
  { id: "compactor", label: "Compaction" },
  { id: "summary", label: "Status summaries" },
  { id: "side", label: "Side questions" },
  { id: "commit-message", label: "Commit messages" },
  { id: "permission-review", label: "Permission reviewer" },
  { id: "sub-ask", label: "Subagent questions" },
];

/** Tools offered as chips on stuck / errors-in-a-row conditions. Any other
 *  name can still be typed in. */
export const COMMON_TOOLS = ["bash", "edit_file", "multi_edit", "write_file", "read_file", "grep", "glob", "web_fetch"];

export const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export const ACTION_KINDS: { value: string; label: string; hint: string }[] = [
  { value: "use", label: "Use model", hint: "Switch to a specific model" },
  { value: "escalate", label: "Escalate", hint: "Move up the ladder from the current model" },
  { value: "deescalate", label: "De-escalate", hint: "Move down the ladder from the current model" },
  { value: "effort", label: "Effort only", hint: "Keep the model, change the reasoning effort" },
  { value: "stay", label: "Stay", hint: "Keep the base model and stop evaluating rules" },
];

/** The default `count` the backend uses when a condition leaves it at 0. */
export function defaultCount(kind: string): number {
  return kind === "stuck" ? 2 : kind === "errors_in_row" ? 3 : kind === "no_progress" ? 5 : 1;
}

/** A fresh condition of a kind, with the fields that kind reads filled in. */
export function newCondition(kind: string): RouteCondition {
  switch (kind) {
    case "stuck": case "errors_in_row": return { kind, count: defaultCount(kind), tools: [] };
    case "no_progress": case "turns": return { kind, count: defaultCount(kind) };
    case "agent": return { kind, agents: ["subagent"] };
    case "context": return { kind, min_tokens: 0, max_tokens: 0, min_pct: 60 };
    case "keyword": return { kind, pattern: "", regex: false, case_sensitive: false };
    case "headroom": return { kind, provider: "", below_pct: 15 };
    case "budget": return { kind, above_pct: 80 };
    case "time": return { kind, from: "22:00", to: "07:00", days: [] };
    case "model": return { kind, models: [] };
    default: return { kind };
  }
}

export function newAction(kind = "use"): RouteAction {
  return { kind, model: "", steps: 1, effort: null, turns: 0 };
}

/** A unique id among `taken`, from a base: `base`, `base-2`, `base-3`… */
export function uniqueId(base: string, taken: Iterable<string>): string {
  const set = new Set(taken);
  const root = slug(base) || "router";
  if (!set.has(root)) return root;
  let n = 2;
  while (set.has(`${root}-${n}`)) n++;
  return `${root}-${n}`;
}

/** A rule id unique within a router, in the backend's `rule-N` form. */
export function nextRuleId(rules: readonly { id: string }[]): string {
  const taken = new Set(rules.map((r) => r.id));
  let n = rules.length + 1;
  while (taken.has(`rule-${n}`)) n++;
  return `rule-${n}`;
}

export function slug(s: string): string {
  return s.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
}

export function newRule(rules: readonly RouteRule[], over: Partial<RouteRule> = {}): RouteRule {
  return { id: nextRuleId(rules), name: "New rule", enabled: true, match: "all", when: [], then: newAction(), note: "", ...over };
}

/** Give each incoming rule an id unique among `existing` and each other. */
export function reId(existing: readonly RouteRule[], incoming: readonly RouteRule[]): RouteRule[] {
  const out: RouteRule[] = [];
  for (const r of incoming) {
    const all = [...existing, ...out];
    const id = r.id && !all.some((x) => x.id === r.id) ? r.id : nextRuleId(all);
    out.push({ ...r, id });
  }
  return out;
}

/** A route with the router fields filled in, for code that reads them. */
export function withRouterDefaults(r: Route): Route & { description: string; ladder: string[]; rules: RouteRule[] } {
  return { ...r, heads: r.heads ?? [], steps: r.steps ?? [], description: r.description ?? "", ladder: r.ladder ?? [], rules: r.rules ?? [] };
}

export const rulesOf = (r: Route): RouteRule[] => r.rules ?? [];

/** Move one entry of a list by `d`; out-of-range moves return the list unchanged. */
export function moveItem<T>(list: readonly T[], i: number, d: number): T[] {
  const j = i + d;
  if (i < 0 || i >= list.length || j < 0 || j >= list.length) return [...list];
  const out = [...list];
  [out[i], out[j]] = [out[j]!, out[i]!];
  return out;
}

// summaries

type NameOf = (model: string) => string;
const plain: NameOf = (m) => m.split("/").pop() || m;

const times = (n: number) => `${n}×`;
const listOf = (xs: readonly string[], none: string) => (xs.length ? xs.join(", ") : none);
const kTokens = (n: number) => (n >= 1000 ? `${Math.round(n / 1000)}k` : String(n));

/** One condition in words, e.g. "stuck (same call fails 2×)". */
export function conditionSummary(c: RouteCondition, name: NameOf = plain): string {
  const n = c.count && c.count > 0 ? c.count : defaultCount(c.kind);
  const tools = c.tools?.length ? ` in ${c.tools.join("/")}` : "";
  let s: string;
  switch (c.kind) {
    case "stuck": s = `stuck (same call fails ${times(n)}${tools})`; break;
    case "errors_in_row": s = `${n} errors in a row${tools}`; break;
    case "no_progress": s = `no successful edit for ${n} rounds`; break;
    case "turns": s = `after ${n} turn${n === 1 ? "" : "s"}`; break;
    case "agent": s = `agent is ${listOf(c.agents ?? [], "nobody")}`; break;
    case "context": {
      const parts: string[] = [];
      if (c.min_pct) parts.push(`≥ ${c.min_pct}% of the window`);
      if (c.min_tokens) parts.push(`≥ ${kTokens(c.min_tokens)} tokens`);
      if (c.max_tokens) parts.push(`≤ ${kTokens(c.max_tokens)} tokens`);
      s = `context ${parts.length ? parts.join(", ") : "any size"}`;
      break;
    }
    case "images": s = "the request has images"; break;
    case "plan": s = "plan mode is on"; break;
    case "keyword": s = c.pattern ? (c.regex ? `message matches /${c.pattern}/` : `message contains "${c.pattern}"`) : "message contains (no pattern)"; break;
    case "headroom": s = `${c.provider || "the model's subscription"} has < ${c.below_pct ?? 0}% left`; break;
    case "budget": s = `≥ ${c.above_pct ?? 0}% of the monthly budget spent`; break;
    case "time": {
      const days = c.days?.length ? ` on ${[...c.days].sort((a, b) => a - b).map((d) => WEEKDAYS[d] ?? `day ${d}`).join(", ")}` : "";
      s = `between ${c.from || "?"} and ${c.to || "?"}${days}`;
      break;
    }
    case "model": s = `base model is ${listOf((c.models ?? []).map((m) => (m.endsWith("*") ? m : name(m))), "(none)")}`; break;
    case "always": s = "always"; break;
    default: s = `${c.kind} (unknown condition)`;
  }
  return c.not ? `not ${s}` : s;
}

const effortWord = (e: number | null | undefined) => (e == null ? "" : EFFORTS[e] ?? `effort ${e}`);

/** The action in words, e.g. "escalate 1 step for 3 turns". */
export function actionSummary(a: RouteAction, name: NameOf = plain): string {
  const steps = a.steps && a.steps > 0 ? a.steps : 1;
  const eff = effortWord(a.effort);
  let s: string;
  switch (a.kind) {
    case "use": s = a.model ? `use ${name(a.model)}` : "use (pick a model)"; break;
    case "escalate": s = `escalate ${steps} step${steps === 1 ? "" : "s"}`; break;
    case "deescalate": s = `de-escalate ${steps} step${steps === 1 ? "" : "s"}`; break;
    case "effort": s = eff ? `set effort ${eff}` : "keep the model (pick an effort)"; break;
    case "stay": return "stay on the base model";
    default: s = `${a.kind} (unknown action)`;
  }
  if (eff && a.kind !== "effort") s += ` at ${eff}`;
  const t = a.turns ?? 0;
  if (t > 0) s += ` for ${t} turn${t === 1 ? "" : "s"}`;
  return s;
}

/** "When stuck (same call fails 2×) → escalate 1 step for 3 turns". */
export function ruleSummary(r: RouteRule, name: NameOf = plain): string {
  const conds = r.when.map((c) => conditionSummary(c, name));
  const when = conds.length ? `When ${conds.join(r.match === "any" ? " or " : " and ")}` : "Always";
  return `${when} → ${actionSummary(r.then, name)}`;
}

/** "3 rules · 2 fallbacks, then pause": what the route menu and the chip say. */
export function routeSummary(r: Route): string {
  const rules = rulesOf(r).filter((x) => x.enabled).length;
  const steps = r.steps?.length ?? 0;
  const parts: string[] = [];
  if (rules) parts.push(`${rules} rule${rules === 1 ? "" : "s"}`);
  parts.push(`${steps ? `${steps} fallback${steps === 1 ? "" : "s"}, then` : "then"} ${r.on_exhausted}`);
  return parts.join(" · ");
}

/** A router in the composer's route menu: its rule count, then the chain it
 *  falls back through — "2 rules · Sonnet then Haiku then pause". */
export function routeMenuSummary(r: Route, name: NameOf = plain): string {
  const chain = (r.steps ?? []).map(name).concat(r.on_exhausted).join(" then ");
  const rules = rulesOf(r).filter((x) => x.enabled).length;
  return rules ? `${rules} rule${rules === 1 ? "" : "s"} · ${chain}` : chain;
}

/** Whether a pattern compiles as a regular expression here. The backend uses
 *  Rust's `regex`, which rejects look-around and back-references that JS
 *  accepts, so a pattern can pass this and still fail there — the dry-run trace
 *  is the authority. Returns the error message, or null when it compiles. */
export function regexError(pattern: string): string | null {
  if (/\(\?<?[=!]/.test(pattern)) return "Look-around isn't supported by the router's regex engine";
  if (/\\[1-9]/.test(pattern)) return "Back-references aren't supported by the router's regex engine";
  try {
    new RegExp(pattern);
    return null;
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}

export const isHhmm = (s: string | undefined) => !!s && /^([01]?\d|2[0-3]):[0-5]\d$/.test(s.trim());
