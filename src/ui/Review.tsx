import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, FileDiff, ATTRIBUTIONS, Attribution, BisectEditsInfo, VisualEntry } from "../api";
import { ImageExplanation } from "./ImageExplanation";
import { flash, go, saveSettings, useStore } from "../store";
import { Tooltip } from "./primitives";
import { Check, ChevronLeft, SearchCode, Sparkles, X } from "lucide-react";
import { Loader, Button, ConfirmDialog, Dropdown, Input, Kbd, Pressable, Switch } from "./primitives";
import { BisectDialog } from "./Bisect";
import { diffRows, safeName } from "./diffRows";

/** How many changed files / diff lines the view mounts at once. A review either
 *  walks the list with the arrow keys (bounded, and it only reads what's on
 *  screen) or scrolls the diff, so the visible cap costs nothing. */
const FILE_CAP = 500;
const LINE_CAP = 4000;

/** RFC3339 → a short local time; the raw text if it doesn't parse. */
function when(at: string | null): string {
  if (!at) return "";
  const d = new Date(at);
  return Number.isNaN(d.getTime()) ? at : d.toLocaleString();
}

/** Before/after screenshots of the URLs the agent compared. Rendered only when
 *  there is at least one entry, so a chat that never captured any shows nothing. */
function VisualChanges({ taskId, entries, onEntries, onCleared }: { taskId: string; entries: VisualEntry[]; onEntries: (e: VisualEntry[]) => void; onCleared: () => void }) {
  const [busy, setBusy] = useState<"" | "recapture" | "discard">("");
  const [err, setErr] = useState("");
  const recapture = async () => {
    if (busy) return;
    setBusy("recapture");
    setErr("");
    try {
      const d = await api.visualRecapture(taskId);
      // Every capture is a new file name (the backend never overwrites one),
      // so the path-keyed image cache can't show a stale picture here.
      onEntries(d.entries);
    } catch (e) {
      setErr(String(e));
      flash(String(e));
    } finally {
      setBusy("");
    }
  };
  const discard = async () => {
    if (busy) return;
    setBusy("discard");
    setErr("");
    try {
      await api.visualClear(taskId);
      onCleared();
    } catch (e) {
      setErr(String(e));
      flash(String(e));
    } finally {
      setBusy("");
    }
  };
  return (
    <details className="review-visual" open>
      <summary>Visual changes ({entries.length})</summary>
      {err && <div className="review-visual-err" role="alert">{err}</div>}
      <div className="review-visual-body">
        <div className="review-visual-actions">
          <Button variant="ghost" style={{ height: 24 }} disabled={!!busy} onClick={() => void recapture()}>
            {busy === "recapture" && <Loader size={12} />}Recapture after
          </Button>
          <Button variant="ghost" style={{ height: 24 }} disabled={!!busy} onClick={() => void discard()}>
            {busy === "discard" && <Loader size={12} />}Discard
          </Button>
        </div>
        {entries.map((entry, i) => (
          <div key={`${i}:${entry.before_at}:${entry.after_at ?? ""}`} className="review-visual-entry">
            <ImageExplanation source={JSON.stringify({ title: safeName(entry.url), src: entry.before, ...(entry.after ? { after: entry.after } : {}) })} />
            <div className="review-visual-meta">
              Before {when(entry.before_at)}{entry.after_at ? ` · after ${when(entry.after_at)}` : ""}
            </div>
            {!entry.after && <div className="review-visual-note" role="status">No after capture yet — Recapture to compare.</div>}
          </div>
        ))}
      </div>
    </details>
  );
}

export function Review() {
  const task = useStore((s) => (s.task ? s.tasks[s.task] : null));
  const settings = useStore((s) => s.settings);
  const [data, setData] = useState<{ git: boolean; files: FileDiff[] } | null>(null);
  const [sel, setSel] = useState(0);
  const [reviewed, setReviewed] = useState<Record<string, "ok" | "rej">>({});
  const [msg, setMsg] = useState("");
  const [err, setErr] = useState("");
  const [genning, setGenning] = useState(false);
  const [genErr, setGenErr] = useState("");
  const [confirmReplace, setConfirmReplace] = useState(false);
  const [bisectInfo, setBisectInfo] = useState<BisectEditsInfo | null>(null);
  const [bisectOpen, setBisectOpen] = useState(false);
  const taskId = task?.id;
  const bisectRunning = useStore((s) => (taskId ? s.bisect[taskId]?.running === true : false));
  const load = useCallback(() => {
    if (!taskId) return;
    setErr("");
    api.review(taskId).then(setData).catch((e) => { setData(null); setErr(String(e)); flash(String(e)); });
    // The edit list decides whether "Find breaking edit" can run at all. A
    // failure here only disables that button: it must not take the diff down.
    api.bisectEdits(taskId).then((i) => setBisectInfo(i ?? null)).catch((e) => setBisectInfo({ edits: [], available: false, reason: String(e) }));
  }, [taskId]);
  useEffect(load, [load]);
  // The visual diff is optional: a backend without the command (or any other
  // failure) just means no section, never a broken Review.
  const [visual, setVisual] = useState<VisualEntry[]>([]);
  useEffect(() => {
    setVisual([]);
    if (!taskId) return;
    let live = true;
    api.visualDiff(taskId).then((d) => { if (live) setVisual(Array.isArray(d?.entries) ? d.entries : []); }).catch(() => {});
    return () => { live = false; };
  }, [taskId]);
  // The newest title, for the seed effect: it must seed the box when the chat
  // changes, not on every title change the user makes while editing.
  const titleRef = useRef(task?.title);
  titleRef.current = task?.title;
  useEffect(() => { setMsg(titleRef.current ?? ""); setGenErr(""); }, [taskId]);

  const attribution: Attribution = settings?.git_attribution ?? "co-authored-by";
  const verify = settings?.git_commit_verify === true;

  // Its own hook so the `shownFiles` memo below has a stable dependency instead
  // of a fresh `[]`/array literal every render.
  const files = useMemo(() => data?.files ?? [], [data]);
  const cur = files[Math.min(sel, files.length - 1)];
  const nextUnreviewed = (r: Record<string, string>) => {
    for (let i = 1; i <= files.length; i++) {
      const j = (sel + i) % files.length;
      // The loop runs at most `files.length` times, so `j` never leaves it.
      const f = files[j];
      if (f && !r[f.path]) return j;
    }
    return sel;
  };
  const accept = () => {
    if (!cur) return;
    const r = { ...reviewed, [cur.path]: "ok" as const };
    setReviewed(r);
    setSel(nextUnreviewed(r));
  };
  const reject = async () => {
    if (!cur || !task) return;
    try {
      await api.revertFile(task.id, cur.path);
      setReviewed({ ...reviewed, [cur.path]: "rej" });
      load();
    } catch (e) {
      flash(String(e));
    }
  };
  /** Draft a message from the diff. Never clears what the user already typed
   *  without asking, and says when the answer was a local fallback rather than
   *  the model's — a guess presented as the model's is worse than a plain one. */
  const runGenerate = async () => {
    if (!task || !data?.git || genning) return;
    setGenning(true);
    setGenErr("");
    try {
      const d = await api.commitMessage(task.id);
      setMsg(d.message);
      if (d.source === "fallback") setGenErr("No model answer — wrote a plain message from the changed files. Edit it before committing.");
    } catch (e) {
      setGenErr(String(e));
    } finally {
      setGenning(false);
    }
  };

  /** Entry point for the button. Asks before overwriting text the user typed,
   *  in the app's own dialog rather than `window.confirm` — which in a native
   *  window paints browser chrome and is unreachable by this app's keyboard. */
  const generate = () => {
    if (!task || !data?.git || genning) return;
    if (msg.trim() && msg.trim() !== (task.title ?? "").trim()) { setConfirmReplace(true); return; }
    void runGenerate();
  };
  const commit = async () => {
    if (!task || !data?.git) return;
    try {
      await api.commit(task.id, msg.trim() || task.title);
      go("session");
    } catch (e) {
      flash(String(e));
    }
  };

  useEffect(() => {
    const kd = (e: KeyboardEvent) => {
      // The bisect panel is a dialog over this view: its keys are its own.
      if (bisectOpen) return;
      if (/TEXTAREA|INPUT/.test((document.activeElement as HTMLElement)?.tagName)) return;
      const k = e.key.toLowerCase();
      if (k === "a" && !e.ctrlKey) accept();
      if (k === "x") void reject();
      if (k === "arrowdown" || k === "j") setSel((s) => Math.min(s + 1, files.length - 1));
      if (k === "arrowup" || k === "k") setSel((s) => Math.max(s - 1, 0));
      if ((e.ctrlKey || e.metaKey) && e.key === "Enter") void commit();
    };
    window.addEventListener("keydown", kd);
    return () => window.removeEventListener("keydown", kd);
  });

  // Above the `!task` early return on purpose: a hook that only runs on one branch
  // changes the hook count between renders, which React does not allow.
  const shownFiles = useMemo(() => files.slice(0, FILE_CAP), [files]);
  const n = Object.keys(reviewed).length;
  const lines = useMemo(() => diffRows(cur?.lines ?? []), [cur]);

  if (!task) return null;
  // A run already going (the agent's, through its tool, or one started before
  // this view remounted) can always be opened to watch or cancel it.
  const canBisect = bisectRunning || (bisectInfo?.available === true && bisectInfo.edits.length > 0);
  const bisectWhy = bisectRunning
    ? "A bisect is running for this chat — open it to watch or cancel"
    : !bisectInfo
      ? "Reading this chat's edit history…"
      : canBisect
        ? "Run a test command against each step of this chat's edit history to find the edit that broke it"
        : bisectInfo.reason || "This chat has no file edits to search";

  return (
    <>
      <div className="shdr review-header">
        <div className="review-summary">
          <Button variant="ghost" onClick={() => go("session")}><ChevronLeft size={13} />Back</Button>
          <div className="review-title">Review changes</div>
          {task.branch && <div className="mono review-branch" title={safeName(task.branch + (task.base_branch && task.worktree ? ` off ${task.base_branch}` : ""))}>{safeName(task.branch)}{task.base_branch && task.worktree ? ` off ${safeName(task.base_branch)}` : ""}</div>}
          <div className="review-progress">
            <div>{n} of {files.length} reviewed</div>
            <div className="bar3" style={{ width: 64 }}><div style={{ width: files.length ? (n / files.length) * 100 + "%" : 0, background: "var(--violet)" }} /></div>
          </div>
          {/* The span carries the tooltip: a disabled button gets no pointer
              events, and a disabled button is exactly when the reason matters. */}
          <Tooltip content={bisectWhy}>
            <span style={{ display: "inline-flex", flex: "none" }}>
              <Button variant="ghost" disabled={!canBisect} onClick={() => setBisectOpen(true)}>
                {bisectRunning ? <Loader size={12} /> : <SearchCode size={13} />}{bisectRunning ? "Bisect running…" : "Find breaking edit…"}
              </Button>
            </span>
          </Tooltip>
        </div>
        {data?.git && (
          <div className="review-commit">
            <Button variant="ghost" disabled={!files.length || genning} onClick={() => void generate()}>
              {genning ? <Loader size={12} /> : <Sparkles size={13} />}Generate
            </Button>
            <Input className="input review-message" aria-label="Commit message" value={msg} onChange={(e) => setMsg(e.currentTarget.value)} placeholder="Commit message" />
            {/* How the commit will be attributed, and whether the repo's own
                hooks run. Both are settings, but they are exactly what a reader
                of `git log` will see of this commit, so they belong next to the
                button rather than one screen away. */}
            <Dropdown search={false} showHint={false} placeholder="Commit attribution" style={{ width: 160, height: 28, flex: "none" }} value={attribution} onChange={(v) => void saveSettings({ git_attribution: v as Attribution })} options={ATTRIBUTIONS.map((a) => ({ value: a.id, label: a.label, hint: a.hint }))} />
            <Switch label="Run repo git hooks" hint={"Run the repository's own pre-commit hooks when the agent commits. Off by default: a hook is code the repository supplies, and the agent commits without asking."} checked={verify} onChange={(v) => void saveSettings({ git_commit_verify: v })} small />
            <Button variant="primary" disabled={!files.length} onClick={commit}>Commit<Kbd>Ctrl Enter</Kbd></Button>
          </div>
        )}
      </div>
      {genErr && data?.git && (
        <div className="desc" role="status" style={{ padding: "0 14px 6px", color: "var(--st-pause, #fbbf24)", fontSize: 11.5 }}>{genErr}</div>
      )}
      {visual.length > 0 && <VisualChanges taskId={task.id} entries={visual} onEntries={setVisual} onCleared={() => setVisual([])} />}
      {err ? (
        <div className="empty" style={{ paddingTop: 80, color: "#ff8a8a" }} role="alert">
          Couldn't load the diff · {err}
          <div style={{ marginTop: 8 }}><Button style={{ height: 26 }} onClick={load}>Try again</Button></div>
        </div>
      ) : !data ? <Loader className="loading-state" size={16} label="Loading diff…" /> : files.length === 0 ? (
        <div className="empty" style={{ paddingTop: 80 }}>No changes yet.</div>
      ) : (
        <div className="diffview">
          <div className="flist">
            {shownFiles.map((f, i) => {
              const r = reviewed[f.path];
              return (
                <Tooltip key={f.path} content={safeName(f.path)}><Pressable className={"frow" + (i === sel ? " on" : "")} onClick={() => setSel(i)}>
                  <span className="mono" style={{ width: 12, fontSize: 10.5, fontWeight: 600, color: f.status === "A" ? "var(--diff-add)" : f.status === "D" ? "var(--diff-del)" : "var(--st-pause)" }}>{f.status}</span>
                  <span style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", fontWeight: 500, direction: "rtl", textAlign: "left", color: r === "rej" ? "var(--dim)" : "var(--fg2)", textDecoration: r === "rej" ? "line-through" : "none" }}>{safeName(f.path)}</span>
                  <span className="mono" style={{ fontSize: 10, color: "var(--diff-add)" }}>+{f.add}</span>
                  <span className="mono" style={{ fontSize: 10, color: "var(--diff-del)" }}>−{f.del}</span>
                  <span style={{ width: 12, display: "flex", color: r === "ok" ? "var(--diff-add)" : "var(--diff-del)" }}>{r === "ok" ? <Check size={12} /> : r === "rej" ? <X size={12} /> : null}</span>
                </Pressable></Tooltip>
              );
            })}
            {files.length > shownFiles.length && <div style={{ padding: "4px 6px", fontSize: 11, color: "var(--dim)" }}>Showing the first {FILE_CAP} of {files.length} changed files</div>}
          </div>
          <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
            <div className="review-file-header">
              <span className="mono sel review-file-path" title={cur && safeName(cur.path)}>{cur && safeName(cur.path)}</span>
              <Button variant="ghost" style={{ height: 24 }} onClick={reject}>Revert<Kbd>X</Kbd></Button>
              <Button style={{ height: 24 }} onClick={accept}>Looks good<Kbd>A</Kbd></Button>
            </div>
            <div style={{ flex: 1, overflow: "auto", padding: "8px 0" }}>
              {lines.slice(0, LINE_CAP).map((l, i) => (
                <div key={sel + ":" + i} className="dline" style={{ background: l.bg, borderLeftColor: l.bar, animation: `olFade .18s ease both ${Math.min(i, 60) * 3}ms` }}>
                  <span>{l.a}</span><span>{l.b}</span><span style={{ color: l.signColor }}>{l.sign}</span><span style={{ color: l.color }}>{l.t}</span>
                </div>
              ))}
              {lines.length > LINE_CAP && <div style={{ padding: "6px 0", fontSize: 11, color: "var(--dim)" }}>Showing the first {LINE_CAP} of {lines.length} lines of this diff</div>}
            </div>
          </div>
        </div>
      )}
      {bisectOpen && <BisectDialog task={task} edits={bisectInfo?.edits ?? []} onClose={() => setBisectOpen(false)} onReverted={load} />}
      {confirmReplace && <ConfirmDialog
        title="Replace your commit message?"
        body="Generating a message will overwrite what you typed."
        confirm="Replace"
        onConfirm={() => void runGenerate()}
        onClose={() => setConfirmReplace(false)}
      />}
    </>
  );
}
