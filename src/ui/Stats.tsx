// Settings: Stats - where tokens, time, money and errors go, filterable by model.
import { useEffect, useMemo, useState } from "react";
import { X } from "lucide-react";
import { api, fmt$, fmtK, UsageView } from "../api";
import { flash, modelColor, modelInfo, saveSettings, useStore } from "../store";
import { MorphText, Loader, Tooltip, Button, Dropdown, Input, Segmented, Switch } from "./primitives";
import { openChatStats } from "./ChatStats";

export interface Counter { requests: number; input: number; output: number; cache_read: number; cache_write: number; reasoning?: number; errors: number; total_ms: number; ttft_ms: number; ttft_n: number; cost: number; last_used: string }
/** One tool's tally. */
export interface ToolCount { calls: number; errors: number }
/** One chat's record: its totals, the models that served it, the tools it called
 *  and the router events it hit.
 *
 *  Keyed by chat id, never a title — `stats.json` holds measurements, and the
 *  UI joins the name in from the chat list it already has. */
export interface ChatStat {
  total: Counter; models: Record<string, Counter>; tools: Record<string, ToolCount>; events: Record<string, number>; started: string;
  /** The chat's `total`, split by *who* spent it — `"main"`, `"sub:<id>"`,
   *  `"compactor"` or `"keepalive"`. Additive to `total`, and optional: a stats
   *  file written before per-agent attribution existed has no such key. */
  by_agent?: Record<string, Counter>;
}
export interface StatsData {
  since: string; models: Record<string, Counter>; accounts: Record<string, Counter>; daily: Record<string, Counter>; hourly: Record<string, Counter>;
  agents: Record<string, Counter>; tools: Record<string, ToolCount>; events: Record<string, number>;
  /** day -> model -> counter (recorded since per-model history was added). */
  model_daily?: Record<string, Record<string, Counter>>;
  /** hour -> model -> counter, last 14 days. */
  model_hourly?: Record<string, Record<string, Counter>>;
  /** chat id -> record, capped at the 100 most recently used. Absent on a stats
   *  file written before per-chat stats existed. */
  chats?: Record<string, ChatStat>;
}

// One restrained hue family keeps categories readable without turning the chart into a legend-driven rainbow.
// No "reasoning" hue: few models still report thinking tokens separately, so a permanent
// empty legend entry (and a permanently dashed tile) read as breakage, not as a zero.
const C = { input: "#a78bfa", cache: "#85838c", output: "#c4b5fd" };
const EMPTY: Counter = { requests: 0, input: 0, output: 0, cache_read: 0, cache_write: 0, reasoning: 0, errors: 0, total_ms: 0, ttft_ms: 0, ttft_n: 0, cost: 0, last_used: "" };

export const sum = (list: Counter[]): Counter => list.reduce((a, c) => ({
  ...a, requests: a.requests + c.requests, input: a.input + c.input, output: a.output + c.output, cache_read: a.cache_read + c.cache_read, cache_write: a.cache_write + c.cache_write,
  reasoning: (a.reasoning ?? 0) + (c.reasoning ?? 0), errors: a.errors + c.errors, total_ms: a.total_ms + c.total_ms, ttft_ms: a.ttft_ms + c.ttft_ms, ttft_n: a.ttft_n + c.ttft_n, cost: a.cost + c.cost,
}), { ...EMPTY });
const day = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
const hour = (d: Date) => `${day(d)}T${String(d.getHours()).padStart(2, "0")}`;
const tokens = (c: Counter) => c.input + c.cache_read + c.cache_write + c.output;
const hitRate = (c: Counter) => { const all = c.input + c.cache_read + c.cache_write; return all ? c.cache_read / all : 0; };
const secs = (ms: number) => (ms >= 10_000 ? Math.round(ms / 1000) + "s" : (ms / 1000).toFixed(1) + "s");
const pct = (x: number) => Math.round(x * 100) + "%";
/** `part` as a share of `whole`, as a percentage string. */
const pctOf = (part: number, whole: number) => (whole ? Math.round((part / whole) * 100) + "%" : "—");

// Shared with the per-chat panel (`ui/ChatStats.tsx`), which has to agree with
// this screen on what a token count, a cache hit rate and a duration read as —
// two spellings of the same number on two screens is how one of them rots.
export { tokens, hitRate, secs, pct, pctOf, EMPTY };

/** Pure: the last `n` days (oldest first), zero-filled. Exported for tests. */
export function lastDays(daily: Record<string, Counter>, n: number, now = new Date()): { key: string; c: Counter }[] {
  return Array.from({ length: n }, (_, i) => {
    const d = new Date(now); d.setDate(d.getDate() - (n - 1 - i));
    const key = day(d);
    return { key, c: daily[key] ?? EMPTY };
  });
}

/** Pure: one bucket's counter, all models (`total`) or only the selected ones (`perModel`). Exported for tests. */
export function pick(total: Counter | undefined, perModel: Record<string, Counter> | undefined, sel: string[]): Counter {
  if (!sel.length) return total ?? EMPTY;
  return sum(sel.map((m) => perModel?.[m] ?? EMPTY));
}

/** Pure: does a filtered range reach back before per-model history exists?
 *  Exported for tests.
 *
 *  The obvious test — compare `rangeStart` against the oldest key in
 *  `model_daily` — is wrong, because a day nobody used the app records no key at
 *  all. The oldest key is therefore the first *busy* day, which slides forward
 *  over any idle stretch; a 30d window that started before per-model tracking but
 *  contains real data from day two onwards would announce a shortfall that isn't
 *  there. So the honest boundary is `since`, the day counting began: per-model
 *  history exists from at most then onward, and anything before it is genuinely
 *  not broken down by model.
 *
 *  The reverse error is possible and deliberately accepted — per-model history
 *  was added some time after `since`, so an old range that predates both stays
 *  quiet while still undercounting. A missing note understates a number; a wrong
 *  one cries wolf on every ordinary day. */
export function historyGap(rangeStart: string, since: string, range: string | number, filtered: boolean): boolean {
  return filtered && range !== "all" && range !== "24h" && rangeStart < since;
}

/** Pure: the days of the spend ledger inside the month named by `month`
 *  (yyyy-mm), oldest first. Exported for tests.
 *
 *  This is the ledger the monthly cap pauses on, not `stats_get`'s daily counters:
 *  it records every request that costs money, survives "Reset stats", and holds
 *  days the stats file was reset past — which is exactly what the cap reads. */
export function spendMonth(spend: Record<string, number>, month: string): { key: string; v: number }[] {
  return Object.keys(spend).filter((d) => d.startsWith(month)).sort()
    // The keys came out of `spend` itself, so the value is there; without the
    // `!` the ledger rows would type as `number | undefined` and print as NaN.
    .map((d) => ({ key: d.slice(month.length + 1), v: spend[d]! }));
}

type Range = "24h" | 7 | 30 | 90 | "all";
type Bucket = { key: string; label: string; c: Counter };

function Tile({ label, value, sub, i, color }: { label: string; value: string; sub?: string; i: number; color?: string }) {
  return (
    <div className="stile" style={{ animationDelay: i * 40 + "ms" }}>
      <div className="label">{color && <i className="tdotc" style={{ background: color }} />}{label}</div>
      <MorphText as="div" className="sval">{value}</MorphText>
      {sub && <div className="ssub">{sub}</div>}
    </div>
  );
}

/** Tokens per bucket, stacked: fresh input / cache reads / output. Output is the
 *  full count, thinking tokens included — most providers no longer report them
 *  apart, and they are part of output either way. */
function TokenChart({ buckets, title }: { buckets: Bucket[]; title: string }) {
  const [hov, setHov] = useState<number | null>(null);
  const W = 640, H = 170, pad = 4;
  const max = Math.max(1, ...buckets.map((d) => tokens(d.c)));
  const bw = (W - pad * 2) / buckets.length;
  const every = Math.max(1, Math.ceil(buckets.length / 8));
  const h = hov !== null ? buckets[hov] : null;
  return (
    <div className="chart">
      <div className="chead"><span>{title}</span>
        <span className="legend"><i style={{ background: C.input }} />Input<i style={{ background: C.cache }} />Cached<i style={{ background: C.output }} />Output</span>
      </div>
      <div style={{ position: "relative" }}>
        <svg viewBox={`0 0 ${W} ${H + 18}`} width="100%" preserveAspectRatio="none" style={{ display: "block", height: H + 18 }} onMouseLeave={() => setHov(null)}>
          {[0.25, 0.5, 0.75].map((f) => <line key={f} x1={0} x2={W} y1={H - H * f} y2={H - H * f} stroke="rgba(255,255,255,0.05)" strokeDasharray="2 4" />)}
          {buckets.map((d, i) => {
            const segs = [[d.c.input + d.c.cache_write, C.input], [d.c.cache_read, C.cache], [d.c.output, C.output]] as const;
            let y = H;
            const x = pad + i * bw + bw * 0.15, w = Math.max(2, bw * 0.7);
            return (
              <g key={d.key} onMouseEnter={() => setHov(i)} style={{ opacity: hov === null || hov === i ? 1 : 0.45, transition: "opacity .2s" }}>
                <rect x={pad + i * bw} y={0} width={bw} height={H + 18} fill="transparent" />
                {tokens(d.c) === 0 && <rect x={x} y={H - 2} width={w} height={2} rx={1} fill="rgba(255,255,255,0.07)" />}
                {segs.map(([v, col], j) => {
                  if (!v) return null;
                  const hh = Math.max(1.5, (v / max) * (H - 4));
                  y -= hh;
                  return <rect key={j} x={x} y={y} width={w} height={Math.max(0.5, hh - 1.5)} rx={Math.min(3, w / 2)} fill={col} className="growbar" style={{ animationDelay: i * 12 + "ms" }} />;
                })}
                {(i === buckets.length - 1 || i % every === 0) && <text x={pad + i * bw + bw / 2} y={H + 14} textAnchor="middle" fontSize="9.5" fill="var(--dim)">{d.label}</text>}
              </g>
            );
          })}
        </svg>
        {h && (
          <div className="ctip" style={{ left: `${((hov! + 0.5) / buckets.length) * 100}%` }}>
            <b>{h.key.replace("T", " ") + (h.key.includes("T") ? ":00" : "")}</b>
            <span><i style={{ background: C.input }} />Input {fmtK(h.c.input)}{h.c.cache_write ? ` + ${fmtK(h.c.cache_write)} cache writes` : ""}</span>
            <span><i style={{ background: C.cache }} />Cached {fmtK(h.c.cache_read)}</span>
            <span><i style={{ background: C.output }} />Output {fmtK(h.c.output)}</span>
            <span className="muted">{h.c.requests} requests · {fmt$(h.c.cost)}{h.c.errors ? ` · ${h.c.errors} errors` : ""}</span>
          </div>
        )}
      </div>
    </div>
  );
}

/** Requests per hour, last 48h: one series, area + crosshair. */
function HourlyChart({ pts }: { pts: Bucket[] }) {
  const [hov, setHov] = useState<number | null>(null);
  const W = 640, H = 110, n = pts.length - 1;
  const max = Math.max(1, ...pts.map((p) => p.c.requests));
  const X = (i: number) => (i / n) * W, Y = (v: number) => H - 4 - (v / max) * (H - 12);
  const line = pts.map((p, i) => `${i ? "L" : "M"}${X(i).toFixed(1)},${Y(p.c.requests).toFixed(1)}`).join("");
  // `setHov` only ever holds a value `onMouseMove` clamped to `0..n`, and `n` is
  // the last index, so every read of `pts[hov]` below is in range.
  const h = hov !== null ? pts[hov] : null;
  return (
    <div className="chart">
      <div className="chead"><span>Requests per hour · last 48h</span><span className="legend muted">peak {max}/h</span></div>
      <div style={{ position: "relative" }}>
        <svg viewBox={`0 0 ${W} ${H}`} width="100%" preserveAspectRatio="none" style={{ display: "block", height: H }}
          onMouseMove={(e) => { const r = (e.currentTarget as SVGSVGElement).getBoundingClientRect(); setHov(Math.max(0, Math.min(n, Math.round(((e.clientX - r.left) / r.width) * n)))); }}
          onMouseLeave={() => setHov(null)}>
          <defs><linearGradient id="hg" x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stopColor={C.cache} stopOpacity="0.35" /><stop offset="100%" stopColor={C.cache} stopOpacity="0" /></linearGradient></defs>
          <path d={`${line}L${W},${H}L0,${H}Z`} fill="url(#hg)" className="fadein" />
          <path d={line} fill="none" stroke={C.cache} strokeWidth="2" vectorEffect="non-scaling-stroke" strokeLinejoin="round" className="drawline" />
          {hov !== null && <><line x1={X(hov)} x2={X(hov)} y1={0} y2={H} stroke="rgba(255,255,255,0.18)" vectorEffect="non-scaling-stroke" /><circle cx={X(hov)} cy={Y(h!.c.requests)} r="4" fill={C.cache} stroke="#1c1c1f" strokeWidth="2" vectorEffect="non-scaling-stroke" /></>}
        </svg>
        {h && <div className="ctip" style={{ left: `${(hov! / n) * 100}%` }}><b>{h.key.replace("T", " ")}:00</b><span>{h.c.requests} requests · {fmtK(tokens(h.c))} tokens</span></div>}
      </div>
    </div>
  );
}

/** Cache hit rate over the same buckets as `TokenChart`.
 *
 *  The one thing the stacked token chart cannot show: whether the run is getting
 *  cheaper per turn. A bar that is mostly cached reads fine on its own, but a
 *  *falling* cache share is the early warning that a chat has grown past what
 *  the provider will keep warm, and that only shows up as a slope.
 *
 *  Drawn as an area because it is a rate, not a quantity — the y axis is 0-100%
 *  and the fill is a level, not an amount, so a stack would imply a total that
 *  does not exist. Days with nothing sent are skipped rather than plotted as 0:
 *  an idle day had no cache behaviour to report, and dropping it to the floor
 *  would draw a cliff that never happened. */
function CacheChart({ buckets }: { buckets: Bucket[] }) {
  const pts = buckets.filter((b) => b.c.input + b.c.cache_read + b.c.cache_write > 0);
  const [hov, setHov] = useState<number | null>(null);
  const W = 640, H = 96;
  const X = (i: number) => (pts.length < 2 ? W / 2 : (i / (pts.length - 1)) * W);
  const Y = (v: number) => H - 6 - v * (H - 14);
  const line = pts.map((p, i) => `${i ? "L" : "M"}${X(i).toFixed(1)},${Y(hitRate(p.c)).toFixed(1)}`).join("");
  const h = hov !== null ? pts[hov] : null;
  const avg = pts.length ? pts.reduce((a, p) => a + hitRate(p.c), 0) / pts.length : 0;
  if (pts.length < 2) return null;
  return (
    <div className="chart">
      <div className="chead">
        <span>Cache hit rate</span>
        <span className="legend muted">avg {pct(avg)} · {pts.length} active {pts.length === 1 ? "day" : "days"}</span>
      </div>
      <div style={{ position: "relative" }}>
        <svg viewBox={`0 0 ${W} ${H}`} width="100%" preserveAspectRatio="none" style={{ display: "block", height: H }}
          onMouseMove={(e) => { const r = (e.currentTarget as SVGSVGElement).getBoundingClientRect(); setHov(Math.max(0, Math.min(pts.length - 1, Math.round(((e.clientX - r.left) / r.width) * (pts.length - 1))))); }}
          onMouseLeave={() => setHov(null)}>
          <defs><linearGradient id="cg" x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stopColor={C.cache} stopOpacity="0.4" /><stop offset="100%" stopColor={C.cache} stopOpacity="0.03" /></linearGradient></defs>
          {/* Midline at 50%: below it, more than half of everything sent was fresh
              context re-read from scratch. */}
          <line x1={0} x2={W} y1={Y(0.5)} y2={Y(0.5)} stroke="rgba(255,255,255,0.07)" strokeDasharray="2 4" vectorEffect="non-scaling-stroke" />
          <path d={`${line}L${X(pts.length - 1)},${H}L${X(0)},${H}Z`} fill="url(#cg)" className="fadein" />
          <path d={line} fill="none" stroke={C.cache} strokeWidth="2" vectorEffect="non-scaling-stroke" strokeLinejoin="round" className="drawline" />
          {hov !== null && <><line x1={X(hov)} x2={X(hov)} y1={0} y2={H} stroke="rgba(255,255,255,0.18)" vectorEffect="non-scaling-stroke" /><circle cx={X(hov)} cy={Y(hitRate(h!.c))} r="4" fill={C.cache} stroke="#1c1c1f" strokeWidth="2" vectorEffect="non-scaling-stroke" /></>}
        </svg>
        {h && <div className="ctip" style={{ left: `${(hov! / (pts.length - 1)) * 100}%` }}><b>{h.key}</b><span>{pct(hitRate(h.c))} cached · {fmtK(h.c.cache_read)} of {fmtK(h.c.input + h.c.cache_read + h.c.cache_write)} sent</span></div>}
      </div>
    </div>
  );
}

/** Where the money went, per model: a cost bar with the token counts behind it.
 *
 *  A share-of-total pie would be the obvious shape here and the wrong one — the
 *  question is not "which third is this" but "how much, and for how many tokens",
 *  and a table already answers both. What it adds over `ModelTable` is the
 *  *relative* cost of the models side by side, sorted, which the table's fixed
 *  column order does not. */
function CostByModel({ rows }: { rows: { id: string; c: Counter }[] }) {
  const withCost = rows.filter((r) => r.c.cost > 0).sort((a, b) => b.c.cost - a.c.cost);
  if (!withCost.length) return null;
  const total = withCost.reduce((a, r) => a + r.c.cost, 0);
  return (
    <div className="chart">
      <div className="chead"><span>Cost by model</span><span className="legend muted">{fmt$(total)} of {withCost.length} models</span></div>
      {withCost.map((r, i) => (
        <Tooltip key={r.id} content={`${pctOf(r.c.cost, total)} of spend · ${fmtK(tokens(r.c))} tokens · ${fmtK(Math.round(r.c.input / (r.c.requests || 1)))} fresh input per request`}>
          <div className="brow" style={{ animationDelay: i * 30 + "ms" }}>
            <div className="bl">
              <span className="bname">{modelInfo(r.id).name}</span>
              {/* Cost per million tokens is the number that says whether a model is
                  expensive or was simply used a lot. */}
              <span className="bsub">{r.c.requests} requests · {pctOf(r.c.cost, total)} of spend</span>
            </div>
            <div className="btrack"><div style={{ width: (r.c.cost / withCost[0]!.c.cost) * 100 + "%", background: modelColor(r.id), animationDelay: 120 + i * 30 + "ms" }} /></div>
            <span className="bright">{fmt$(r.c.cost)}</span>
          </div>
        </Tooltip>
      ))}
    </div>
  );
}

/** The chats behind the numbers, biggest first. Clicking one opens its own
 *  panel.
 *
 *  The title is joined in from the chat list the store already holds — the
 *  backend keys these by id alone, so no title is written to `stats.json`. A
 *  chat that is no longer in the list (deleted, or archived out of view) keeps
 *  its row and says so, rather than being hidden: its spend is still real and
 *  it is the row that explains a total nobody else accounts for. */
function ChatsPanel({ rows, onOpen }: { rows: { id: string; c: ChatStat }[]; onOpen: (id: string) => void }) {
  const tasks = useStore((st) => st.tasks);
  const max = Math.max(1, ...rows.map((r) => tokens(r.c.total)));
  return (
    <div className="chart">
      <div className="chead">
        <span>Chats</span>
        <span className="legend muted">tokens · click for the full picture</span>
      </div>
      {rows.map((r, i) => {
        const t = r.c.total;
        const title = tasks[r.id]?.title ?? "";
        const errRate = t.errors / (t.requests + t.errors || 1);
        return (
          <Tooltip key={r.id} content={`${t.requests} requests · ${pct(hitRate(t))} cached · ${t.requests ? secs(t.total_ms / t.requests) : "-"} avg${t.errors ? ` · ${t.errors} errors` : ""}${t.cost ? ` · ${fmt$(t.cost)}` : ""}`}>
            <button className="crow" style={{ animationDelay: i * 24 + "ms" }} onClick={() => onOpen(r.id)}>
              <div className="bl">
                <span className="bname">{title || "Deleted chat"}</span>
                <span className="bsub">
                  {t.requests} requests
                  {/* A chat whose row outlived its title is the one that explains
                      a total no live chat accounts for. */}
                  {!title && " · kept after deletion"}
                  {Object.keys(r.c.models).length > 1 && ` · ${Object.keys(r.c.models).length} models`}
                  {(r.c.events.retries ?? 0) > 0 && ` · ${r.c.events.retries} retries`}
                </span>
              </div>
              <div className="btrack"><div style={{ width: (tokens(t) / max) * 100 + "%", animationDelay: 120 + i * 24 + "ms" }} /></div>
              <span className="bright">{fmtK(tokens(t))}</span>
              <span className="cchg">{t.cost ? fmt$(t.cost) : ""}</span>
              <span className={"cerr" + (errRate > 0.1 ? " warn" : "")}>{errRate > 0.1 ? pct(errRate) : ""}</span>
            </button>
          </Tooltip>
        );
      })}
      {!rows.length && <div className="empty" style={{ padding: 12 }}>No chat activity recorded yet</div>}
    </div>
  );
}

/** Ranked rows with an inline magnitude bar (one hue). */
export function BarList({ title, rows, unit }: { title: string; rows: { key: string; label: string; value: number; right: string; sub?: string; warn?: boolean }[]; unit?: string }) {
  const max = Math.max(1, ...rows.map((r) => r.value));
  return (
    <div className="chart">
      <div className="chead"><span>{title}</span>{unit && <span className="legend muted">{unit}</span>}</div>
      {rows.map((r, i) => (
        <Tooltip key={r.key} content={r.sub}><div className="brow" style={{ animationDelay: i * 30 + "ms" }}>
          <div className="bl"><span className="bname">{r.label}</span>{r.sub && <span className="bsub">{r.sub}</span>}</div>
          <div className="btrack"><div style={{ width: (r.value / max) * 100 + "%", animationDelay: 120 + i * 30 + "ms" }} /></div>
          <span className={"bright" + (r.warn ? " warn" : "")}>{r.right}</span>
        </div></Tooltip>
      ))}
      {!rows.length && <div className="empty" style={{ padding: 12 }}>Nothing yet</div>}
    </div>
  );
}

/** Side-by-side numbers per model for the current range. Clicking a row toggles it in the filter. */
function ModelTable({ rows, sel, toggle }: { rows: { id: string; c: Counter }[]; sel: string[]; toggle: (id: string) => void }) {
  return (
    <div className="chart">
      <div className="chead"><span>By model</span><span className="legend muted">click a row to filter</span></div>
      <div className="stable-wrap">
        <table className="stable">
          <thead><tr><th>Model</th><th>Requests</th><th>Input</th><th>Cached</th><th>Output</th><th>Cache hit</th><th>Avg</th><th>Est. cost</th></tr></thead>
          <tbody>
            {rows.map(({ id, c }) => (
              <tr key={id} className={sel.includes(id) ? "on" : undefined} onClick={() => toggle(id)}>
                <td><i className="tdotc" style={{ background: modelColor(id) }} />{modelInfo(id).name}</td>
                <td>{fmtK(c.requests)}</td><td>{fmtK(c.input + c.cache_write)}</td><td>{fmtK(c.cache_read)}</td><td>{fmtK(c.output)}</td>
                <td>{pct(hitRate(c))}</td><td>{c.requests ? secs(c.total_ms / c.requests) : "—"}</td><td>{fmt$(c.cost)}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {!rows.length && <div className="empty" style={{ padding: 12 }}>No model activity in this range</div>}
      </div>
    </div>
  );
}

export function StatsTab() {
  const s = useStore((st) => st.settings)!;
  const month = useStore((st) => st.monthSpend);
  const [data, setData] = useState<StatsData | null>(null);
  const [range, setRange] = useState<Range>(30);
  const [sel, setSel] = useState<string[]>([]);
  const [budget, setBudget] = useState(String(s.budget));
  const [arm, setArm] = useState(false);
  const [err, setErr] = useState("");
  // The budget ledger: what the cap actually pauses on. `stats_get` cannot say
  // it — "Reset stats" clears that history but not this one.
  const [usage, setUsage] = useState<UsageView | null>(null);
  useEffect(() => {
    // Poll, but a failure clears the message the first time it happens and stays
    // quiet until a poll comes back clean, so a backend that is down for a
    // minute doesn't fire a toast every few seconds. An unchanged payload is
    // dropped: `stats_get` returns a big object and re-rendering the charts (and
    // recomputing every day × model) for identical numbers is pure waste.
    let prev = "";
    const load = () => api.stats().then((d) => {
      // `chats` is in this key because the ranked list below is drawn from it,
      // and a key without it would swallow every chat-only change: a running
      // chat's row would freeze while the charts above it kept moving.
      const key = JSON.stringify([d.models, d.daily, d.hourly, d.chats]);
      if (key !== prev) { prev = key; setData(d); setErr(""); }
    }).catch((e) => setErr(String(e)));
    load();
    const iv = setInterval(load, 15_000);
    return () => clearInterval(iv);
  }, []);
  useEffect(() => {
    let prev = "";
    const load = () => api.usage().then((u) => {
      const key = JSON.stringify([u.month, u.tokens, u.budget, u.spend]);
      if (key !== prev) { prev = key; setUsage(u); }
    }).catch(() => {});
    load();
    const iv = setInterval(load, 15_000);
    return () => clearInterval(iv);
  }, []);

  const view = useMemo(() => {
    if (!data) return null;
    const md = data.model_daily ?? {}, mh = data.model_hourly ?? {};
    // Buckets for the chart + range totals.
    let buckets: Bucket[];
    if (range === "24h") {
      buckets = Array.from({ length: 24 }, (_, i) => {
        const d = new Date(Date.now() - (23 - i) * 3600_000), key = hour(d);
        return { key, label: key.slice(11) + "h", c: pick(data.hourly[key], mh[key], sel) };
      });
    } else {
      const n = range === "all" ? Math.max(1, Math.ceil((Date.now() - new Date(data.since).getTime()) / 86400000) + 1) : range;
      buckets = lastDays(data.daily, n).map(({ key }) => ({ key, label: key.slice(5), c: pick(data.daily[key], md[key], sel) }));
    }
    const hourly = Array.from({ length: 48 }, (_, i) => {
      const key = hour(new Date(Date.now() - (47 - i) * 3600_000));
      return { key, label: key, c: pick(data.hourly[key], mh[key], sel) };
    });
    // All time per model is exact (kept since the start); ranges come from the buckets.
    const tot = range === "all" ? (sel.length ? sum(sel.map((m) => data.models[m] ?? EMPTY)) : sum(Object.values(data.models))) : sum(buckets.map((b) => b.c));
    // Every branch above yields at least one bucket (`Math.max(1, …)` on "all"),
    // so the oldest day is there; hoisted so both readers below share the read.
    const firstKey = buckets[0]!.key;
    // Per-model numbers for the table.
    const inRange = (k: string) => range === "24h" ? k >= hour(new Date(Date.now() - 23 * 3600_000)) : range === "all" || k >= firstKey;
    const byModel: Record<string, Counter[]> = {};
    if (range === "all") for (const [m, c] of Object.entries(data.models)) byModel[m] = [c];
    else for (const [k, per] of Object.entries(range === "24h" ? mh : md)) if (inRange(k)) for (const [m, c] of Object.entries(per)) (byModel[m] ??= []).push(c);
    const rows = Object.entries(byModel).map(([id, cs]) => ({ id, c: sum(cs) })).filter((r) => r.c.requests || r.c.errors).sort((a, b) => tokens(b.c) - tokens(a.c));
    // Only flag the gap when the range actually reaches back before per-model
    // history began. See `historyGap` for why the oldest key isn't the answer.
    const perSince = Object.keys(md).sort()[0] ?? data.since;
    const gap = historyGap(firstKey, data.since, range, sel.length > 0);
    return { buckets, hourly, tot, rows, perSince, gap };
  }, [data, range, sel]);

  if (!data || !view) return <>
    <div className="settings-head"><div className="settings-head-main settings-title">Stats</div></div>
    {err
      ? <div className="empty" role="alert" style={{ color: "#ff8a8a" }}>Couldn't load stats · {err}</div>
      : <Loader className="loading-state" size={16} label="Loading stats…" />}
  </>;
  const { buckets, hourly, tot, rows, perSince, gap } = view;
  const errRate = tot.requests + tot.errors ? tot.errors / (tot.requests + tot.errors) : 0;
  const known = Object.entries(data.models).sort((a, b) => tokens(b[1]) - tokens(a[1])).map(([id]) => id);
  const toggle = (id: string) => setSel((cur) => (cur.includes(id) ? cur.filter((x) => x !== id) : [...cur, id]));
  const rangeLabel = range === "24h" ? "last 24h" : range === "all" ? "all time" : `last ${range}d`;
  // `sel.length === 1` above is the guard that puts an entry in the slot.
  const who = sel.length === 0 ? "all models" : sel.length === 1 ? modelInfo(sel[0]!).name : `${sel.length} models`;
  const rowsOf = (m: Record<string, Counter>, label: (k: string) => string) =>
    Object.entries(m).sort((a, b) => tokens(b[1]) - tokens(a[1])).slice(0, 10).map(([k, c]) => ({
      key: k, label: label(k), value: tokens(c),
      right: `${fmtK(tokens(c))} · ${c.requests ? secs(c.total_ms / c.requests) : "-"}`,
      sub: `${c.requests} requests · ${pct(hitRate(c))} cached · ${c.ttft_n ? secs(c.ttft_ms / c.ttft_n) + " to first token" : "no TTFT"}${c.errors ? ` · ${c.errors} errors` : ""}${c.cost ? ` · ${fmt$(c.cost)}` : ""}`,
      warn: c.errors > 0 && c.errors / (c.requests + c.errors) > 0.1,
    }));
  const tools = Object.entries(data.tools).sort((a, b) => b[1].calls - a[1].calls).slice(0, 10).map(([k, t]) => ({ key: k, label: k.replace(/^mcp__/, "").replace("__", " · "), value: t.calls, right: `${t.calls}${t.errors ? ` · ${Math.round((t.errors / t.calls) * 100)}% err` : ""}`, warn: t.errors / t.calls > 0.25 }));
  // Chats, all time — unlike the model table this has no range filter, because a
  // chat is not a time window: it started whenever it started and its total is
  // its total. Filtering it by the selected range would need a per-day chat map
  // the backend does not keep, and showing a partial total under a chat's name
  // would be worse than showing the whole thing.
  const chatRows = Object.entries(data.chats ?? {}).map(([id, c]) => ({ id, c }))
    .filter((r) => r.c.total.requests || r.c.total.errors)
    .sort((a, b) => tokens(b.c.total) - tokens(a.c.total));
  const ev = data.events;
  // The cap's own ledger for this calendar month, biggest day first.
  const spendDays = spendMonth(usage?.spend ?? {}, day(new Date()).slice(0, 7)).sort((a, b) => b.v - a.v)
    .map((d) => ({ key: d.key, label: d.key, value: d.v, right: fmt$(d.v) }));
  const cap = usage?.budget ?? 0, spent = usage?.month ?? month;

  return (
    <>
      <div className="settings-head">
        <div className="settings-head-main"><div className="settings-title">Stats</div><div className="settings-lead">{who} · {rangeLabel} · since {new Date(data.since).toLocaleDateString()} · updates live</div></div>
        <Segmented label="Stats range" options={[{ value: "24h", label: "24h" }, { value: 7, label: "7d" }, { value: 30, label: "30d" }, { value: 90, label: "90d" }, { value: "all", label: "All" }]} value={range} onChange={setRange} style={{ width: 340 }} />
      </div>
      {known.length > 0 && (
        // A searchable picker, not a row of chips: once a few dozen models have
        // been used the chip list wraps to several lines and pushes the charts
        // off the screen.
        <div className="modelfilter">
          <Dropdown
            value=""
            placeholder="Filter by model…"
            options={[
              { value: "", label: "All models", hint: `${known.length} used` },
              ...known.map((id) => ({ value: id, label: modelInfo(id).name, hint: modelInfo(id).id, icon: <i className="tdotc" style={{ background: modelColor(id) }} /> })),
            ]}
            onChange={(v: string) => { if (v) toggle(v); }}
            style={{ width: 260 }}
          />
          {sel.map((id) => (
            <span key={id} className="mchip on">
              <i className="tdotc" style={{ background: modelColor(id) }} />
              {modelInfo(id).name}
              <button type="button" className="x" aria-label={`Clear the ${modelInfo(id).name} filter`} onClick={() => toggle(id)}><X size={10} strokeWidth={2} /></button>
            </span>
          ))}
        </div>
      )}
      {gap && <div className="snote">Per-model history starts {new Date(perSince + "T00:00").toLocaleDateString()}; earlier days only have totals for all models, so this range undercounts. "All" is exact.</div>}
      {/* Numbers below are from the last poll that worked; say so rather than let them read as live. */}
      {err && <div className="snote" role="status">Not updating · {err}</div>}
      <div className="settings-section-label">Tokens</div>
      <div className="stiles">
        <Tile i={0} label="Total" value={fmtK(tokens(tot))} sub={`${fmtK(tot.requests)} requests`} />
        <Tile i={1} label="Input" color={C.input} value={fmtK(tot.input)} sub={tot.cache_write ? `+ ${fmtK(tot.cache_write)} cache writes` : "fresh, uncached"} />
        <Tile i={2} label="Cached" color={C.cache} value={fmtK(tot.cache_read)} sub={`${pct(hitRate(tot))} cache hit`} />
        <Tile i={3} label="Output" color={C.output} value={fmtK(tot.output)} sub={tot.requests ? `${fmtK(Math.round(tot.output / tot.requests))} per request` : undefined} />
      </div>
      <div className="settings-section-label">Usage</div>
      <div className="stiles">
        <Tile i={0} label="Requests" value={fmtK(tot.requests)} sub={rangeLabel} />
        <Tile i={1} label="Avg response" value={tot.requests ? secs(tot.total_ms / tot.requests) : "-"} sub={tot.ttft_n ? `${secs(tot.ttft_ms / tot.ttft_n)} to first token` : undefined} />
        <Tile i={2} label="Errors" value={(errRate * 100).toFixed(errRate < 0.1 ? 1 : 0) + "%"} sub={`${tot.errors} failed attempts`} />
        <Tile i={3} label="Est. cost" value={fmt$(tot.cost)} sub="at API list prices" />
      </div>
      <TokenChart buckets={buckets} title={range === "24h" ? "Tokens per hour" : "Tokens per day"} />
      {/* Both of these follow the range filter, like the charts above: they are
          read as "over the period you are looking at". The hourly chart below is
          the deliberate exception — it is always the last 48h, because an hour
          axis is unreadable stretched over a month. */}
      <CacheChart buckets={buckets} />
      <HourlyChart pts={hourly} />
      <ModelTable rows={rows} sel={sel} toggle={toggle} />
      <CostByModel rows={rows} />
      {chatRows.length > 0 && <ChatsPanel rows={chatRows} onOpen={openChatStats} />}
      <div className="settings-section-label">Everything else · all models</div>
      <div className="cgrid">
        <BarList title="Accounts" unit="tokens · avg time" rows={rowsOf(data.accounts, (k) => k)} />
        <BarList title="Agents" unit="tokens · avg time" rows={rowsOf(data.agents, (k) => (k === "main" ? "Main agent" : k))} />
        <BarList title="Tools" unit="calls" rows={tools} />
      </div>
      <div className="stiles small">
        <Tile i={0} label="Retries" value={String(ev.retries ?? 0)} sub="attempts the router repeated" />
        <Tile i={1} label="Fallbacks" value={String(ev.fallbacks ?? 0)} sub="switched account or model" />
        <Tile i={2} label="Auto-pauses" value={String(ev.pauses ?? 0)} sub="everything was out" />
        <Tile i={3} label="Compactions" value={String(ev.compactions ?? 0)} sub="context summaries" />
      </div>
      <div className="settings-section-label">Budget</div>
      <div className="sgroup"><div className="srowx">
          <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Monthly cap · {fmt$(month)} spent</div><div className="desc">Pauses before the cap. 0 disables it.</div></div>
          <span style={{ color: "var(--mut3)" }}>$</span><Input style={{ width: 80 }} value={budget} onChange={(e) => setBudget(e.currentTarget.value)} onBlur={() => saveSettings({ budget: Math.max(0, parseFloat(budget) || 0) })} />
      </div>
      {/* Lives with the cap because it is the same kind of decision: a per-task
          spend knob. Each ping is a real request, so this one is opt-in rather
          than on by default. */}
      <div className="srowx">
          <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Keep the cache warm while waiting</div><div className="desc">Pings the provider while a chat waits on you, so its cached prefix stays warm and the next turn is mostly a cache read. Every ping is a real, billable request.</div></div>
          <Switch label="Keep the cache warm while waiting" hint="Warm the prompt cache while a chat waits on you · each ping costs a request" checked={s.cache_keepalive ?? false} onChange={(checked) => void saveSettings({ cache_keepalive: checked })} />
      </div>
      <div className="srowx">
          <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Pings per task</div><div className="desc">How many keepalive pings one task may send before it stops.</div></div>
          <Dropdown search={false} style={{ width: 110 }} value={String(s.cache_keepalive_pings ?? 4)} options={[2, 4, 8, 12].map((n) => ({ value: String(n), label: `${n} pings` }))} onChange={(v) => void saveSettings({ cache_keepalive_pings: Number(v) })} />
      </div>
      </div>
      {/* What the cap counts: the spend ledger, not the chart's day totals. It
          records every billed request and survives a stats reset, so this is the
          only place a day the reset cleared still shows up. */}
      {usage && spendDays.length > 0 && <BarList title="Spend this month" unit="what the cap counts" rows={spendDays} />}
      <div className="stiles small">
        <Tile i={0} label="Cap" value={cap > 0 ? fmt$(cap) : "Off"} sub={cap > 0 ? `${pct(spent / cap)} of it spent` : "set a cap to pause before the money runs out"} />
        <Tile i={1} label="Spent" value={fmt$(usage?.month ?? month)} sub={cap > 0 ? `${fmt$(Math.max(0, spent - cap))} left` : "no cap set"} />
        <Tile i={2} label="Billable tokens" value={fmtK(usage?.tokens ?? 0)} sub="input + output, all requests" />
        <Tile i={3} label="Days billed" value={String(spendDays.length)} sub={`in ${new Date().toLocaleString(undefined, { month: "long", year: "numeric" })}`} />
      </div>
      <div style={{ display: "flex", justifyContent: "flex-end" }}>
        {arm ? <Button variant="primary" onClick={async () => {
          try {
            await api.statsReset();
            setArm(false);
            setData(await api.stats());
            setErr("");
          } catch (e) { setErr(String(e)); flash(String(e)); }
        }}>Clear all stats</Button>
          : <Button variant="ghost" style={{ color: "var(--mut3)" }} onClick={() => setArm(true)}>Reset stats…</Button>}
      </div>
    </>
  );
}
