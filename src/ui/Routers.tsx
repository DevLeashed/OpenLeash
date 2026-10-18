// Settings → Routers: per-request model routing rules in front of a fallback
// chain. The pure parts (defaults, summaries, presets, import/export) live in
// routerModel.ts, routerPresets.ts and routerIo.ts; this file is the editor.
//
// Edits collect in a local draft of the whole routes list and are written with
// one Save. A router is many small fields, and writing settings.json on every
// keystroke of a number field would round-trip half-typed values through the
// backend — and the test panel is meant to try rules *before* they go live.
import { useMemo, useState } from "react";
import { api, EFFORTS, type ModelInfo, type Route, type RouteAction, type RouteCondition, type RouteDecision, type RouteRule, type RouteSituation } from "../api";
import { flash, isConnected, modelInfo, saveSettings, shownProviders, useStore } from "../store";
import { copyText } from "./clipboard";
import { I } from "./icons";
import { ModelSelect } from "./Accounts";
import { Button, ChoiceChip, Dropdown, IconButton, Input, Loader, Modal, Pressable, RemovableChip, Segmented, Switch, TextArea, TextButton, Tooltip, type Opt } from "./primitives";
import { ArrowDown, ArrowUp, ChevronDown, ChevronRight, ChevronUp, Copy, Download, FlaskConical, Upload, WandSparkles } from "lucide-react";
import { ACTION_KINDS, COMMON_TOOLS, CONDITION_KINDS, CONDITION_LABELS, conditionSummary, defaultCount, isHhmm, moveItem, newCondition, newRule, regexError, routeSummary, ROUTER_SYSTEM_AGENTS, ruleSummary, uniqueId, WEEKDAYS } from "./routerModel";
import { exportRouters, normalizeRoute, parseRouters, RouterImportError } from "./routerIo";
import { applyPreset, ROUTER_PRESETS, routerFromPreset, type PresetCtx, type RouterPreset } from "./routerPresets";

// Stable fallbacks for selectors (a fresh [] each render loops useSyncExternalStore).
const NO_ROUTES: Route[] = [];
const NO_RULES: RouteRule[] = [];
const NO_STRS: string[] = [];

const mname = (id: string) => (id ? modelInfo(id).name : "");
const SMALL = { width: 24, height: 24 } as const;

/** What a preset may pick from: the models the user can actually select. */
function usePresetCtx(ladder: string[]): PresetCtx {
  const models = useStore((s) => s.models);
  const provs = useStore((s) => s.providers);
  const current = useStore((s) => (s.task && s.tasks[s.task] ? s.tasks[s.task]!.pending.model ?? s.tasks[s.task]!.model : s.home.model));
  useStore((s) => s.accounts);
  return useMemo(() => {
    const live = shownProviders(provs).filter((p) => p.enabled && isConnected(p));
    const ids = new Set(live.map((p) => p.id));
    return { models: models.filter((m: ModelInfo) => ids.has(m.provider) && m.enabled !== false), current, ladder, subscriptions: provs.filter((p) => p.account).map((p) => p.id) };
  }, [models, provs, current, ladder]);
}

/** Pool kinds a headroom condition (and the test panel) can name. */
function usePoolKinds(): string[] {
  const accounts = useStore((s) => s.accounts);
  return useMemo(() => [...new Set(accounts.map((a) => a.kind))], [accounts]);
}

// small fields

const MAX_ROUTER_UINT = 0xffff_ffff;

/** A non-negative number. Blank reads as 0, which most fields treat as "unset". */
function NumField({ value, onChange, label, placeholder, float = false, max = MAX_ROUTER_UINT, width = 72 }: { value: number | undefined; onChange: (v: number) => void; label: string; placeholder?: string; float?: boolean; max?: number; width?: number }) {
  return (
    <Input className="input router-num" type="number" min={0} max={max} step={float ? "any" : 1} aria-label={label} placeholder={placeholder} style={{ width }}
      value={value ? String(value) : ""}
      onChange={(e) => {
        const n = Number(e.currentTarget.value);
        const v = Number.isFinite(n) ? Math.max(0, float ? n : Math.floor(n)) : 0;
        onChange(max !== undefined ? Math.min(max, v) : v);
      }} />
  );
}

function Field({ label, children, wide = false }: { label: string; children: React.ReactNode; wide?: boolean }) {
  // A div, not a <label>: a label wrapping chips would click the first chip
  // whenever its caption was clicked.
  return <div className={"router-field" + (wide ? " wide" : "")} role="group" aria-label={label}><span className="router-field-label">{label}</span><div className="router-field-ctl">{children}</div></div>;
}

function EffortSelect({ value, onChange, none }: { value: number | null | undefined; onChange: (v: number | null) => void; none?: string }) {
  const opts: Opt[] = [...(none !== undefined ? [{ value: "", label: none }] : []), ...EFFORTS.map((label, i) => ({ value: String(i), label }))];
  return <Dropdown value={value == null ? "" : String(value)} options={opts} search={false} placeholder="Effort" onChange={(v) => onChange(v === "" ? null : Number(v))} style={{ width: 140, flex: "none" }} />;
}

/** A list of models as removable chips plus a picker. */
function ModelChips({ models, onChange, placeholder, patterns = false }: { models: string[]; onChange: (v: string[]) => void; placeholder: string; patterns?: boolean }) {
  const [pat, setPat] = useState("");
  const add = (v: string) => { const t = v.trim(); if (t && !models.includes(t)) onChange([...models, t]); };
  return (
    <div className="router-chips">
      {models.map((m) => <RemovableChip key={m} label={m.endsWith("*") ? m : mname(m)} onRemove={() => onChange(models.filter((x) => x !== m))}>{m.endsWith("*") ? <span className="mono">{m}</span> : mname(m)}</RemovableChip>)}
      <div className="route-add-model"><ModelSelect value="" placeholder={placeholder} onChange={add} /></div>
      {patterns && (
        <Input className="input mono" aria-label="Model id pattern" placeholder="provider/prefix*" value={pat} style={{ width: 150, height: 26, fontSize: 11.5 }}
          onChange={(e) => setPat(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === "Enter") { add(pat); setPat(""); } }} />
      )}
    </div>
  );
}

// conditions

function ToolChips({ tools, onChange }: { tools: string[]; onChange: (v: string[]) => void }) {
  const [extra, setExtra] = useState("");
  const all = [...new Set([...COMMON_TOOLS, ...tools])];
  const toggle = (t: string) => onChange(tools.includes(t) ? tools.filter((x) => x !== t) : [...tools, t]);
  return (
    <div className="router-chips">
      {all.map((t) => <ChoiceChip key={t} selected={tools.includes(t)} onClick={() => toggle(t)}><span className="mono">{t}</span></ChoiceChip>)}
      <Input className="input mono" aria-label="Other tool" placeholder="other tool…" value={extra} style={{ width: 120, height: 26, fontSize: 11.5 }}
        onChange={(e) => setExtra(e.currentTarget.value)}
        onKeyDown={(e) => { const t = extra.trim(); if (e.key === "Enter" && t) { if (!tools.includes(t)) onChange([...tools, t]); setExtra(""); } }} />
      <span className="router-hint">{tools.length ? "Only these tools count" : "Any tool counts"}</span>
    </div>
  );
}

function AgentChips({ agents, onChange }: { agents: string[]; onChange: (v: string[]) => void }) {
  const defs = useStore((s) => s.agents);
  const toggle = (id: string) => onChange(agents.includes(id) ? agents.filter((x) => x !== id) : [...agents, id]);
  const chip = (id: string, label: string, hint?: string) => <ChoiceChip key={id} hint={hint} selected={agents.includes(id)} onClick={() => toggle(id)}>{label}</ChoiceChip>;
  const known = new Set(["main", "subagent", ...defs.map((d) => d.id), ...ROUTER_SYSTEM_AGENTS.map((a) => a.id)]);
  return (
    <div className="router-agent-groups">
      <div className="router-chips">{chip("main", "Main agent")}{chip("subagent", "Any subagent")}{defs.map((d) => chip(d.id, d.name, `Subagent type ${d.id}`))}</div>
      <div className="router-chips"><span className="router-hint">System</span>{ROUTER_SYSTEM_AGENTS.map((a) => chip(a.id, a.label, `${a.id} · only routed by rules that name it`))}</div>
      {agents.filter((a) => !known.has(a)).map((a) => <RemovableChip key={a} label={a} onRemove={() => toggle(a)}><span className="mono">{a}</span></RemovableChip>)}
    </div>
  );
}

function ConditionFields({ c, set, pools }: { c: RouteCondition; set: (patch: Partial<RouteCondition>) => void; pools: string[] }) {
  switch (c.kind) {
    case "stuck":
    case "errors_in_row":
      return <>
        <Field label={c.kind === "stuck" ? "Same call fails" : "Failing calls in a row"}><NumField max={MAX_ROUTER_UINT} label="Count" value={c.count} placeholder={String(defaultCount(c.kind))} onChange={(count) => set({ count })} /><span className="router-hint">times</span></Field>
        <Field label="Tools" wide><ToolChips tools={c.tools ?? []} onChange={(tools) => set({ tools })} /></Field>
      </>;
    case "no_progress":
      return <Field label="Rounds without a successful edit"><NumField max={MAX_ROUTER_UINT} label="Count" value={c.count} placeholder={String(defaultCount(c.kind))} onChange={(count) => set({ count })} /></Field>;
    case "turns":
      return <Field label="At least this many turns"><NumField max={MAX_ROUTER_UINT} label="Count" value={c.count} placeholder="1" onChange={(count) => set({ count })} /></Field>;
    case "agent":
      return <Field label="Any of" wide><AgentChips agents={c.agents ?? []} onChange={(agents) => set({ agents })} /></Field>;
    case "context":
      return <>
        <Field label="% of base window ≥"><NumField float max={100} label="Minimum percent of window" value={c.min_pct} placeholder="any" onChange={(min_pct) => set({ min_pct })} /></Field>
        <Field label="Tokens ≥"><NumField label="Minimum tokens" value={c.min_tokens} placeholder="any" width={96} onChange={(min_tokens) => set({ min_tokens })} /></Field>
        <Field label="Tokens ≤"><NumField label="Maximum tokens" value={c.max_tokens} placeholder="any" width={96} onChange={(max_tokens) => set({ max_tokens })} /></Field>
      </>;
    case "keyword": {
      const err = c.regex && c.pattern ? regexError(c.pattern) : null;
      return <Field label="In the latest user message" wide>
        <div className="router-chips">
          <Input className={"input" + (c.regex ? " mono" : "") + (err ? " bad" : "")} aria-label="Pattern" placeholder={c.regex ? "refactor|rewrite" : "refactor"} value={c.pattern ?? ""} style={{ flex: 1, minWidth: 160, height: 28 }} onChange={(e) => set({ pattern: e.currentTarget.value })} />
          <ChoiceChip selected={!!c.regex} onClick={() => set({ regex: !c.regex })} hint="Treat the pattern as a regular expression">Regex</ChoiceChip>
          <ChoiceChip selected={!!c.case_sensitive} onClick={() => set({ case_sensitive: !c.case_sensitive })}>Match case</ChoiceChip>
        </div>
        {err && <span className="router-error" role="alert">Invalid regex · {err}</span>}
      </Field>;
    }
    case "headroom": {
      const opts: Opt[] = [{ value: "", label: "The base model's subscription" }, ...[...new Set([...pools, ...(c.provider ? [c.provider] : [])])].map((k) => ({ value: k, label: k }))];
      return <>
        <Field label="Subscription"><Dropdown value={c.provider ?? ""} options={opts} search={false} placeholder="Subscription" onChange={(provider) => set({ provider })} style={{ width: 210, flex: "none" }} /></Field>
        <Field label="Less than % left"><NumField float max={100} label="Below percent" value={c.below_pct} placeholder="15" onChange={(below_pct) => set({ below_pct })} /></Field>
      </>;
    }
    case "budget":
      return <Field label="% of monthly budget spent ≥"><NumField float label="Above percent" value={c.above_pct} placeholder="80" onChange={(above_pct) => set({ above_pct })} /></Field>;
    case "time": {
      const days = c.days ?? [];
      const bad = !isHhmm(c.from) || !isHhmm(c.to);
      return <>
        <Field label="From"><Input className={"input" + (bad && !isHhmm(c.from) ? " bad" : "")} type="time" aria-label="From time" value={c.from ?? ""} style={{ width: 110, height: 28 }} onChange={(e) => set({ from: e.currentTarget.value })} /></Field>
        <Field label="To"><Input className={"input" + (bad && !isHhmm(c.to) ? " bad" : "")} type="time" aria-label="To time" value={c.to ?? ""} style={{ width: 110, height: 28 }} onChange={(e) => set({ to: e.currentTarget.value })} /></Field>
        <Field label="Days" wide>
          <div className="router-chips">
            {WEEKDAYS.map((d, i) => <ChoiceChip key={d} selected={days.includes(i)} onClick={() => set({ days: days.includes(i) ? days.filter((x) => x !== i) : [...days, i].sort((a, b) => a - b) })}>{d}</ChoiceChip>)}
            <span className="router-hint">{days.length ? "" : "Every day · "}local time; a window past midnight wraps</span>
          </div>
        </Field>
      </>;
    }
    case "model":
      return <Field label="Base model is one of" wide><ModelChips models={c.models ?? []} onChange={(models) => set({ models })} placeholder="Add model" patterns /></Field>;
    case "images": return <span className="router-hint">The request carries images.</span>;
    case "plan": return <span className="router-hint">The chat is in plan mode.</span>;
    case "always": return <span className="router-hint">Always holds.</span>;
    default: return <span className="router-hint">This build doesn't know “{c.kind}”: it never matches, and is kept as it is.</span>;
  }
}

const KIND_OPTS: Opt[] = CONDITION_KINDS.map((k) => ({ value: k, label: CONDITION_LABELS[k] ?? k }));

function ConditionRow({ c, onChange, onRemove, pools }: { c: RouteCondition; onChange: (c: RouteCondition) => void; onRemove: () => void; pools: string[] }) {
  return (
    <div className="router-cond">
      <div className="router-cond-head">
        <Dropdown value={c.kind} options={KIND_OPTS.some((o) => o.value === c.kind) ? KIND_OPTS : [...KIND_OPTS, { value: c.kind, label: c.kind, hint: "unknown" }]} search={false} placeholder="Condition" onChange={(kind) => kind !== c.kind && onChange({ ...newCondition(kind), ...(c.not ? { not: true } : {}) })} style={{ width: 200, flex: "none" }} />
        <ChoiceChip selected={!!c.not} onClick={() => onChange({ ...c, not: !c.not })} hint="Invert this condition">Not</ChoiceChip>
        <span className="router-cond-sum">{conditionSummary(c, mname)}</span>
        <IconButton label="Remove condition" style={SMALL} onClick={onRemove}>{I.close()}</IconButton>
      </div>
      <div className="router-cond-body"><ConditionFields c={c} pools={pools} set={(patch) => onChange({ ...c, ...patch })} /></div>
    </div>
  );
}

// action

function ActionEditor({ a, ladder, onChange, hasSystemRuleAgent }: { a: RouteAction; ladder: string[]; onChange: (a: RouteAction) => void; hasSystemRuleAgent: boolean }) {
  const kind = a.kind;
  const known = ACTION_KINDS.some((k) => k.value === a.kind);
  const set = (patch: Partial<RouteAction>) => onChange({ ...a, ...patch });
  return (
    <div className="router-action">
      <Segmented label="What the rule does" value={kind} options={known ? ACTION_KINDS : [...ACTION_KINDS, { value: a.kind, label: `Unknown: ${a.kind}`, hint: "This action is preserved but unsupported" }]} onChange={(k) => set({ kind: k })} style={{ width: "100%" }} />
      {!known && <span className="router-error">This build doesn't know the action “{a.kind}”. It is preserved and does nothing until a supporting build handles it.</span>}
      {kind !== "stay" && known && <div className="router-fields">
        {kind === "use" && <Field label="Model" wide><ModelSelect value={a.model ?? ""} placeholder="Pick a model" onChange={(model) => set({ model })} /></Field>}
        {(kind === "escalate" || kind === "deescalate") && <Field label="Ladder steps">
          <NumField max={MAX_ROUTER_UINT} label="Steps" value={a.steps} placeholder="1" onChange={(steps) => set({ steps: Math.max(1, steps) })} />
          {!ladder.length && <span className="router-error">Add models to the ladder above, or this rule has nowhere to go.</span>}
        </Field>}
        <Field label={kind === "effort" ? "Effort" : "Effort override"}><EffortSelect value={a.effort} none="Unchanged" onChange={(effort) => set({ effort })} /></Field>
        <Field label="Hold for turns"><NumField max={MAX_ROUTER_UINT} label="Hold turns" value={a.turns} placeholder="0" onChange={(turns) => set({ turns })} /><span className="router-hint">{(a.turns ?? 0) > 0 ? `then revert` : "0 = this request only"}</span>{(a.turns ?? 0) > 0 && hasSystemRuleAgent && <span className="router-error">This request is a system one-shot; it cannot hold a model for later turns. Set turns to 0.</span>}</Field>
      </div>}
      {kind === "stay" && <span className="router-hint">Keep the base model for this request and skip the rules below.</span>}
    </div>
  );
}

// rules

function RuleCard({ r, i, n, ladder, open, onToggle, onChange, onMove, onDuplicate, onDelete, pools }: {
  r: RouteRule; i: number; n: number; ladder: string[]; open: boolean; pools: string[];
  onToggle: () => void; onChange: (r: RouteRule) => void; onMove: (d: number) => void; onDuplicate: () => void; onDelete: () => void;
}) {
  const [del, setDel] = useState(false);
  const [addKind, setAddKind] = useState("");
  return (
    <div className={"router-rule" + (r.enabled ? "" : " off")} data-rule={r.id}>
      <div className="router-rule-head">
        <span className="router-rule-n">{i + 1}</span>
        <Switch small label={`${r.enabled ? "Disable" : "Enable"} rule ${r.name || i + 1}`} checked={r.enabled} onChange={(enabled) => onChange({ ...r, enabled })} />
        <Pressable className="router-rule-title" aria-expanded={open} aria-label={`${open ? "Collapse" : "Expand"} rule ${r.name || i + 1}`} onClick={onToggle}>
          <span className="router-rule-name">{r.name || "Untitled rule"}</span>
          <span className="router-rule-sum">{ruleSummary(r, mname)}</span>
        </Pressable>
        <IconButton label="Move rule up" disabled={i === 0} style={SMALL} onClick={() => onMove(-1)}><ArrowUp size={12} /></IconButton>
        <IconButton label="Move rule down" disabled={i === n - 1} style={SMALL} onClick={() => onMove(1)}><ArrowDown size={12} /></IconButton>
        <IconButton label="Duplicate rule" style={SMALL} onClick={onDuplicate}><Copy size={12} /></IconButton>
        {del ? <Button variant="primary" style={{ height: 24 }} onClick={onDelete}>Delete</Button> : <IconButton label="Delete rule" style={SMALL} onClick={() => setDel(true)}>{I.trash()}</IconButton>}
        <IconButton label={open ? "Collapse rule" : "Expand rule"} style={SMALL} onClick={onToggle}>{open ? <ChevronUp size={12} /> : <ChevronDown size={12} />}</IconButton>
      </div>
      {open && <div className="router-rule-body">
        <div className="router-fields">
          <Field label="Name" wide><Input className="input" aria-label="Rule name" value={r.name} style={{ height: 28 }} onChange={(e) => onChange({ ...r, name: e.currentTarget.value })} /></Field>
        </div>
        <div className="router-sub">
          <span className="route-label">When</span>
          <div className="router-sub-main">
            {r.when.length > 1 && <Segmented label="How conditions combine" value={r.match} options={[{ value: "all", label: "All of these", hint: "Every condition must hold" }, { value: "any", label: "Any of these", hint: "One condition is enough" }]} onChange={(match) => onChange({ ...r, match })} style={{ width: 240 }} />}
            {r.when.map((c, j) => (
              <ConditionRow key={j} c={c} pools={pools} onChange={(nc) => onChange({ ...r, when: r.when.map((x, k) => (k === j ? nc : x)) })} onRemove={() => onChange({ ...r, when: r.when.filter((_, k) => k !== j) })} />
            ))}
            {!r.when.length && <span className="router-hint">No conditions: the rule always applies (to agent turns).</span>}
            <Dropdown value={addKind} options={KIND_OPTS} search={false} placeholder="Add condition…" onChange={(kind) => { setAddKind(""); onChange({ ...r, when: [...r.when, newCondition(kind)] }); }} style={{ width: 200 }} />
          </div>
        </div>
        <div className="router-sub">
          <span className="route-label">Then</span>
          <div className="router-sub-main"><ActionEditor a={r.then} ladder={ladder} hasSystemRuleAgent={r.when.some((c) => c.kind === "agent" && (c.agents ?? []).some((a) => SYSTEM_IDS.has(a)))} onChange={(then) => onChange({ ...r, then })} /></div>
        </div>
        <div className="router-fields">
          <Field label="Note" wide><Input className="input" aria-label="Rule note" placeholder="Why this rule exists (optional)" value={r.note ?? ""} style={{ height: 28 }} onChange={(e) => onChange({ ...r, note: e.currentTarget.value })} /></Field>
        </div>
      </div>}
    </div>
  );
}

function RulesEditor({ rules, ladder, onChange }: { rules: RouteRule[]; ladder: string[]; onChange: (rules: RouteRule[]) => void }) {
  const [open, setOpen] = useState<string | null>(null);
  const pools = usePoolKinds();
  return (
    <div className="sgroup router-rules">
      {rules.map((r, i) => (
        <RuleCard key={r.id} r={r} i={i} n={rules.length} ladder={ladder} pools={pools} open={open === r.id}
          onToggle={() => setOpen(open === r.id ? null : r.id)}
          onChange={(nr) => onChange(rules.map((x) => (x.id === r.id ? nr : x)))}
          onMove={(d) => onChange(moveItem(rules, i, d))}
          onDuplicate={() => {
            const copy = { ...structuredClone(r), id: newRule(rules).id, name: `${r.name || "Rule"} copy` };
            onChange([...rules.slice(0, i + 1), copy, ...rules.slice(i + 1)]);
            setOpen(copy.id);
          }}
          onDelete={() => onChange(rules.filter((x) => x.id !== r.id))} />
      ))}
      {!rules.length && <div className="routing-empty"><div>No rules</div><div className="desc">Every request uses the chat's model. Add a rule or start from a preset.</div></div>}
      <div className="srowx router-rules-foot">
        <TextButton onClick={() => { const r = newRule(rules); onChange([...rules, r]); setOpen(r.id); }}>{I.plus(12)} Add rule</TextButton>
        <span className="router-hint">Top to bottom; the first enabled rule that matches picks the model. Rules without an Agent condition only route agent turns.</span>
      </div>
    </div>
  );
}

// ladder + chain

function ModelList({ list, onChange, origin, add, label }: { list: string[]; onChange: (v: string[]) => void; origin?: string; add: string; label: string }) {
  return (
    <div className="route-chain">
      {origin && <div className="route-origin">{origin}</div>}
      {list.map((s, i) => (
        <div key={i} className="route-step">
          <span className="route-step-number">{i + 1}.</span>
          <ModelSelect value={s} onChange={(v) => onChange(list.map((x, j) => (j === i ? v : x)))} />
          <IconButton label={`Move ${label} up`} disabled={i === 0} style={SMALL} onClick={() => onChange(moveItem(list, i, -1))}><ChevronUp size={12} /></IconButton>
          <IconButton label={`Move ${label} down`} disabled={i === list.length - 1} style={SMALL} onClick={() => onChange(moveItem(list, i, 1))}><ChevronDown size={12} /></IconButton>
          <IconButton label={`Remove ${label}`} style={SMALL} onClick={() => onChange(list.filter((_, j) => j !== i))}>{I.trash()}</IconButton>
        </div>
      ))}
      <div className="route-add-fallback" style={{ marginLeft: list.length ? 24 : 0 }}><ModelSelect value="" placeholder={add} onChange={(v) => v && !list.includes(v) && onChange([...list, v])} /></div>
    </div>
  );
}

// test panel

const SYSTEM_IDS = new Set(ROUTER_SYSTEM_AGENTS.map((a) => a.id));

/** A situation to start the test panel from: the main agent of a chat on `model`, now. */
export function defaultSituation(model: string, effort: number): RouteSituation {
  const now = new Date();
  return {
    agent: "main", sub: false, turn: true, base_model: model, base_effort: effort, base_context: model ? modelInfo(model).context || 0 : 0,
    plan: false, images: false, context_tokens: 0, user_text: "", repeat_failures: 0, failing_tool: "", errors_in_row: 0, last_error_tool: "",
    no_progress_turns: 0, turns: 1, headroom: {}, budget_used_pct: null, minute_of_day: now.getHours() * 60 + now.getMinutes(), weekday: (now.getDay() + 6) % 7, hold: null,
  };
}

const hhmm = (m: number) => `${String(Math.floor(m / 60) % 24).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;

function DryRun({ route }: { route: Route }) {
  const taskId = useStore((s) => s.task);
  const task = useStore((s) => (s.task ? s.tasks[s.task] ?? null : null));
  const home = useStore((s) => s.home);
  const defs = useStore((s) => s.agents);
  const pools = usePoolKinds();
  const [open, setOpen] = useState(false);
  const [s, setS] = useState<RouteSituation>(() => defaultSituation(task?.model ?? home.model, task?.effort ?? home.effort));
  const [res, setRes] = useState<RouteDecision | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  const set = (patch: Partial<RouteSituation>) => setS((x) => ({ ...x, ...patch }));
  const agentOpts: Opt[] = [
    { value: "main", label: "Main agent", group: "Agents" },
    ...defs.map((d) => ({ value: d.id, label: d.name, hint: "subagent", group: "Agents" })),
    ...ROUTER_SYSTEM_AGENTS.map((a) => ({ value: a.id, label: a.label, group: "System requests" })),
  ];
  const pickAgent = (agent: string) => set({ agent, sub: agent !== "main" && !SYSTEM_IDS.has(agent), turn: !SYSTEM_IDS.has(agent) });
  const run = async () => {
    setBusy(true); setErr("");
    try { setRes(await api.routeDryRun(normalizeRoute(route), s)); }
    catch (e) { setRes(null); setErr(e instanceof RouterImportError ? e.message : String(e)); }
    setBusy(false);
  };
  const load = async () => {
    if (!taskId) return;
    setBusy(true); setErr("");
    try { setS(await api.routeSituation(taskId, null)); }
    catch (e) { setErr(`Couldn't read the chat's situation · ${String(e)}`); }
    setBusy(false);
  };
  const kinds = [...new Set([...pools, ...Object.keys(s.headroom)])];
  return (
    <div className="sgroup router-test">
      <div className="srowx">
        <Pressable className="router-test-toggle" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}<FlaskConical size={13} aria-hidden="true" />
          <span style={{ fontWeight: 500 }}>Test this router</span>
          <span className="router-hint">Try a situation against the rules as edited, before saving</span>
        </Pressable>
      </div>
      {open && <div className="router-test-body">
        <div className="router-fields">
          <Field label="Agent"><Dropdown value={s.agent} options={agentOpts.some((o) => o.value === s.agent) ? agentOpts : [{ value: s.agent, label: s.agent }, ...agentOpts]} placeholder="Agent" onChange={pickAgent} style={{ width: 180, flex: "none" }} /></Field>
          <Field label="Base model" wide><ModelSelect value={s.base_model} placeholder="Base model" onChange={(m) => set({ base_model: m, base_context: modelInfo(m).context || s.base_context })} /></Field>
          <Field label="Effort"><EffortSelect value={s.base_effort} onChange={(e) => set({ base_effort: e ?? 2 })} /></Field>
          <Field label="Window (tokens)"><NumField label="Base context window" value={s.base_context} width={96} onChange={(base_context) => set({ base_context })} /></Field>
          <Field label="Context tokens"><NumField label="Context tokens" value={s.context_tokens} width={96} onChange={(context_tokens) => set({ context_tokens })} /></Field>
          <Field label="Plan mode"><Switch small label="Plan mode" checked={s.plan} onChange={(plan) => set({ plan })} /></Field>
          <Field label="Images"><Switch small label="Has images" checked={s.images} onChange={(images) => set({ images })} /></Field>
          <Field label="Latest user message" wide><Input className="input" aria-label="User message" value={s.user_text} style={{ height: 28 }} onChange={(e) => set({ user_text: e.currentTarget.value })} /></Field>
          <Field label="Same call failed"><NumField max={MAX_ROUTER_UINT} label="Repeat failures" value={s.repeat_failures} onChange={(repeat_failures) => set({ repeat_failures })} /><Input className="input mono" aria-label="Failing tool" placeholder="tool" value={s.failing_tool} style={{ width: 100, height: 28 }} onChange={(e) => set({ failing_tool: e.currentTarget.value })} /></Field>
          <Field label="Errors in a row"><NumField max={MAX_ROUTER_UINT} label="Errors in a row" value={s.errors_in_row} onChange={(errors_in_row) => set({ errors_in_row })} /><Input className="input mono" aria-label="Last error tool" placeholder="tool" value={s.last_error_tool} style={{ width: 100, height: 28 }} onChange={(e) => set({ last_error_tool: e.currentTarget.value })} /></Field>
          <Field label="Rounds without progress"><NumField max={MAX_ROUTER_UINT} label="No-progress rounds" value={s.no_progress_turns} onChange={(no_progress_turns) => set({ no_progress_turns })} /></Field>
          <Field label="Turns so far"><NumField max={MAX_ROUTER_UINT} label="Turns" value={s.turns} onChange={(turns) => set({ turns })} /></Field>
          {kinds.map((k) => (
            <Field key={k} label={`${k} headroom %`}>
              <Input className="input router-num" type="number" min={0} max={100} aria-label={`${k} headroom`} placeholder="unknown" style={{ width: 80 }} value={s.headroom[k] ?? ""}
                onChange={(e) => { const v = e.currentTarget.value; const h = { ...s.headroom }; if (v === "") delete h[k]; else h[k] = Math.max(0, Math.min(100, Number(v) || 0)); set({ headroom: h }); }} />
            </Field>
          ))}
          <Field label="Budget spent %">
            <Input className="input router-num" type="number" min={0} aria-label="Budget spent" placeholder="no budget" style={{ width: 90 }} value={s.budget_used_pct ?? ""}
              onChange={(e) => { const v = e.currentTarget.value; set({ budget_used_pct: v === "" ? null : Math.max(0, Number(v) || 0) }); }} />
          </Field>
          <Field label="Local time"><Input className="input" type="time" aria-label="Local time" value={hhmm(s.minute_of_day)} style={{ width: 110, height: 28 }} onChange={(e) => { const [h, m] = e.currentTarget.value.split(":").map(Number); if (Number.isFinite(h) && Number.isFinite(m)) set({ minute_of_day: (h ?? 0) * 60 + (m ?? 0) }); }} /></Field>
          <Field label="Weekday"><Dropdown value={String(s.weekday)} search={false} options={WEEKDAYS.map((d, i) => ({ value: String(i), label: d }))} placeholder="Weekday" onChange={(v) => set({ weekday: Number(v) })} style={{ width: 90, flex: "none" }} /></Field>
          <Field label="Active hold"><Switch small label="Active hold" checked={!!s.hold} onChange={(on) => set({ hold: on ? { model: s.base_model, effort: null, turns_left: 2, rule_id: "", rule_name: "test hold", reason: "test", from_model: s.base_model, action: "use" } : null })} /></Field>
          {s.hold && <>
            <Field label="Held model" wide><ModelSelect value={s.hold.model} placeholder="Held model" onChange={(model) => set({ hold: { ...s.hold!, model } })} /></Field>
            <Field label="Turns left"><NumField max={MAX_ROUTER_UINT} label="Hold turns left" value={s.hold.turns_left} onChange={(turns_left) => set({ hold: { ...s.hold!, turns_left } })} /></Field>
          </>}
        </div>
        <div className="router-test-actions">
          <Tooltip content={taskId ? "Fill the form from the open chat's main agent" : "Open a chat to load its situation"}>
            <span><Button disabled={!taskId || busy} onClick={() => void load()}>Load from current chat</Button></span>
          </Tooltip>
          <div style={{ flex: 1 }} />
          <Button variant="primary" disabled={busy} onClick={() => void run()}>{busy && <Loader size={13} />}Run</Button>
        </div>
        {err && <div className="router-error" role="alert">{err}</div>}
        {res && <DecisionView d={res} />}
      </div>}
    </div>
  );
}

const ACTION_WORDS: Record<string, string> = { base: "No rule matched · base model", use: "Use", escalate: "Escalate", deescalate: "De-escalate", effort: "Effort", stay: "Stay", hold: "Held from an earlier turn", revert: "Hold ended · back to base" };

function DecisionView({ d }: { d: RouteDecision }) {
  return (
    <div className="router-decision" aria-label="Router decision">
      <div className="router-decision-main">
        <span className="router-decision-model">{mname(d.model) || d.model}</span>
        <span className="branchtag">{EFFORTS[d.effort] ?? `effort ${d.effort}`}</span>
        <span className="branchtag">{ACTION_WORDS[d.action] ?? d.action}</span>
      </div>
      {d.reason && <div className="router-decision-reason">{d.reason}</div>}
      {d.hold && <div className="router-hint">Holds {mname(d.hold.model)} for {d.hold.turns_left} more turn{d.hold.turns_left === 1 ? "" : "s"} after this</div>}
      {d.trace.length > 0 && <ol className="router-trace">
        {d.trace.map((t, i) => (
          <li key={t.rule_id + i} className={t.matched ? "hit" : t.skipped ? "skip" : "miss"}>
            <span className="router-trace-name">{t.rule_name || t.rule_id}</span>
            <span className="router-trace-state">{t.matched ? "matched" : t.skipped ? `skipped · ${t.skipped}` : "no match"}</span>
            {t.conditions.length > 0 && <ul>{t.conditions.map((c, j) => <li key={j} className={c.matched ? "hit" : "miss"}>{c.matched ? "✓" : "✗"} {c.detail || c.kind}</li>)}</ul>}
          </li>
        ))}
      </ol>}
    </div>
  );
}

// one router

function RouterEditor({ r, onChange, onDelete, onDuplicate, onExport }: { r: Route; onChange: (r: Route) => void; onDelete: () => void; onDuplicate: () => void; onExport: () => void }) {
  const [del, setDel] = useState(false);
  const rules = r.rules ?? NO_RULES;
  const ladder = r.ladder ?? NO_STRS;
  return (
    <>
      <div className="sgroup routecard">
        <div className="srowx">
          <Input className="input" aria-label="Router name" style={{ flex: 1, height: 28, fontWeight: 600, background: "transparent", border: 0, padding: 0 }} value={r.name} onChange={(e) => onChange({ ...r, name: e.currentTarget.value })} />
          <span className="mono" style={{ fontSize: 10.5, color: "var(--dim)" }}>route/{r.id}</span>
          <IconButton label="Duplicate router" style={SMALL} onClick={onDuplicate}><Copy size={12} /></IconButton>
          <IconButton label="Export router" style={SMALL} onClick={onExport}><Download size={12} /></IconButton>
          {del ? <Button variant="primary" style={{ height: 24 }} onClick={onDelete}>Delete</Button> : <IconButton label="Delete router" style={SMALL} onClick={() => setDel(true)}>{I.trash()}</IconButton>}
        </div>
        <div className="srowx route-row">
          <span className="route-label">Description</span>
          <Input className="input" aria-label="Router description" placeholder="What this router is for (optional)" value={r.description ?? ""} style={{ flex: 1, height: 28 }} onChange={(e) => onChange({ ...r, description: e.currentTarget.value })} />
        </div>
        <div className="srowx route-row">
          <span className="route-label">Applies to</span>
          <div className="route-targets">
            <Segmented label="Which models use this route" value={r.all ? "all" : "heads"} style={{ width: 220, flex: "none" }}
              options={[{ value: "all", label: "All models", hint: "Every model uses this router unless it has a more specific one" }, { value: "heads", label: "Specific models", hint: "Only the models listed here" }]}
              onChange={(v) => onChange({ ...r, all: v === "all" })} />
            {!r.all && <>
              {(r.heads ?? []).map((m) => <RemovableChip key={m} label={mname(m)} onRemove={() => onChange({ ...r, heads: r.heads.filter((x) => x !== m) })}>{mname(m)}</RemovableChip>)}
              <div className="route-add-model"><ModelSelect value="" placeholder="Add model" onChange={(v) => v && !r.heads?.includes(v) && onChange({ ...r, heads: [...(r.heads ?? []), v] })} /></div>
              {!r.heads?.length && <span className="desc">Add a model to use this router.</span>}
            </>}
          </div>
        </div>
        <div className="srowx route-row">
          <span className="route-label">Ladder</span>
          <div className="router-sub-main">
            <ModelList list={ladder} label="ladder model" add="Add model to the ladder…" onChange={(l) => onChange({ ...r, ladder: l })} />
            <span className="router-hint">Weakest first, strongest last. Escalate and de-escalate move along it from the current model.</span>
          </div>
        </div>
      </div>

      <div className="settings-section-label">Rules</div>
      <RulesEditor rules={rules} ladder={ladder} onChange={(next) => onChange({ ...r, rules: next })} />

      <div className="settings-section-label">Fallbacks</div>
      <div className="sgroup routecard">
        <div className="srowx route-row">
          <span className="route-label">Try in order</span>
          <ModelList list={r.steps} origin="Picked model" label="fallback" add="Add fallback model…" onChange={(steps) => onChange({ ...r, steps })} />
        </div>
        <div className="srowx route-row route-outcome">
          <span className="route-label">If none work</span>
          <Segmented label="When all models are unavailable" options={[{ value: "pause", label: "Pause", hint: "Wait for a model to become available" }, { value: "fail", label: "Fail", hint: "Mark the task as failed" }]} value={r.on_exhausted} onChange={(v) => onChange({ ...r, on_exhausted: v })} style={{ width: 170 }} />
        </div>
      </div>

      <DryRun route={r} />
    </>
  );
}

// dialogs

function ImportDialog({ taken, onImport, onClose }: { taken: string[]; onImport: (routes: Route[], note: string) => void; onClose: () => void }) {
  const [text, setText] = useState("");
  const [err, setErr] = useState("");
  const go = () => {
    try {
      const { routes, renamed } = parseRouters(text, taken);
      onImport(routes, renamed.length ? `Renamed ${renamed.map((x) => `${x.from} → ${x.to}`).join(", ")} to avoid a clash` : "");
    } catch (e) {
      setErr(e instanceof RouterImportError ? e.message : `Couldn't import · ${String(e)}`);
    }
  };
  return (
    <Modal onClose={onClose} ariaLabel="Import routers" style={{ width: "min(560px, 100%)" }}>
      <div className="mh"><Upload size={15} aria-hidden="true" /><span style={{ flex: 1 }}>Import routers</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
      <div className="mb">
        <div className="field">
          <label>Paste router JSON <span style={{ color: "var(--dim)" }}>one router, a list, or an “Export all”</span></label>
          <TextArea className={"input mono" + (err ? " bad" : "")} aria-label="Router JSON" autoFocus style={{ height: 200, padding: 10, fontSize: 11, resize: "vertical" }} value={text} onChange={(e) => { setText(e.currentTarget.value); setErr(""); }} />
        </div>
        {err && <div className="router-error" role="alert">{err}</div>}
        <div className="secondary-text">Imported routers are added to the list; nothing is saved until you press Save.</div>
      </div>
      <div className="mf"><div style={{ flex: 1 }} /><Button variant="ghost" onClick={onClose}>Cancel</Button><Button variant="primary" disabled={!text.trim()} onClick={go}>Import</Button></div>
    </Modal>
  );
}

function ExportDialog({ routes, onClose }: { routes: Route[]; onClose: () => void }) {
  const json = useMemo(() => {
    try { return exportRouters(routes); } catch (e) { return `// ${String(e)}`; }
  }, [routes]);
  const title = routes.length === 1 ? `Export “${routes[0]!.name}”` : `Export ${routes.length} routers`;
  return (
    <Modal onClose={onClose} ariaLabel={title} style={{ width: "min(560px, 100%)" }}>
      <div className="mh"><Download size={15} aria-hidden="true" /><span style={{ flex: 1 }}>{title}</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
      <div className="mb"><TextArea className="input mono" aria-label="Exported JSON" readOnly style={{ height: 260, padding: 10, fontSize: 11, resize: "vertical" }} value={json} /></div>
      <div className="mf"><span className="router-hint">Paste it into Import on another machine.</span><div style={{ flex: 1 }} /><Button variant="ghost" onClick={onClose}>Close</Button><Button variant="primary" onClick={() => copyText(json, "Router JSON copied")}><Copy size={12} />Copy</Button></div>
    </Modal>
  );
}

function PresetDialog({ current, ctx, onNew, onAppend, onClose }: { current: Route | null; ctx: PresetCtx; onNew: (p: RouterPreset) => void; onAppend: (p: RouterPreset) => void; onClose: () => void }) {
  return (
    <Modal onClose={onClose} ariaLabel="Router presets" style={{ width: "min(620px, 100%)" }}>
      <div className="mh"><WandSparkles size={15} aria-hidden="true" /><span style={{ flex: 1 }}>Presets</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
      <div className="mb">
        <div className="secondary-text">Models are pre-filled from the open chat and the ladder; check them before saving.</div>
        <div className="sgroup">
          {ROUTER_PRESETS.map((p) => {
            const out = p.build({ ...ctx, ladder: current?.ladder?.length ? current.ladder : ctx.ladder });
            return (
              <div key={p.id} className="srowx router-preset">
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 500 }}>{p.name}</div>
                  <div className="desc">{p.description}</div>
                  {out.rules.map((r) => <div key={r.id} className="router-rule-sum">{ruleSummary(r, mname)}</div>)}
                </div>
                <Button onClick={() => onNew(p)}>New router</Button>
                {current && <Button variant="primary" onClick={() => onAppend(p)}>Add to {current.name.length > 18 ? current.name.slice(0, 17) + "…" : current.name}</Button>}
              </div>
            );
          })}
        </div>
      </div>
    </Modal>
  );
}

// the page

export function RoutersTab() {
  const saved = useStore((s) => s.settings?.routes ?? NO_ROUTES);
  const [draft, setDraft] = useState<Route[] | null>(null);
  const routes = draft ?? saved;
  const [sel, setSel] = useState<string | null>(null);
  const cur = routes.find((r) => r.id === sel) ?? routes[0] ?? null;
  const [dialog, setDialog] = useState<null | "import" | "presets" | { export: Route[] }>(null);
  const [saving, setSaving] = useState(false);
  const ctx = usePresetCtx(cur?.ladder ?? NO_STRS);
  const ids = routes.map((r) => r.id);

  const edit = (next: Route[]) => setDraft(next);
  const update = (r: Route) => edit(routes.map((x) => (x.id === r.id ? r : x)));
  const addRoute = (r: Route) => { edit([...routes, r]); setSel(r.id); };
  const create = () => {
    const id = uniqueId("router", ids);
    addRoute({ id, name: routes.length ? `Router ${routes.length + 1}` : "New router", heads: [], all: routes.length === 0, steps: [], on_exhausted: "pause", description: "", ladder: [], rules: [] });
  };
  const save = async () => {
    let clean: Route[];
    try { clean = routes.map((r) => normalizeRoute(r)); }
    catch (e) { flash(String(e instanceof Error ? e.message : e)); return; }
    setSaving(true);
    const ok = await saveSettings({ routes: clean });
    setSaving(false);
    if (ok) setDraft(null);
  };

  return (
    <>
      <div className="settings-head">
        <div className="settings-head-main">
          <div className="settings-title">Routers</div>
          <div className="settings-lead">Rules pick the model for each request; fallbacks take over when it's unavailable.</div>
        </div>
        <Button onClick={() => setDialog("presets")}><WandSparkles size={12} />Presets</Button>
        <Button onClick={() => setDialog("import")}><Upload size={12} />Import</Button>
        {routes.length > 1 && <Button onClick={() => setDialog({ export: routes })}><Download size={12} />Export all</Button>}
        <Button onClick={create}>{I.plus(12)}New router</Button>
      </div>
      {draft && (
        <div className="router-savebar" role="status">
          <span>Unsaved changes</span>
          <div style={{ flex: 1 }} />
          <Button variant="ghost" onClick={() => setDraft(null)}>Discard</Button>
          <Button variant="primary" disabled={saving} onClick={() => void save()}>{saving && <Loader size={13} />}Save</Button>
        </div>
      )}
      {routes.length > 1 && (
        <div className="sgroup router-list" role="group" aria-label="Routers">
          {routes.map((r) => (
            <Pressable key={r.id} aria-current={r.id === cur?.id ? "true" : undefined} className={"srowx router-list-row" + (r.id === cur?.id ? " on" : "")} onClick={() => setSel(r.id)}>
              <span style={{ display: "flex", color: "var(--mut2)" }}>{I.route(13)}</span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontWeight: 500 }}>{r.name || r.id}</div>
                <div className="desc">{r.description ? `${r.description} · ` : ""}{routeSummary(r)}</div>
              </div>
              <span className="router-hint">{r.all ? "all models" : `${r.heads?.length ?? 0} model${r.heads?.length === 1 ? "" : "s"}`}</span>
            </Pressable>
          ))}
        </div>
      )}
      {cur && <RouterEditor key={cur.id} r={cur}
        onChange={update}
        onDuplicate={() => addRoute({ ...structuredClone(cur), id: uniqueId(cur.id, ids), name: `${cur.name} copy` })}
        onExport={() => setDialog({ export: [cur] })}
        onDelete={() => { edit(routes.filter((x) => x.id !== cur.id)); setSel(null); }} />}
      {!routes.length && <div className="sgroup routing-empty"><div>No routers</div><div className="desc">Chats use their own model and pause if it's unavailable. Create a router or start from a preset.</div></div>}
      {dialog === "import" && <ImportDialog taken={ids} onClose={() => setDialog(null)} onImport={(list, note) => {
        edit([...routes, ...list]);
        setSel(list[0]?.id ?? null);
        setDialog(null);
        flash(`Imported ${list.length} router${list.length === 1 ? "" : "s"}${note ? ` · ${note}` : ""} — Save to keep`);
      }} />}
      {dialog === "presets" && <PresetDialog current={cur} ctx={ctx} onClose={() => setDialog(null)}
        onNew={(p) => { addRoute(routerFromPreset(p, ctx, ids, routes.length === 0)); setDialog(null); }}
        onAppend={(p) => { if (cur) update(applyPreset(cur, p, ctx)); setDialog(null); }} />}
      {dialog && typeof dialog === "object" && <ExportDialog routes={dialog.export} onClose={() => setDialog(null)} />}
    </>
  );
}
