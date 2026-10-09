import { useCallback, useEffect, useMemo, useState } from "react";
import { api, FileDiff, ATTRIBUTIONS, Attribution } from "../api";
import { flash, go, saveSettings, useStore } from "../store";
import { Tooltip } from "./primitives";
import { Check, ChevronLeft, Sparkles, X } from "lucide-react";
import { Loader, Button, Dropdown, Input, Kbd, Pressable, Switch } from "./primitives";

/** How many changed files / diff lines the view mounts at once. A review either
 *  walks the list with the arrow keys (bounded, and it only reads what's on
 *  screen) or scrolls the diff, so the visible cap costs nothing. */
const FILE_CAP = 500;
const LINE_CAP = 4000;

/** Unicode bidi overrides let a filename lie about its own name: `evil\u202Efdp.exe`
 *  renders as `evilepx.d` — a trojan renaming itself. A repo that ships one gets
 *  a user to revert the wrong file on the one screen meant to catch that, so the
 *  control characters are dropped and the override shown as the escape it is.
 *  The character classes are written as `\u` escapes, which is why this needs no
 *  `no-control-regex` disable — the rule does not fire on an escaped range. */
function safeName(s: string): string {
  return s.replace(/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`);
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
  const load = useCallback(() => {
    if (!task) return;
    setErr("");
    api.review(task.id).then(setData).catch((e) => { setData(null); setErr(String(e)); flash(String(e)); });
  }, [task?.id]);
  useEffect(load, [load]);
  useEffect(() => { setMsg(task?.title ?? ""); setGenErr(""); }, [task?.id]);

  const attribution: Attribution = settings?.git_attribution ?? "co-authored-by";
  const verify = settings?.git_commit_verify === true;

  const files = data?.files ?? [];
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
  const generate = async () => {
    if (!task || !data?.git || genning) return;
    if (msg.trim() && msg.trim() !== (task.title ?? "").trim() && !window.confirm("Replace the message you typed with a generated one?")) return;
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
  const lines = useMemo(() => {
    // Line numbers come from the diff's own hunk headers, so they have to be
    // counted in order — a skip that started at zero would renumber every line.
    // Same shape and same tokens as the transcript's diff rows: the sign and the
    // bar carry the hue, the code text stays in a readable body colour, and the
    // band is a 10% tint rather than a slab the text has to fight.
    let a = 0, b = 0;
    return (cur?.lines ?? []).map((l) => {
      if (l.k === "h") {
        const m = l.t.match(/-(\d+)(?:,\d+)? \+(\d+)/);
        if (m) { a = +m[1]! - 1; b = +m[2]! - 1; }
        return { a: "", b: "", sign: "", signColor: "var(--diff-hunk)", t: l.t, color: "var(--diff-hunk)", bg: "var(--diff-hunk-bg)", bar: "transparent" };
      }
      if (l.k === "a") { b++; return { a: "", b: String(b), sign: "+", signColor: "var(--diff-add)", t: l.t, color: "var(--fg2)", bg: "var(--diff-add-bg)", bar: "var(--diff-add)" }; }
      if (l.k === "d") { a++; return { a: String(a), b: "", sign: "−", signColor: "var(--diff-del)", t: l.t, color: "var(--fg2)", bg: "var(--diff-del-bg)", bar: "var(--diff-del)" }; }
      a++; b++;
      return { a: String(a), b: String(b), sign: "", signColor: "", t: l.t, color: "var(--mut)", bg: "transparent", bar: "transparent" };
    });
  }, [cur]);

  if (!task) return null;

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
    </>
  );
}
