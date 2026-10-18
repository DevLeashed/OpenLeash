/** "Which agent edit broke it?" — checkpoint bisect from the Review view.
 *
 *  The backend replays this chat's file history into scratch copies and runs a
 *  test command against chosen steps of it, halving the range until one edit is
 *  left. This panel only collects the command, asks before running it, shows
 *  the live `bisect` events the store folds (`State.bisect`), and offers the
 *  two follow-ups that make a culprit useful: see it in the chat, or revert it.
 *  A run the agent started through its own tool shows up here the same way. */
import { useEffect, useMemo, useRef, useState } from "react";
import { api, BisectEdit, BisectProbe, BisectReport, BisectVerdict, TaskSummary } from "../api";
import { flash, foldBisect, get, go, set, useStore, type BisectProgress } from "../store";
import { Button, ConfirmDialog, Dropdown, IconButton, Input, Loader, Modal, Pressable, Segmented, Tooltip } from "./primitives";
import { I } from "./icons";
import { SearchCode } from "lucide-react";
import { diffRows, safeName } from "./diffRows";

/** Diff lines shown for the culprit. The Review view itself caps far higher; a
 *  dialog that tried to mount a whole generated file would stop scrolling. */
const LINE_CAP = 1500;
const DEFAULT_TIMEOUT_S = 120;

const isExternal = (e: BisectEdit) => e.tool === "external";
const EXTERNAL_LABEL = "Changes made outside the agent's file edits (shell commands or you)";

/** One-line name for a step, as the range pickers and the probe log show it. */
export function editLabel(e: BisectEdit): string {
  return isExternal(e) ? `#${e.index} Outside changes · ${safeName(e.path)}` : `#${e.index} ${e.tool} · ${safeName(e.path)}`;
}

/** Which file state a probe ran against, in words. `state` k is "after step k";
 *  the bisect also tests `from - 1`, the files just before the range. */
export function stateLabel(state: number, from: number, edits: readonly BisectEdit[]): string {
  if (state <= 0) return "before any edit";
  if (state === from - 1) return `before edit #${from}`;
  const e = edits.find((x) => x.index === state);
  return e ? `after ${editLabel(e)}` : `after edit #${state}`;
}

/** Command output with the same escaping as a path, line by line so newlines
 *  and tabs survive: an ANSI or bidi control in a test log shows as its escape
 *  rather than restyling or reordering the text around it. */
function safeOutput(s: string): string {
  return s.split(/\r?\n/).map((l) => safeName(l).replace(/\\u0009/g, "\t")).join("\n");
}

const VERDICT: Record<BisectVerdict, { label: string; color: string }> = {
  good: { label: "passes", color: "var(--diff-add)" },
  bad: { label: "fails", color: "var(--diff-del)" },
  timeout: { label: "timed out", color: "var(--st-pause, #fbbf24)" },
  flaky: { label: "flaky", color: "var(--st-pause, #fbbf24)" },
  error: { label: "couldn't run", color: "#ff8a8a" },
};

/** Roughly how many probes a range costs: both ends, then one per halving. It
 *  only sizes the warning — the backend decides the real count. */
function estimate(steps: number, runs: number): number {
  return (2 + Math.ceil(Math.log2(Math.max(steps, 1)))) * runs;
}

/** The transcript row for an item, or the row that holds it. Consecutive tool
 *  calls fold into one group keyed by the group's first call, so a call inside a
 *  group has no row of its own: the nearest mounted row before it is the group. */
function findRow(taskId: string, itemId: string): HTMLElement | null {
  const q = (id: string) => {
    try {
      // Ids are harness-generated, but escaping keeps a stray quote from making
      // an invalid selector that throws.
      const esc = typeof CSS !== "undefined" && CSS.escape ? CSS.escape(id) : id.replace(/["\\]/g, "\\$&");
      return document.querySelector<HTMLElement>(`[data-item="${esc}"]`);
    } catch {
      return null;
    }
  };
  const direct = q(itemId);
  if (direct) return direct;
  const items = get().items[taskId] ?? [];
  const at = items.findIndex((i) => i.id === itemId);
  for (let i = at - 1; i >= 0; i--) {
    const row = q(items[i]!.id);
    if (row) return row;
  }
  return null;
}

/** Leave Review for the chat and bring the edit's row into view. The session
 *  mounts after the navigation, so the row is looked for over a few frames. */
export function jumpToEdit(taskId: string, edit: BisectEdit) {
  if (edit.sub_id) {
    // A subagent's calls live in its own transcript, not the main feed.
    go("session", { subOpen: edit.sub_id, details: true });
    flash(`That edit came from subagent ${safeName(edit.sub_id)} — its transcript is open in the side panel.`);
    return;
  }
  go("session");
  let tries = 0;
  const attempt = () => {
    const row = findRow(taskId, edit.item_id);
    if (row) { row.scrollIntoView?.({ block: "center" }); return; }
    if (++tries < 20) requestAnimationFrame(attempt);
    else flash("That edit's row isn't in the loaded transcript — scroll up to find it.");
  };
  requestAnimationFrame(attempt);
}

function Stat({ e }: { e: BisectEdit }) {
  return (
    <>
      <span className="mono" style={{ fontSize: 10.5, color: "var(--diff-add)", flex: "none" }}>+{e.add}</span>
      <span className="mono" style={{ fontSize: 10.5, color: "var(--diff-del)", flex: "none" }}>−{e.del}</span>
    </>
  );
}

/** The edit a bisect pinned down, with its diff and what to do about it. */
function Culprit({ task, edit, onReverted }: { task: TaskSummary; edit: BisectEdit; onReverted: () => void }) {
  const [asking, setAsking] = useState(false);
  const [reverted, setReverted] = useState(false);
  const [err, setErr] = useState("");
  const rows = useMemo(() => diffRows(edit.lines), [edit]);
  const ext = isExternal(edit);
  const revert = async () => {
    setErr("");
    try {
      await api.revertFile(task.id, edit.path);
      setReverted(true);
      flash(`Reverted ${safeName(edit.path)}`);
      onReverted();
    } catch (e) {
      setErr(String(e));
    }
  };
  return (
    <div className="sgroup" data-testid="bisect-culprit">
      <div className="srowx" style={{ minHeight: 0, alignItems: "flex-start", flexDirection: "column", gap: 4 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8, width: "100%", minWidth: 0 }}>
          <span className="bgtag" style={{ flex: "none" }}>{ext ? "outside" : safeName(edit.tool)}</span>
          <span className="mono" style={{ fontSize: 10.5, color: "var(--dim)", flex: "none" }}>#{edit.index}</span>
          <Tooltip content={safeName(edit.path)} truncated>
            <span className="mono" style={{ flex: 1, minWidth: 0, fontSize: 12, color: "var(--fg2)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{safeName(edit.path)}</span>
          </Tooltip>
          {!ext && <Stat e={edit} />}
        </div>
        {ext && <div className="desc" style={{ fontSize: 11.5 }}>{EXTERNAL_LABEL}</div>}
        {edit.sub_id && <div className="desc" style={{ fontSize: 11.5 }}>Made by subagent {safeName(edit.sub_id)}</div>}
      </div>
      {ext ? (
        edit.lines.length > 0 && (
          <ul style={{ margin: 0, padding: "6px 14px 10px 30px", maxHeight: 200, overflow: "auto" }}>
            {edit.lines.slice(0, LINE_CAP).map((l, i) => <li key={i} className="mono" style={{ fontSize: 11.5, color: "var(--fg2)" }}>{safeName(l.t)}</li>)}
          </ul>
        )
      ) : rows.length > 0 && (
        // `.diffview` scopes the `.dline` column styles the Review view uses;
        // `block` undoes its two-pane flex for a single scrolling column.
        <div className="diffview" style={{ display: "block", maxHeight: 280, overflow: "auto", padding: "6px 0", borderTop: "1px solid var(--ov-50)" }}>
          {rows.slice(0, LINE_CAP).map((l, i) => (
            <div key={i} className="dline" style={{ background: l.bg, borderLeftColor: l.bar }}>
              <span>{l.a}</span><span>{l.b}</span><span style={{ color: l.signColor }}>{l.sign}</span><span style={{ color: l.color }}>{l.t}</span>
            </div>
          ))}
          {rows.length > LINE_CAP && <div style={{ padding: "6px 14px", fontSize: 11, color: "var(--dim)" }}>Showing the first {LINE_CAP} of {rows.length} lines — the Review view has the rest</div>}
        </div>
      )}
      {!ext && (
        <div className="srowx" style={{ minHeight: 0, gap: 8, flexWrap: "wrap" }}>
          <Button onClick={() => jumpToEdit(task.id, edit)}>Jump to message</Button>
          <Tooltip content="Restores this file to how it was before the agent first touched it in this chat — that also drops every later edit to it, not just this one.">
            <span style={{ display: "inline-flex" }}>
              <Button variant="ghost" disabled={reverted} onClick={() => setAsking(true)}>{reverted ? "File reverted" : "Revert this file…"}</Button>
            </span>
          </Tooltip>
        </div>
      )}
      {err && <div className="desc" role="alert" style={{ padding: "0 14px 10px", color: "#ff8a8a", fontSize: 11.5 }}>Couldn't revert · {err}</div>}
      {asking && <ConfirmDialog
        title={`Revert ${safeName(edit.path)}?`}
        body="This restores the file to its state before the agent first touched it in this chat. It drops every agent edit to this file — the later ones too, not only the one that broke it."
        confirm="Revert file"
        danger
        onConfirm={() => void revert()}
        onClose={() => setAsking(false)}
      />}
    </div>
  );
}

function ProbeLog({ probes, from, edits }: { probes: readonly BisectProbe[]; from: number; edits: readonly BisectEdit[] }) {
  const [open, setOpen] = useState<number | null>(null);
  if (!probes.length) return null;
  return (
    <div className="sgroup" aria-label="Probe log" role="list">
      {probes.map((p, i) => {
        const v = VERDICT[p.verdict] ?? VERDICT.error;
        return (
          <div key={i} role="listitem">
            <Pressable className="srowx" aria-expanded={open === i} style={{ minHeight: 30, padding: "4px 12px", gap: 8, cursor: "pointer" }} onClick={() => setOpen(open === i ? null : i)}>
              <span className="mono" style={{ fontSize: 10.5, color: "var(--dim)", flex: "none" }}>{i + 1}</span>
              <span style={{ flex: 1, minWidth: 0, fontSize: 12, color: "var(--fg2)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{stateLabel(p.state, from, edits)}</span>
              <span data-verdict={p.verdict} style={{ fontSize: 11.5, fontWeight: 600, color: v.color, flex: "none" }}>{v.label}</span>
              <span className="mono" style={{ fontSize: 10.5, color: "var(--dim)", flex: "none" }}>{p.code === null ? "—" : `exit ${p.code}`} · {(p.ms / 1000).toFixed(1)}s</span>
            </Pressable>
            {open === i && (
              <pre className="mono" style={{ margin: 0, padding: "6px 12px 10px", maxHeight: 200, overflow: "auto", fontSize: 11, color: "var(--mut)", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>{p.output ? safeOutput(p.output) : "(no output)"}</pre>
            )}
          </div>
        );
      })}
    </div>
  );
}

function EditRow({ task, e }: { task: TaskSummary; e: BisectEdit }) {
  return (
    <div className="srowx" style={{ minHeight: 34, padding: "6px 12px", gap: 8 }}>
      <span style={{ flex: 1, minWidth: 0, fontSize: 12, color: "var(--fg2)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{isExternal(e) ? `#${e.index} ${EXTERNAL_LABEL}: ${safeName(e.path)}` : editLabel(e)}</span>
      {!isExternal(e) && <Stat e={e} />}
      {!isExternal(e) && <Button variant="ghost" style={{ height: 24 }} onClick={() => jumpToEdit(task.id, e)}>Jump</Button>}
    </div>
  );
}

function Outcome({ task, report, onReverted }: { task: TaskSummary; report: BisectReport; onReverted: () => void }) {
  const o = report.outcome;
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }} role="status" data-outcome={o}>
      <div style={{ fontWeight: 600, color: o === "found" ? "var(--fg)" : o === "all_good" ? "var(--diff-add)" : o === "cancelled" ? "var(--mut)" : "var(--st-pause, #fbbf24)" }}>
        {o === "found" && (report.culprit && isExternal(report.culprit) ? "The break came from changes made outside the agent's file edits" : "Found the edit that broke it")}
        {o === "all_good" && "The command passes after the last edit — nothing to bisect."}
        {o === "already_bad" && "The command already fails before the first edit in range."}
        {o === "inconclusive" && "Couldn't narrow it to one edit."}
        {o === "cancelled" && "Bisect cancelled."}
      </div>
      {report.message && <div className="desc" style={{ fontSize: 12, whiteSpace: "pre-wrap" }}>{report.message}</div>}
      {o === "already_bad" && (
        <div className="desc" style={{ fontSize: 11.5 }}>
          Scratch copies hold only the project's non-ignored files. If the command needs installed dependencies or build output (<code className="mono">node_modules</code>, a virtualenv, <code className="mono">target</code>), add them under Shared folders, or widen the range to start earlier.
        </div>
      )}
      {o === "found" && report.culprit && <Culprit task={task} edit={report.culprit} onReverted={onReverted} />}
      {o === "inconclusive" && report.candidates.length > 0 && (
        <>
          <div className="label">Edits that couldn't be told apart</div>
          <div className="sgroup" data-testid="bisect-candidates">
            {report.candidates.map((e) => <EditRow key={e.index} task={task} e={e} />)}
          </div>
        </>
      )}
    </div>
  );
}

/** Steps in the range and the probes so far, for the running view. */
function Running({ prog, edits, onCancel, cancelling }: { prog: BisectProgress; edits: readonly BisectEdit[]; onCancel: () => void; cancelling: boolean }) {
  const steps = prog.to >= prog.from && prog.to > 0 ? prog.to - prog.from + 1 : 0;
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 10 }} role="status" aria-live="polite">
        <Loader size={14} />
        <span style={{ flex: 1, minWidth: 0, fontSize: 12.5, color: "var(--fg2)" }}>
          {prog.current
            ? <>Probe {prog.current.step}: testing the files {stateLabel(prog.current.state, prog.from, edits)}</>
            : "Preparing scratch copies…"}
        </span>
        <Button variant="ghost" disabled={cancelling} onClick={onCancel}>{cancelling ? "Cancelling…" : "Cancel"}</Button>
      </div>
      {steps > 0 && <div className="desc" style={{ fontSize: 11.5 }}>Searching edits #{prog.from}–#{prog.to} ({steps} of {prog.total}). Closing this panel keeps the run going.</div>}
    </div>
  );
}

export function BisectDialog({ task, edits, onClose, onReverted }: { task: TaskSummary; edits: readonly BisectEdit[]; onClose: () => void; onReverted: () => void }) {
  const prog = useStore((s) => s.bisect[task.id]);
  const first = edits[0]?.index ?? 1;
  const last = edits[edits.length - 1]?.index ?? 1;
  const [phase, setPhase] = useState<"form" | "run">(prog ? "run" : "form");
  const [command, setCommand] = useState("");
  const [from, setFrom] = useState(String(first));
  const [to, setTo] = useState(String(last));
  const [timeout, setTimeoutS] = useState(String(DEFAULT_TIMEOUT_S));
  const [runs, setRuns] = useState(1);
  const [share, setShare] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [err, setErr] = useState("");
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  // A run that finished clears the Cancelling… state for the next one.
  useEffect(() => { if (!prog?.running) setCancelling(false); }, [prog?.running]);

  const f = Number(from), t = Number(to);
  const secs = Number(timeout);
  const shared = share.split(",").map((x) => x.trim()).filter(Boolean);
  const rangeBad = !(f <= t);
  const timeoutBad = !(Number.isFinite(secs) && secs > 0);
  const canRun = command.trim().length > 0 && !rangeBad && !timeoutBad && edits.length > 0;
  const steps = rangeBad ? 0 : edits.filter((e) => e.index >= f && e.index <= t).length;
  const options = useMemo(() => edits.map((e) => ({ value: String(e.index), label: editLabel(e), hint: isExternal(e) ? EXTERNAL_LABEL : undefined })), [edits]);

  const start = async () => {
    const prev = get().bisect[task.id];
    setErr("");
    const pending = foldBisect(prev, { phase: "start", from: f, to: t, total: edits.length });
    set((s) => ({ bisect: { ...s.bisect, [task.id]: pending } }));
    // A later start (including the backend's own start) makes events the
    // authority. Never let an older IPC resolve/refusal roll that stream back.
    // Keep this in the store, not mounted state: closing the panel must still
    // hydrate a no-event result, but must not revive a cancelled/superseded run.
    const isCurrent = (p: BisectProgress | undefined) => p?.generation === pending.generation && p.running;
    setPhase("run");
    try {
      // The whole-chat default goes as null so the backend picks its own ends.
      const r = await api.bisectRun(task.id, {
        command: command.trim(),
        from: f === first ? null : f,
        to: t === last ? null : t,
        timeout: Math.round(secs * 1000),
        runs,
        share: shared.length ? shared : null,
      });
      set((s) => isCurrent(s.bisect[task.id])
        ? { bisect: { ...s.bisect, [task.id]: foldBisect(s.bisect[task.id], { phase: "done", report: r }) } }
        : {});
    } catch (e) {
      // Only a still-current optimistic request can be refused. A rejection
      // arriving after a start/done event belongs to history, not this panel.
      if (!isCurrent(get().bisect[task.id])) return;
      set((s) => {
        if (!isCurrent(s.bisect[task.id])) return {};
        const b = { ...s.bisect };
        if (prev) b[task.id] = prev; else delete b[task.id];
        return { bisect: b };
      });
      if (mounted.current) { setErr(String(e)); setPhase("form"); } else flash(`Bisect didn't run · ${String(e)}`);
    }
  };
  const cancel = () => {
    setCancelling(true);
    api.bisectCancel(task.id)
      .then((ok) => { if (!ok) { setCancelling(false); flash("No bisect is running for this chat"); } })
      .catch((e) => { setCancelling(false); flash(String(e)); });
  };

  // A run in flight always takes the panel, including one the agent started
  // while the form was open: a second run would only be refused.
  const showRun = !!prog && (phase === "run" || prog.running);
  const field = { display: "flex", flexDirection: "column", gap: 6 } as const;
  return (
    <Modal onClose={onClose} ariaLabel="Find breaking edit" style={{ width: "min(680px, 100%)" }}>
      <div className="mh">
        <span style={{ display: "flex", color: "var(--mut2)" }}><SearchCode size={15} /></span>
        <span style={{ flex: 1, minWidth: 0 }}>Find the edit that broke it</span>
        <IconButton label="Close" onClick={onClose}>{I.close()}</IconButton>
      </div>
      {showRun && prog ? (
        <div className="mb" style={{ gap: 12 }}>
          {prog.running
            ? <Running prog={prog} edits={edits} onCancel={cancel} cancelling={cancelling} />
            : prog.report && <Outcome task={task} report={prog.report} onReverted={onReverted} />}
          {prog.probes.length > 0 && <div className="label">Probes</div>}
          <ProbeLog probes={prog.probes} from={prog.from} edits={edits} />
        </div>
      ) : (
        <div className="mb" style={{ gap: 12 }}>
          <div className="desc" style={{ fontSize: 12 }}>
            Give a command that exits 0 when things work. It runs against copies of this chat's files at chosen points in its edit history, halving the range until one edit is left.
          </div>
          <div style={field}>
            <label className="label" htmlFor="bisect-cmd">Test command</label>
            <Input id="bisect-cmd" className="input mono" autoFocus value={command} placeholder="npm test -- src/parser.test.ts" onChange={(e) => setCommand(e.currentTarget.value)}
              onKeyDown={(e) => { if (e.key === "Enter" && canRun) { e.preventDefault(); setConfirming(true); } }} />
          </div>
          <div className="fieldrow">
            <div style={{ ...field, flex: "1 1 0", minWidth: 0 }}>
              <span className="label">From</span>
              <Dropdown placeholder="First edit in range" value={from} options={options} onChange={setFrom} showHint={false} />
            </div>
            <div style={{ ...field, flex: "1 1 0", minWidth: 0 }}>
              <span className="label">To</span>
              <Dropdown placeholder="Last edit in range" value={to} options={options} onChange={setTo} showHint={false} />
            </div>
          </div>
          {rangeBad && <div className="desc" role="alert" style={{ color: "#ff8a8a", fontSize: 11.5 }}>The first edit has to come before the last one.</div>}
          <div className="fieldrow" style={{ alignItems: "flex-end" }}>
            <div style={{ ...field, width: 150, flex: "none" }}>
              <label className="label" htmlFor="bisect-timeout">Timeout per probe (s)</label>
              <Input id="bisect-timeout" className={"input" + (timeoutBad ? " bad" : "")} type="number" min={1} value={timeout} onChange={(e) => setTimeoutS(e.currentTarget.value)} />
            </div>
            <div style={{ ...field, flex: "none" }}>
              <span className="label">Runs per step</span>
              <Segmented label="Runs per step" value={runs} onChange={setRuns} options={[
                { value: 1, label: "1" },
                { value: 2, label: "2", hint: "Run twice; a step that disagrees with itself is reported as flaky" },
                { value: 3, label: "3", hint: "Run three times; a step that disagrees with itself is reported as flaky" },
              ]} />
            </div>
          </div>
          <div style={field}>
            <label className="label" htmlFor="bisect-share">Shared folders (optional)</label>
            <Input id="bisect-share" className="input mono" value={share} placeholder="node_modules, .venv" onChange={(e) => setShare(e.currentTarget.value)} />
            <div className="desc" style={{ fontSize: 11.5 }}>
              Comma-separated folders, relative to the project, linked into every scratch copy instead of copied (copies hold only non-ignored files). They are your real folders: the command can write into them.
            </div>
          </div>
          {err && <div className="desc" role="alert" style={{ color: "#ff8a8a", fontSize: 12 }}>Bisect didn't run · {err}</div>}
        </div>
      )}
      <div className="mf">
        <div style={{ flex: 1 }} />
        <Button variant="ghost" onClick={onClose}>Close</Button>
        {showRun && prog && !prog.running && <Button onClick={() => { setPhase("form"); setErr(""); }}>New run</Button>}
        {!showRun && <Button variant="primary" disabled={!canRun} onClick={() => setConfirming(true)}>Run bisect…</Button>}
      </div>
      {confirming && <ConfirmDialog
        title="Run this command?"
        style={{ width: "min(500px, 100%)" }}
        body={
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }} data-testid="bisect-confirm">
            <div className="desc">
              This exact command will run several times (about {estimate(steps, runs)}), each time in an isolated scratch copy of this chat's file history — not in your working folder.
            </div>
            <code className="mono" style={{ display: "block", padding: "8px 10px", borderRadius: 8, background: "var(--ov-280)", fontSize: 12, whiteSpace: "pre-wrap", wordBreak: "break-all" }}>{safeName(command.trim())}</code>
            <div className="desc">This chat's normal shell permission rules apply: a Deny rule or plan mode still refuses it.</div>
            {shared.length > 0 && (
              <div className="desc" style={{ color: "var(--st-pause, #fbbf24)" }}>
                Shared folders are linked, not copied: anything the command writes into {shared.map(safeName).join(", ")} changes your real files.
              </div>
            )}
          </div>
        }
        confirm="Run bisect"
        onConfirm={() => void start()}
        onClose={() => setConfirming(false)}
      />}
    </Modal>
  );
}
