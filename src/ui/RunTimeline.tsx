// One chat's run timeline, opened from "Run timeline…" in a chat's context menu
// or the clock-chart button in the chat header: a Gantt view of where the run
// spent its wall-clock time and its money, one lane per agent.
//
// Its own fetch (`task_run_timeline`) for the same reason as the stats panel:
// the numbers are derived on demand from the chat's recorded timings, and the
// panel must work for a chat that is not the open one. The fetched timeline is
// the only server data held here; zoom, pan and hover are transient view state.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Item, RunBlock, RunLane, RunTimeline, RunTotals } from "../api";
import { api, fmt$, fmtK } from "../api";
import { get, go, loadPreviousHistory, modelColor, modelInfo, set, useStore } from "../store";
import { ST } from "./icons";
import { Button, IconButton, Loader, Modal, Tooltip } from "./primitives";
import { ZoomIn, ZoomOut, Maximize2 } from "lucide-react";
import "./ChatStats.css";
import "./RunTimeline.css";

/** The visible slice of the run, as absolute epoch-ms bounds. */
export interface View { from: number; to: number }

/** Pure: a duration the way a reader says it — `450ms`, `4.2s`, `42s`,
 *  `4m 05s`, `1h 05m`, `2d 3h`. Exported for tests. */
export function fmtDur(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "0s";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 10_000) return `${(Math.round(ms / 100) / 10).toString()}s`;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  if (s < 3600) { const r = s % 60; return r ? `${Math.floor(s / 60)}m ${String(r).padStart(2, "0")}s` : `${s / 60}m`; }
  const m = Math.round(s / 60);
  if (m < 1440) { const r = m % 60; return r ? `${Math.floor(m / 60)}h ${String(r).padStart(2, "0")}m` : `${m / 60}h`; }
  const h = Math.round(m / 60);
  return h % 24 ? `${Math.floor(h / 24)}d ${h % 24}h` : `${h / 24}d`;
}

/** An axis label: the same units as `fmtDur`, without the padding a tooltip
 *  wants — "1m30s" crowds less than "1m 30s" under a tick. */
function tickLabel(ms: number): string {
  if (ms <= 0) return "0s";
  return fmtDur(ms).replace(/ 0?/, "");
}

/** Tick steps an axis may use: every one reads as a round number. */
const STEPS = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1e3, 2e3, 5e3, 10e3, 15e3, 30e3, 60e3, 120e3, 300e3, 600e3, 900e3, 1800e3, 3600e3, 7200e3, 10800e3, 21600e3, 43200e3, 86400e3];

/** Pure: about `n` evenly spaced ticks between `from` and `to` (absolute ms),
 *  at round multiples of the step counted from `origin` (the run's start), so
 *  the labels read 0s, 30s, 1m… however the view is panned. Exported for tests. */
export function axisTicks(from: number, to: number, n: number, origin = from): { at: number; label: string }[] {
  const span = to - from;
  if (!(span > 0) || n < 1) return [];
  const want = span / n;
  const step = STEPS.find((s) => s >= want) ?? Math.ceil(want / 86400e3) * 86400e3;
  const first = origin + Math.ceil((from - origin) / step) * step;
  const out: { at: number; label: string }[] = [];
  for (let at = first; at <= to + 1e-6 && out.length < 200; at += step) out.push({ at, label: tickLabel(at - origin) });
  return out;
}

/** Pure: the run's time bounds. A run that has not recorded anything yet gets a
 *  one-second window so the axis still has a scale. Exported for tests. */
export function timelineBounds(tl: RunTimeline): View {
  const starts = tl.blocks.map((b) => b.start_ms);
  const ends = tl.blocks.map((b) => b.end_ms);
  const from = tl.start_ms ?? (starts.length ? Math.min(...starts) : tl.now_ms);
  const to = Math.max(tl.end_ms ?? (ends.length ? Math.max(...ends) : tl.now_ms), from);
  return { from, to: to > from ? to : from + 1000 };
}

/** The narrowest window zoom may reach: a few milliseconds is already below
 *  anything the backend records with meaning. */
export const MIN_SPAN = 20;

/** Pure: zoom `view` by `factor` (<1 zooms in) keeping the instant at
 *  `anchor` (0..1 across the view) under the pointer, clamped to `bounds`.
 *  Exported for tests. */
export function zoomView(view: View, factor: number, anchor: number, bounds: View): View {
  const full = bounds.to - bounds.from;
  const span = view.to - view.from;
  const next = Math.min(full, Math.max(Math.min(MIN_SPAN, full), span * factor));
  const a = Math.min(1, Math.max(0, anchor));
  const at = view.from + a * span;
  return clampView({ from: at - a * next, to: at - a * next + next }, bounds);
}

/** Pure: move `view` by `delta` ms, without leaving `bounds`. Exported for tests. */
export function panView(view: View, delta: number, bounds: View): View {
  return clampView({ from: view.from + delta, to: view.to + delta }, bounds);
}

function clampView(v: View, b: View): View {
  const span = Math.min(v.to - v.from, b.to - b.from);
  const from = Math.min(Math.max(v.from, b.from), b.to - span);
  return { from, to: from + span };
}

/** Layout constants for the lane view, in CSS pixels. */
export const AXIS_H = 22, ROW_H = 16, ROW_GAP = 3, LANE_PAD = 5;

export interface LaneBox { lane: RunLane; top: number; height: number }
export interface BlockRect { block: RunBlock; index: number; x: number; y: number; w: number; h: number }

/** Pure: where every lane and block lands, for a time area `width` pixels wide
 *  showing `view`. Blocks entirely outside the view are dropped; the rest are
 *  clipped to it and never thinner than one pixel, so a 3ms tool call in a
 *  two-hour run is still something you can hover. `y` is measured from the top
 *  of the SVG, below the axis. Exported for tests. */
export function layoutTimeline(tl: RunTimeline, width: number, view: View): { lanes: LaneBox[]; rects: BlockRect[]; height: number } {
  const lanes: LaneBox[] = [];
  const at = new Map<string, LaneBox>();
  let top = AXIS_H;
  for (const lane of tl.lanes) {
    const rows = Math.max(1, lane.rows || 1);
    const box = { lane, top, height: LANE_PAD * 2 + rows * ROW_H + (rows - 1) * ROW_GAP };
    lanes.push(box); at.set(lane.id, box);
    top += box.height;
  }
  const span = Math.max(1, view.to - view.from);
  const px = (ms: number) => ((ms - view.from) / span) * width;
  const rects: BlockRect[] = [];
  tl.blocks.forEach((block, index) => {
    const box = at.get(block.lane);
    if (!box) return;
    const xs = px(block.start_ms), xe = px(Math.max(block.end_ms, block.start_ms));
    if (xe < 0 || xs > width) return;
    const x0 = Math.max(0, xs), x1 = Math.min(width, xe);
    const x = Math.min(x0, width - 1);
    rects.push({ block, index, x, y: box.top + LANE_PAD + block.row * (ROW_H + ROW_GAP), w: Math.max(1, x1 - x), h: ROW_H });
  });
  return { lanes, rects, height: top };
}

/** One segment of the exclusive wall-clock split. */
export interface Segment { key: string; label: string; ms: number; color: string }

/** Pure: the exclusive split as bar segments, in the backend's precedence order,
 *  zero parts dropped. They add up to `wall_ms`. Exported for tests. */
export function splitSegments(t: RunTotals): Segment[] {
  const all: Segment[] = [
    { key: "you", label: "Waiting on you", ms: t.you_ms, color: "#fbbf24" },
    { key: "paused", label: "Paused / parked", ms: t.paused_ms, color: "#6b6a73" },
    { key: "model", label: "Model", ms: t.model_ms, color: "#a78bfa" },
    { key: "retry", label: "Retry backoff", ms: t.retry_ms, color: "#fb923c" },
    { key: "tool", label: "Tools", ms: t.tool_ms, color: "#9ca3af" },
    { key: "idle", label: "Idle", ms: t.idle_ms, color: "#3f3e45" },
  ];
  return all.filter((s) => s.ms > 0);
}

const WAIT_LABEL: Record<string, string> = {
  approval: "Waiting for your approval", question: "Waiting for your answer", pause: "Paused",
  park: "Parked (waiting for an event)", retry: "Retry backoff",
};

/** A usage role id as a reader should see it. */
function roleLabel(role: string): string {
  if (!role || role === "main") return "Main agent";
  if (role.startsWith("sub:")) return `Subagent ${role.slice(4)}`;
  const known: Record<string, string> = { compactor: "Compactor", title: "Title", keepalive: "Cache keepalive", side: "Side question", summary: "Summary", "sub-ask": "Subagent question" };
  return known[role] ?? role;
}

/** Pure: the hover card for one block, as label/value lines. Exported for tests. */
export function blockTip(b: RunBlock, origin: number): { title: string; lines: [string, string][] } {
  const lines: [string, string][] = [];
  const took = Math.max(0, b.end_ms - b.start_ms);
  lines.push(["Duration", fmtDur(took) + (b.open ? " · still running" : "")]);
  lines.push(["Starts at", "+" + fmtDur(b.start_ms - origin)]);
  let title: string;
  if (b.kind === "model") {
    title = `Model · ${modelInfo(b.name).name}`;
    if (b.account) lines.push(["Account", b.account]);
    if (b.role) lines.push(["Role", roleLabel(b.role)]);
    if (b.ttft_ms != null) lines.push(["First token", fmtDur(b.ttft_ms)]);
    if (b.input || b.output || b.cache_read || b.cache_write) lines.push(["Tokens", `${fmtK(b.input ?? 0)} in · ${fmtK(b.output ?? 0)} out · ${fmtK(b.cache_read ?? 0)} cached · ${fmtK(b.cache_write ?? 0)} written`]);
    if (b.cost != null) lines.push(["Cost", fmt$(b.cost)]);
    if ((b.attempt ?? 1) > 1 || b.fallback) lines.push(["Attempt", `#${b.attempt ?? 1}${b.fallback ? " · fallback" : ""}`]);
    if (b.status && b.status !== "ok") lines.push(["Status", b.status]);
    if (b.detail) lines.push(["Error", b.detail]);
  } else if (b.kind === "tool") {
    title = `Tool · ${b.name}`;
    lines.push(["Status", b.status || (b.open ? "running" : "ok")]);
  } else if (b.kind === "wait") {
    title = WAIT_LABEL[b.name] ?? `Wait · ${b.name}`;
    // A pause's kind (manual, exhausted, global) or a park's trigger.
    if (b.detail) lines.push(["Why", b.detail]);
  } else {
    title = `Background · ${b.name}`;
    if (b.status) lines.push(["Exit", b.status]);
  }
  if (b.estimated) lines.push(["Timing", "estimated from transcript timestamps"]);
  return { title, lines };
}

/** The CSS class a block is drawn with: what it is, then how it went. */
function blockClass(b: RunBlock): string {
  const c = ["rt-b", b.kind];
  if (b.kind === "wait") c.push(b.name === "approval" || b.name === "question" ? "you" : b.name === "retry" ? "retry" : "hold");
  if (b.kind === "tool" && (b.status === "error" || b.status === "denied")) c.push("err");
  if (b.kind === "tool" && b.status === "cancelled") c.push("cancel");
  if (b.kind === "model" && b.status && b.status !== "ok" && !b.open) c.push("fail");
  if (b.estimated) c.push("est");
  if (b.open) c.push("open");
  return c.join(" ");
}

// ---------------------------------------------------------------------------
// Jumping to the transcript

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
const esc = (id: string) => (typeof CSS !== "undefined" && CSS.escape ? CSS.escape(id) : id.replace(/["\\]/g, "\\$&"));

/** Scroll the row for `id` into view inside `root`. When that row is not
 *  mounted — folded inside a tool group or a finished turn's "Worked for" — the
 *  nearest preceding row of `list` that is mounted stands in for it, which is
 *  where the folded row lives. Returns which one was revealed, or null. */
export function revealItem(id: string, list: Item[] | undefined, root: ParentNode = document): "exact" | "near" | null {
  const find = (x: string) => root.querySelector<HTMLElement>(`[data-item="${esc(x)}"]`);
  try {
    let el = find(id);
    let how: "exact" | "near" = "exact";
    if (!el && list) {
      const i = list.findIndex((it) => it.id === id);
      for (let j = i - 1; i >= 0 && j >= 0 && !el; j--) { el = find(list[j]!.id); how = "near"; }
    }
    if (!el) return null;
    el.scrollIntoView?.({ block: "center", behavior: "smooth" });
    el.classList.add("rt-flash");
    setTimeout(() => el.classList.remove("rt-flash"), 1600);
    return how;
  } catch {
    return null;
  }
}

/** Make `taskId` the open chat, unless it already is. */
function ensureOpen(taskId: string) {
  const s = get();
  if (s.view !== "session" || s.task !== taskId) go("session", { task: taskId });
}

/** Close the panel and show the transcript row behind a lane/block.
 *
 *  `lane` decides which transcript: "main" and "bg" rows are in the chat's own
 *  transcript; any other lane is a subagent, whose rows are in its panel in the
 *  Details rail. The row may not be loaded yet (older history, or the chat was
 *  only just opened), so this retries a few times, paging older history in on
 *  the main transcript when the row is not in it. Exported for tests; resolves
 *  to whether a row was revealed. */
export async function jumpToRow(taskId: string, lane: string, item: string, opts: { tries?: number; delay?: number } = {}): Promise<boolean> {
  const tries = opts.tries ?? 12, delay = opts.delay ?? 120;
  set({ runTimeline: null });
  ensureOpen(taskId);
  let sub = lane !== "main" && lane !== "bg" ? lane : null;
  // A background command launched by a subagent names a row in that agent's
  // transcript; find which one already has it loaded, if any.
  if (lane === "bg" && item && !get().items[taskId]?.some((it) => it.id === item)) {
    const subs = get().subItems[taskId] ?? {};
    sub = Object.keys(subs).find((k) => subs[k]?.some((it) => it.id === item)) ?? null;
  }
  if (sub) set({ subOpen: sub, details: true });
  if (!item) return false;
  let pages = 0;
  for (let n = 0; n < tries; n++) {
    // A tick first: the chat, or the subagent's panel, renders after the set.
    await sleep(n === 0 ? 0 : delay);
    const s = get();
    const list = sub ? s.subItems[taskId]?.[sub] : s.items[taskId];
    const root = (sub ? document.querySelector(".subpanel") : document.querySelector(".timeline")) ?? document;
    if (revealItem(item, list, root)) return true;
    if (!sub && list && !list.some((it) => it.id === item) && pages < 4) {
      const tl = document.querySelector<HTMLElement>(".timeline");
      if (tl) { pages++; await loadPreviousHistory(taskId, tl); }
    }
  }
  return false;
}

/** Close the panel and open the chat's Details rail, where the context-window
 *  ring already lives — this panel does not draw a second one.
 *
 *  The rail is not drawn while the Artificts workspace is open (`Session` stands
 *  it down for the record surface), so opening it from there would look like the
 *  button did nothing. Hand the pane back to the chat first, as its "Chat" button
 *  does. */
export function openContextWindow(taskId: string) {
  set({ runTimeline: null });
  ensureOpen(taskId);
  const task = get().tasks[taskId];
  if (task?.artifict_open) {
    set((s) => (s.tasks[taskId] ? { tasks: { ...s.tasks, [taskId]: { ...s.tasks[taskId]!, artifict_open: false } } } : {}));
    void api.update(taskId, { artifict_open: false }).catch(() => {});
  }
  set({ details: true, subOpen: null });
}

// ---------------------------------------------------------------------------
// Panel

function Fact({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="sfact">
      <div className="label">{label}</div>
      <div className="sval">{value}</div>
      {sub && <div className="ssub">{sub}</div>}
    </div>
  );
}

function SplitBar({ t }: { t: RunTotals }) {
  const seg = splitSegments(t);
  const total = seg.reduce((a, s) => a + s.ms, 0);
  if (!total) return null;
  return (
    <div className="chart">
      <div className="chead"><span>Where the time went</span><span className="legend">{fmtDur(t.wall_ms)} wall clock</span></div>
      <div className="rt-split" aria-label="Wall-clock split">
        {seg.map((s) => (
          <Tooltip key={s.key} content={`${s.label} · ${fmtDur(s.ms)} · ${Math.round((s.ms / total) * 100)}%`}>
            <div style={{ background: s.color, width: (s.ms / total) * 100 + "%" }} />
          </Tooltip>
        ))}
      </div>
      <div className="legend" style={{ marginTop: 8, justifyContent: "flex-start", gap: 14, flexWrap: "wrap" }}>
        {seg.map((s) => <span key={s.key}><i style={{ background: s.color }} />{s.label} {fmtDur(s.ms)}</span>)}
        {t.bg_ms > 0 && <span className="muted">Background commands ran {fmtDur(t.bg_ms)} alongside</span>}
      </div>
    </div>
  );
}

function laneName(l: RunLane): string {
  if (l.id === "main") return l.label || "Main agent";
  if (l.id === "bg") return l.label || "Background commands";
  return l.role ? `${l.role} · ${l.label}` : l.label || l.id;
}

function laneTitle(tl: RunTimeline, id: string): string {
  const l = tl.lanes.find((x) => x.id === id);
  return l ? laneName(l) : id;
}

function Lanes({ tl, taskId }: { tl: RunTimeline; taskId: string }) {
  const bounds = useMemo(() => timelineBounds(tl), [tl]);
  // null = fitted, which follows a still-running chat's growing end.
  const [zoomed, setZoomed] = useState<View | null>(null);
  const view = zoomed ? clampView(zoomed, bounds) : bounds;
  const [width, setWidth] = useState(800);
  const [hover, setHover] = useState<{ i: number; x: number; y: number } | null>(null);
  const area = useRef<HTMLDivElement>(null);
  const drag = useRef<{ x: number; view: View; moved: boolean } | null>(null);
  const suppressClick = useRef(false);

  const apply = useCallback((v: View) => {
    setZoomed(v.to - v.from >= bounds.to - bounds.from - 0.5 ? null : v);
  }, [bounds]);

  useEffect(() => {
    const el = area.current;
    if (!el) return;
    const measure = () => { if (el.clientWidth > 0) setWidth(el.clientWidth); };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Wheel needs a non-passive listener to keep the modal from scrolling while
  // you zoom; React's onWheel is passive.
  const viewRef = useRef(view);
  viewRef.current = view;
  useEffect(() => {
    const el = area.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      const v = viewRef.current;
      const r = el.getBoundingClientRect();
      const w = r.width || width;
      e.preventDefault();
      if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
        const d = (e.deltaX || e.deltaY) / w * (v.to - v.from);
        apply(panView(v, d, bounds));
      } else {
        apply(zoomView(v, Math.exp(e.deltaY * 0.0015), (e.clientX - r.left) / w, bounds));
      }
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [apply, bounds, width]);

  const lay = useMemo(() => layoutTimeline(tl, width, view), [tl, width, view]);
  const ticks = useMemo(() => axisTicks(view.from, view.to, Math.max(2, Math.floor(width / 90)), bounds.from), [view, width, bounds.from]);
  const px = (ms: number) => ((ms - view.from) / Math.max(1, view.to - view.from)) * width;

  const onDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    drag.current = { x: e.clientX, view, moved: false };
    suppressClick.current = false;
  };
  const onMove = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d) return;
    const dx = e.clientX - d.x;
    if (Math.abs(dx) > 3) { d.moved = true; suppressClick.current = true; setHover(null); }
    if (d.moved) apply(panView(d.view, -dx / width * (d.view.to - d.view.from), bounds));
  };
  const onUp = () => { drag.current = null; };

  const hoverAt = (i: number) => (e: React.MouseEvent) => {
    if (drag.current?.moved) return;
    const r = area.current?.getBoundingClientRect();
    setHover({ i, x: e.clientX - (r?.left ?? 0), y: e.clientY - (r?.top ?? 0) });
  };
  const clickBlock = (b: RunBlock) => {
    if (suppressClick.current) { suppressClick.current = false; return; }
    void jumpToRow(taskId, b.lane, b.item);
  };
  const clickLane = (l: RunLane) => {
    if (l.id === "main" || l.id === "bg") return;
    void jumpToRow(taskId, l.id, "");
  };

  const hb = hover ? tl.blocks[hover.i] : undefined;
  const tip = hb ? blockTip(hb, bounds.from) : null;
  const zoomBy = (f: number) => apply(zoomView(view, f, 0.5, bounds));

  return (
    <div className="chart rt-chart">
      <div className="chead">
        <span>Lanes</span>
        <span className="legend muted rt-hint">Scroll to zoom · Shift+scroll or drag to pan · click a block to jump to it</span>
        <span className="rt-tools">
          <IconButton label="Zoom in" onClick={() => zoomBy(0.5)}><ZoomIn size={13} /></IconButton>
          <IconButton label="Zoom out" disabled={!zoomed} onClick={() => zoomBy(2)}><ZoomOut size={13} /></IconButton>
          <IconButton label="Fit" disabled={!zoomed} onClick={() => setZoomed(null)}><Maximize2 size={13} /></IconButton>
        </span>
      </div>
      <div className="rt-lanes">
        <div className="rt-labels" style={{ paddingTop: AXIS_H }}>
          {lay.lanes.map(({ lane, height }) => (
            <div key={lane.id} className={"rt-label" + (lane.id !== "main" && lane.id !== "bg" ? " sub" : "")} data-lane={lane.id}
              style={{ height, paddingLeft: 6 + Math.max(0, lane.depth) * 12 }} title={laneName(lane)} onClick={() => clickLane(lane)}>
              <i className="rt-dot" style={{ background: ST[lane.status]?.dot ?? "var(--st-idle)" }} />
              <span className="rt-lname">{laneName(lane)}</span>
            </div>
          ))}
        </div>
        <div ref={area} className="rt-area"
          onPointerDown={onDown} onPointerMove={onMove} onPointerUp={onUp} onPointerLeave={() => { onUp(); setHover(null); }}>
          <svg width={width} height={lay.height} role="img" aria-label="Run timeline lanes">
            <defs>
              <pattern id="rt-hatch" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
                <rect width="6" height="6" fill="#4a4950" />
                <line x1="0" y1="0" x2="0" y2="6" stroke="#77767e" strokeWidth="2.5" />
              </pattern>
            </defs>
            {lay.lanes.map(({ lane, top, height }, i) => (
              <rect key={lane.id} className={"rt-lanebg" + (i % 2 ? " odd" : "")} x={0} y={top} width={width} height={height} />
            ))}
            {ticks.map((t) => (
              <g key={t.at} className="rt-tick">
                <line x1={px(t.at)} x2={px(t.at)} y1={AXIS_H - 4} y2={lay.height} />
                <text x={px(t.at) + 3} y={AXIS_H - 8}>{t.label}</text>
              </g>
            ))}
            {lay.rects.map((r) => (
              <rect key={r.index} data-block={r.index} className={blockClass(r.block)} x={r.x} y={r.y} width={r.w} height={r.h} rx={Math.min(3, r.w / 2)}
                style={r.block.kind === "model" ? { fill: modelColor(r.block.name) } : undefined}
                onMouseEnter={hoverAt(r.index)} onMouseMove={hoverAt(r.index)} onMouseLeave={() => setHover(null)}
                onClick={() => clickBlock(r.block)} />
            ))}
          </svg>
          {tip && hover && (
            <div className="rt-tip" role="tooltip" style={{ left: Math.min(hover.x + 14, Math.max(0, width - 260)), top: hover.y + 16 }}>
              <div className="rt-tip-title">{tip.title}</div>
              {tip.lines.map(([k, v]) => <div key={k} className="rt-tip-line"><span>{k}</span><span>{v}</span></div>)}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export function RunTimelinePanel() {
  const id = useStore((s) => s.runTimeline);
  const task = useStore((s) => (id ? s.tasks[id] ?? null : null));
  const close = () => set({ runTimeline: null });
  const [tl, setTl] = useState<RunTimeline | null>(null);
  const [err, setErr] = useState("");
  const live = task?.status === "running" || task?.status === "waiting";

  useEffect(() => { setTl(null); setErr(""); }, [id]);
  // Poll only while the chat can still be adding to its timeline.
  useEffect(() => {
    if (!id) return;
    let alive = true, prev = "";
    const load = () => {
      api.runTimeline(id).then((r) => {
        if (!alive) return;
        const k = JSON.stringify(r);
        if (k !== prev) { prev = k; setTl(r); }
        setErr("");
      }).catch((e) => { if (alive) setErr(String(e)); });
    };
    load();
    const iv = live ? setInterval(load, 3000) : undefined;
    return () => { alive = false; if (iv) clearInterval(iv); };
  }, [id, live]);

  const t = tl?.totals;
  const empty = !!tl && !tl.recorded && tl.blocks.length === 0;
  const costNote = t && Math.abs(t.task_cost - t.cost) >= 0.005 && t.task_cost > t.cost ? `recorded ${fmt$(t.cost)} of ${fmt$(t.task_cost)}` : "at API list prices";

  if (!id) return null;
  return (
    <Modal className="modal-chat-stats modal-run-timeline" onClose={close} ariaLabel="Run timeline">
      <div className="chat-stats-header">
        <div className="chat-stats-heading">
          <div className="chat-stats-title">{task?.title || "This chat"}</div>
          <div className="chat-stats-subtitle">
            Run timeline
            {tl?.start_ms != null && ` · started ${new Date(tl.start_ms).toLocaleString()}`}
            {live && " · live"}
          </div>
        </div>
        <Button variant="ghost" onClick={() => openContextWindow(id)}>Context window…</Button>
        <Button variant="ghost" onClick={close}>Close</Button>
      </div>

      <div className="chat-stats-body">
        {err && <div className="empty" role="alert" style={{ color: "#ff8a8a" }}>Couldn't load · {err}</div>}
        {!tl && !err && <Loader className="loading-state" size={16} label="Reading timings…" />}
        {empty && <div className="empty rt-empty">No timings recorded for this chat — runs from before this version don't have them.</div>}

        {tl && t && !empty && (
          <div className="cstats">
            {tl.partial && <div className="rt-banner" role="note">Part of this timeline is estimated from transcript timestamps — those blocks are drawn dashed and faded.</div>}
            <div className="stiles">
              <Fact label="Wall time" value={fmtDur(t.wall_ms)} sub={`${t.requests} requests · ${t.tool_calls} tool calls`} />
              <Fact label="Model time" value={fmtDur(t.model_ms)} sub={`${t.failed_requests} failed · ${t.retries} retries · ${t.fallbacks} fallbacks`} />
              <Fact label="Tools" value={fmtDur(t.tool_ms)} sub={`${t.tool_calls} calls · ${t.tool_errors} errors`} />
              <Fact label="Waiting on you" value={fmtDur(t.you_ms)} sub="approvals and questions" />
              <Fact label="Paused / parked" value={fmtDur(t.paused_ms)} sub={`retry ${fmtDur(t.retry_ms)} · idle ${fmtDur(t.idle_ms)}`} />
              <Fact label="Cost" value={fmt$(t.cost)} sub={costNote} />
            </div>

            <SplitBar t={t} />

            <Lanes tl={tl} taskId={id} />

            <div className="cgrid">
              <div className="chart">
                <div className="chead"><span>Cost by model</span><span className="legend muted">{t.by_model.length} {t.by_model.length === 1 ? "model" : "models"}</span></div>
                {t.by_model.map((m) => (
                  <div key={m.model} className="rt-row">
                    <i className="tdotc" style={{ background: modelColor(m.model) }} />
                    <span className="rt-rname">{modelInfo(m.model).name}<span className="bsub">{m.requests} requests{m.failed ? ` · ${m.failed} failed` : ""} · {fmtDur(m.ms)}</span></span>
                    <span className="bright">{fmt$(m.cost)}</span>
                  </div>
                ))}
                {!t.by_model.length && <div className="empty" style={{ padding: 12 }}>No model requests recorded</div>}
              </div>
              <div className="chart">
                <div className="chead"><span>Slowest tools</span><span className="legend muted">click to jump</span></div>
                {t.slowest_tools.map((s, i) => (
                  <button key={`${s.item}:${i}`} type="button" className="rt-row rt-jump" onClick={() => void jumpToRow(id, s.lane, s.item)}>
                    <span className="rt-rname mono">{s.name}<span className="bsub">+{fmtDur(s.start_ms - (tl.start_ms ?? s.start_ms))}{s.lane !== "main" ? ` · ${laneTitle(tl, s.lane)}` : ""}</span></span>
                    <span className="bright">{fmtDur(s.ms)}</span>
                  </button>
                ))}
                {!t.slowest_tools.length && <div className="empty" style={{ padding: 12 }}>No tool calls</div>}
              </div>
            </div>
          </div>
        )}
      </div>
    </Modal>
  );
}

/** Open the panel for a chat. The menu it came from is closed by the caller. */
export const openRunTimeline = (id: string) => set({ runTimeline: id });
