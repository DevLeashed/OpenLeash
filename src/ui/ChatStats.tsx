// One chat's stats, opened from "Stats…" in a chat's context menu.
//
// Deliberately its own fetch (`stats_chat`) rather than a slice of the polled
// `stats_get` payload: that returns every model, day and chat, and opening one
// chat does not need the rest of it. It also means this panel is correct for a
// chat that has been deleted — the id is all that is needed to find its numbers.
import { useEffect, useMemo, useState } from "react";
import { api, fmt$, fmtK } from "../api";
import { modelColor, modelInfo, set, useStore } from "../store";
import { Button, Loader, Modal, Tooltip } from "./primitives";
import "./ChatStats.css";
import { BarList, ChatStat, Counter, EMPTY, hitRate, pct, pctOf, secs, tokens } from "./Stats";

/** Events the router counts, in the order a reader wants them: what went wrong
 *  before what was merely interesting. `compactions` is last because a chat that
 *  compacted worked *well* — it is the one that stayed inside its context. */
const EVENTS: { key: string; label: string; hint: string }[] = [
  { key: "retries", label: "Retries", hint: "attempts the router repeated after a failure" },
  { key: "fallbacks", label: "Fallbacks", hint: "switches to another model or account mid-chat" },
  { key: "pauses", label: "Auto-pauses", hint: "every model was out, so the chat waited" },
  { key: "compactions", label: "Compactions", hint: "the conversation was summarised to free context" },
];

/** Pure: a chat's headline figures, as the rows the panel prints. Exported for tests.
 *
 *  `span` is wall-clock from first request to last, which is not the sum of the
 *  request times: a chat that spent four hours paused has a long span and a
 *  short sum, and conflating them would read as "slow model". Both are shown,
 *  labelled, for exactly that reason. */
export function chatFacts(c: ChatStat) {
  const t = c.total;
  const spent = t.requests + t.errors;
  const started = c.started ? new Date(c.started) : null;
  const ended = t.last_used ? new Date(t.last_used) : null;
  const span = started && ended ? Math.max(0, ended.getTime() - started.getTime()) : 0;
  return {
    tokens: tokens(t),
    requests: t.requests,
    errors: t.errors,
    errRate: spent ? t.errors / spent : 0,
    cost: t.cost,
    // Wall-clock end to end, as opposed to the sum of request times below.
    spanMs: span,
    // What the models actually spent generating. This is the honest "how long
    // was the agent thinking" number; `span` includes time the user was away.
    busyMs: t.total_ms,
    avgMs: t.requests ? t.total_ms / t.requests : 0,
    ttftMs: t.ttft_n ? t.ttft_ms / t.ttft_n : 0,
    ttftN: t.ttft_n,
    cache: hitRate(t),
    models: Object.keys(c.models).length,
    startedAt: started,
    endedAt: ended,
  };
}

// `secs` is right for a response time but unreadable for a span: 30964s.
function span(ms: number) {
  const m = Math.round(ms / 60_000);
  if (m < 1) return secs(ms);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return h < 24 ? `${h}h ${m % 60}m` : `${Math.floor(h / 24)}d ${h % 24}h`;
}

/** A usage id as a reader should see it. An id this build does not recognise
 *  passes through as the raw key rather than being dropped — a future spender
 *  still belongs in a breakdown whose rows have to add up to the headline. */
export function agentLabel(id: string): string {
  if (id === "main") return "Main agent";
  if (id === "compactor") return "Compactor";
  if (id === "keepalive") return "Cache keepalive";
  if (id.startsWith("sub:")) return `Sub ${id.slice(4)}`;
  return id;
}

/** Pure: the chat's `total` split by who spent it, biggest spender first.
 *  Exported for tests.
 *
 *  Zero rows are dropped so a component that recorded a request but no tokens
 *  and no cost does not leave an empty line — a row like that carries none of
 *  the total, so removing it cannot make the rows stop adding up. */
export function agentRows(by: Record<string, Counter> | undefined): { id: string; label: string; c: Counter }[] {
  return Object.entries(by ?? {})
    .filter(([, c]) => c.requests || c.errors || tokens(c) || c.cost)
    .sort((a, b) => b[1].cost - a[1].cost)
    .map(([id, c]) => ({ id, label: agentLabel(id), c }));
}

function Fact({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="sfact">
      <div className="label">{label}</div>
      <Morph value={value} />
      {sub && <div className="ssub">{sub}</div>}
    </div>
  );
}

// A plain span rather than the settings screen's `MorphText`: this panel can
// re-render on every poll tick, and a text morph animating numbers that are
// already on screen reads as flicker, not as a live figure.
function Morph({ value }: { value: string }) {
  return <div className="sval">{value}</div>;
}

/** The token mix as one horizontal bar — the share of the chat that was cache
 *  hits is the number that decides whether the run was cheap, and three tiles
 *  of raw counts do not make that visible. */
function MixBar({ c }: { c: Counter }) {
  const fresh = c.input + c.cache_write, cached = c.cache_read, out = c.output;
  const total = fresh + cached + out;
  if (!total) return null;
  const seg: [string, number, string][] = [["Input", fresh, "#a78bfa"], ["Cached", cached, "#85838c"], ["Output", out, "#c4b5fd"]];
  return (
    <div className="chart">
      <div className="chead"><span>Token mix</span><span className="legend">{fmtK(total)} total</span></div>
      <div style={{ display: "flex", height: 10, borderRadius: 5, overflow: "hidden", gap: 1 }}>
        {seg.filter(([, v]) => v > 0).map(([label, v, col]) => (
          <Tooltip key={label} content={`${label} · ${fmtK(v)} · ${pctOf(v, total)}`}>
            <div style={{ background: col, width: (v / total) * 100 + "%" }} />
          </Tooltip>
        ))}
      </div>
      <div className="legend" style={{ marginTop: 8, justifyContent: "flex-start", gap: 14 }}>
        {seg.map(([label, v, col]) => (
          <span key={label}><i style={{ background: col }} />{label} {fmtK(v)} · {pctOf(v, total)}</span>
        ))}
      </div>
    </div>
  );
}

export function ChatStatsPanel() {
  const id = useStore((s) => s.chatStats);
  const close = () => set({ chatStats: null });
  const task = useStore((s) => (id ? s.tasks[id] ?? null : null));
  const [row, setRow] = useState<[string, ChatStat] | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  // Poll while open, same as the Stats tab: a chat that is running is the one
  // whose numbers you most want to watch move.
  useEffect(() => {
    if (!id) return;
    let prev = "";
    const load = () => {
      api.statsChat(id).then((r) => {
        const k = JSON.stringify(r);
        if (k !== prev) { prev = k; setRow(r); setBusy(false); setErr(""); }
      }).catch((e) => { setBusy(false); setErr(String(e)); });
    };
    load();
    const iv = setInterval(load, 15_000);
    return () => clearInterval(iv);
  }, [id]);

  // `chatStats` can be set for a chat that no longer exists (opened from a menu,
  // then deleted elsewhere); the id is still enough to find its numbers.
  const title = row?.[0] || task?.title || "";
  const stat = row?.[1];
  const facts = useMemo(() => (stat ? chatFacts(stat) : null), [stat]);
  const agents = useMemo(() => (stat ? agentRows(stat.by_agent) : []), [stat]);

  if (!id) return null;
  return (
    <Modal className="modal-chat-stats" onClose={close}>
      <div className="chat-stats-header">
        <div className="chat-stats-heading">
          <div className="chat-stats-title">{title || "This chat"}</div>
          <div className="chat-stats-subtitle">
            {/* An empty title from the backend means the chat itself is gone —
                the numbers outlive it on purpose, so say that rather than
                showing a blank line. */}
            {!title && "Deleted chat — these are the numbers it spent"}
            {title && facts?.startedAt && `Started ${facts.startedAt.toLocaleString()}`}
            {title && facts?.startedAt && facts.spanMs > 0 && ` · ${span(facts.spanMs)} end to end`}
          </div>
        </div>
        <Button variant="ghost" onClick={close}>Close</Button>
      </div>

      <div className="chat-stats-body">
        {err && <div className="empty" role="alert" style={{ color: "#ff8a8a" }}>Couldn't load · {err}</div>}
        {!stat && !err && <Loader className="loading-state" size={16} label="Reading stats…" />}
        {busy && !stat && <Loader size={13} />}

        {stat && facts && (
          <div className="cstats">
            <div className="stiles">
              <Fact label="Tokens" value={fmtK(facts.tokens)} sub={`${facts.requests} requests`} />
              <Fact label="Est. cost" value={fmt$(facts.cost)} sub="at API list prices" />
              <Fact label="Cache hit" value={pct(facts.cache)} sub="of everything sent" />
              <Fact label="Errors" value={String(facts.errors)} sub={`${pct(facts.errRate)} of attempts`} />
            </div>

            <div className="stiles small">
              <Fact label="Avg response" value={secs(facts.avgMs)} />
              <Fact label="To first token" value={facts.ttftN ? secs(facts.ttftMs) : "—"} sub={facts.ttftN ? `${facts.ttftN} streamed` : "no streaming data"} />
              {/* Two different durations, and the gap between them is the point:
                  the models were busy for `busyMs` across a `spanMs` conversation. */}
              <Fact label="Agent time" value={secs(facts.busyMs)} sub="sum of responses" />
              <Fact label="Models used" value={String(facts.models)} sub={facts.models > 1 ? "the chat switched" : "one model throughout"} />
            </div>

            <MixBar c={stat.total} />

            {/* The Cline-style "what did each sub-agent cost on its own" view: the
                headline `total` is one number, and this is the only place that says
                which sub-agent or background task (compaction, keepalive) spent it.
                Additive to that headline — the rows sum to it — so a single "Main
                agent" row is still worth drawing: it confirms nothing hid in the
                total. Old stats files have no `by_agent` at all, hence the guard. */}
            {agents.length > 0 && (
              <BarList
                title="By agent"
                unit={`${fmt$(facts.cost)} across ${agents.length} ${agents.length === 1 ? "spender" : "spenders"}`}
                rows={agents.map((a) => ({
                  key: a.id, label: a.label, value: tokens(a.c),
                  right: `${fmt$(a.c.cost)} · ${fmtK(tokens(a.c))}`,
                  sub: `${fmt$(a.c.cost)} of ${fmt$(facts.cost)} · ${a.c.requests} requests`,
                }))}
              />
            )}

            {facts.models > 0 && (
              <div className="chart">
                <div className="chead">
                  <span>Models</span>
                  <span className="legend muted">{facts.models > 1 ? "the chat fell back between them" : "one model served this chat"}</span>
                </div>
                <div className="stable-wrap">
                  <table className="stable">
                    <thead><tr><th>Model</th><th>Requests</th><th>Input</th><th>Cached</th><th>Output</th><th>Cache hit</th><th>Avg</th><th>Est. cost</th></tr></thead>
                    <tbody>
                      {Object.entries(stat.models).sort((a, b) => tokens(b[1]) - tokens(a[1])).map(([mid, c]) => (
                        <tr key={mid}>
                          <td><i className="tdotc" style={{ background: modelColor(mid) }} />{modelInfo(mid).name}</td>
                          <td>{c.requests}</td><td>{fmtK(c.input + c.cache_write)}</td><td>{fmtK(c.cache_read)}</td>
                          <td>{fmtK(c.output)}</td><td>{pct(hitRate(c))}</td>
                          <td>{c.requests ? secs(c.total_ms / c.requests) : "—"}</td><td>{fmt$(c.cost)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            )}

            <div className="cgrid">
              <BarList
                title="Tools"
                unit="calls"
                rows={Object.entries(stat.tools).sort((a, b) => b[1].calls - a[1].calls).map(([k, t]) => ({
                  key: k, label: k.replace(/^mcp__/, "").replace("__", " · "), value: t.calls,
                  right: `${t.calls}${t.errors ? ` · ${pctOf(t.errors, t.calls)} err` : ""}`,
                  warn: t.errors / t.calls > 0.25,
                }))}
              />
              <BarList
                title="What happened"
                unit="events"
                rows={EVENTS.map((e) => ({
                  key: e.key, label: e.label, value: stat.events[e.key] ?? 0,
                  right: String(stat.events[e.key] ?? 0), sub: e.hint,
                  warn: e.key === "retries" && (stat.events[e.key] ?? 0) > facts.requests,
                }))}
              />
            </div>
        </div>
      )}

        {stat && !facts && <div className="empty">No recorded activity</div>}
      </div>
    </Modal>
  );
}

/** Open the panel for a chat. The menu it replaces is closed by the caller. */
export const openChatStats = (id: string) => set({ chatStats: id });

// Re-exported so the panel and the Stats tab agree on what "no data" means.
export { EMPTY };