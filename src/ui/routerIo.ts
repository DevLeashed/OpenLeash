// Routers: import, export and the normalisation both go through.
//
// Import is untrusted input — pasted from a chat, a gist, another machine — and
// what it produces is written to settings.json, where the Rust side
// deserialises it with serde. A negative or fractional number in a field Rust
// reads as `u32` would make the whole settings write fail, so every number is
// clamped to the shape the backend accepts here rather than trusted.
import type { Route, RouteAction, RouteCondition, RouteRule } from "../api";
import { uniqueId, nextRuleId } from "./routerModel";

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
const str = (v: unknown, d = ""): string => (typeof v === "string" ? v : d);
const bool = (v: unknown, d = false): boolean => (typeof v === "boolean" ? v : d);
const strs = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);
/** A non-negative integer (Rust `u32`/`u64`), or the default. */
const uint = (v: unknown, d = 0): number => (typeof v === "number" && Number.isFinite(v) ? Math.min(0xffffffff, Math.max(0, Math.floor(v))) : d);
/** A non-negative float (Rust `f64` percentages), or the default. */
const pct = (v: unknown, d = 0): number => (typeof v === "number" && Number.isFinite(v) ? Math.max(0, v) : d);

export class RouterImportError extends Error {}

function condition(v: unknown, where: string): RouteCondition {
  if (!isObj(v)) throw new RouterImportError(`${where}: a condition must be an object`);
  const kind = str(v.kind).trim();
  if (!kind) throw new RouterImportError(`${where}: a condition has no "kind"`);
  const c: RouteCondition = { kind };
  if (v.not === true) c.not = true;
  // Keep only the fields this kind reads: a pasted rule with stray fields from
  // another kind would otherwise ride along forever, invisible in the editor.
  switch (kind) {
    case "stuck": case "errors_in_row": c.count = uint(v.count); c.tools = strs(v.tools); break;
    case "no_progress": case "turns": c.count = uint(v.count); break;
    case "agent": c.agents = strs(v.agents); break;
    case "context": c.min_tokens = uint(v.min_tokens); c.max_tokens = uint(v.max_tokens); c.min_pct = pct(v.min_pct); break;
    case "keyword": c.pattern = str(v.pattern); c.regex = bool(v.regex); c.case_sensitive = bool(v.case_sensitive); break;
    case "headroom": c.provider = str(v.provider); c.below_pct = pct(v.below_pct); break;
    case "budget": c.above_pct = pct(v.above_pct); break;
    case "time": c.from = str(v.from); c.to = str(v.to); c.days = (Array.isArray(v.days) ? v.days : []).filter((d): d is number => Number.isInteger(d) && d >= 0 && d <= 6); break;
    case "model": c.models = strs(v.models); break;
    case "images": case "plan": case "always": break;
    default: {
      // A kind from a newer build: keep its plain fields so a round trip
      // through this build does not strip what that build needs.
      for (const [k, x] of Object.entries(v)) {
        if (k === "kind" || k === "not") continue;
        if (typeof x === "string" || typeof x === "boolean" || (typeof x === "number" && Number.isFinite(x) && x >= 0)) (c as unknown as Obj)[k] = x;
        else if (Array.isArray(x) && x.every((y) => typeof y === "string")) (c as unknown as Obj)[k] = x;
      }
    }
  }
  return c;
}

function action(v: unknown): RouteAction {
  const o = isObj(v) ? v : {};
  const e = o.effort;
  return {
    kind: str(o.kind).trim() || "use",
    model: str(o.model),
    steps: uint(o.steps, 1) || 1,
    effort: typeof e === "number" && Number.isInteger(e) && e >= 0 && e <= 4 ? e : null,
    turns: uint(o.turns),
  };
}

function rule(v: unknown, where: string): RouteRule {
  if (!isObj(v)) throw new RouterImportError(`${where}: a rule must be an object`);
  const name = str(v.name);
  const at = `${where}, rule "${name || "unnamed"}"`;
  return {
    id: str(v.id),
    name,
    enabled: bool(v.enabled, true),
    match: v.match === "any" ? "any" : "all",
    when: (Array.isArray(v.when) ? v.when : []).map((c) => condition(c, at)),
    then: action(v.then),
    note: str(v.note),
  };
}

/** Rule ids unique within the router; missing or repeated ones get `rule-N`. */
function fixRuleIds(rules: RouteRule[]): RouteRule[] {
  const out: RouteRule[] = [];
  for (const r of rules) out.push(r.id && !out.some((x) => x.id === r.id) ? r : { ...r, id: nextRuleId([...out, ...rules.filter((x) => x !== r)]) });
  return out;
}

/** One route from untrusted JSON, with every field present and well-typed.
 *  Throws `RouterImportError` with a message fit to show the user. */
export function normalizeRoute(v: unknown, where = "Router"): Route {
  if (!isObj(v)) throw new RouterImportError(`${where} must be a JSON object, not ${v === null ? "null" : Array.isArray(v) ? "an array" : typeof v}`);
  const name = str(v.name).trim();
  const at = name ? `Router "${name}"` : where;
  if (v.rules !== undefined && !Array.isArray(v.rules)) throw new RouterImportError(`${at}: "rules" must be a list`);
  return {
    id: str(v.id).trim(),
    name: name || "Untitled router",
    heads: strs(v.heads),
    all: bool(v.all),
    steps: strs(v.steps),
    on_exhausted: v.on_exhausted === "fail" ? "fail" : "pause",
    description: str(v.description),
    ladder: strs(v.ladder),
    rules: fixRuleIds((Array.isArray(v.rules) ? v.rules : []).map((r) => rule(r, at))),
  };
}

export interface ImportResult { routes: Route[]; renamed: { from: string; to: string }[] }

/**
 * Parse pasted router JSON: one router object, a list of them, or the
 * `{ "routes": [...] }` shape an export of every router writes. Ids that clash
 * with `taken` (the routers already saved) or with each other are replaced, and
 * each replacement is reported so the dialog can say so.
 */
export function parseRouters(text: string, taken: readonly string[]): ImportResult {
  let data: unknown;
  try {
    data = JSON.parse(text);
  } catch (e) {
    throw new RouterImportError(`Not valid JSON: ${e instanceof Error ? e.message : String(e)}`);
  }
  if (isObj(data) && Array.isArray(data.routes)) data = data.routes;
  const list = Array.isArray(data) ? data : [data];
  if (!list.length) throw new RouterImportError("There are no routers in that JSON");
  const ids = [...taken];
  const renamed: ImportResult["renamed"] = [];
  const routes = list.map((v, i) => {
    const r = normalizeRoute(v, list.length > 1 ? `Item ${i + 1}` : "The JSON");
    // An id is also the `route/<id>` a chat names, so one with a slash or a
    // space in it is replaced the same way as one that is already taken.
    const id = r.id && /^[A-Za-z0-9_-]+$/.test(r.id) && !ids.includes(r.id) ? r.id : uniqueId(r.id || r.name, ids);
    if (r.id && id !== r.id) renamed.push({ from: r.id, to: id });
    ids.push(id);
    return { ...r, id };
  });
  return { routes, renamed };
}

/** Export JSON: a single router as itself, several as `{ routes: [...] }`. */
export function exportRouters(routes: readonly Route[]): string {
  const clean = routes.map((r) => normalizeRoute(r));
  return JSON.stringify(clean.length === 1 ? clean[0] : { routes: clean }, null, 2);
}
