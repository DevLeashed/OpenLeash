// The race comparison view: every attempt of one race side by side, with its
// diff, checks, cost, time and the judge's ranking, and the actions that end
// a race — pick a winner, merge it, clean up the rest.
import { useCallback, useEffect, useState } from "react";
import { baseName, fmt$, normPath, raceApi, type Race, type RaceAttempt, type RaceAttemptStats, type RaceRank, type TaskSummary } from "../api";
import { flash, go, modelInfo, useNow, useStore } from "../store";
import { archiveRace, attemptElapsed, budgetMessage, closeRace, fmtDuration, isBudgetError, openWorktree, RACE_STATE, raceCost, upsertRace, useRaces } from "../race";
import { deleteTask } from "./Chrome";
import { chatStatus, I, statusOf } from "./icons";
import { Button, ConfirmDialog, Dropdown, IconButton, Modal, Switch } from "./primitives";
import { effortOpts, RaceSetupDialog, useModelOptions } from "./RaceSetup";
import "./Race.css";

/** Mounted once in the overlay layer: the composer's race setup and the comparison view. */
export function RaceOverlays() {
  return <RaceSetupDialog />;
}

/** The comparison screen: reached by pressing a race group in the sidebar. */
export function RaceScreen() {
  const race = useRaces((s) => (s.open ? s.races[s.open] ?? null : null));
  if (!race) {
    return <div style={{ flex: 1, padding: "24px 28px" }}>
      <div className="empty">This race is gone.</div>
      <Button variant="ghost" onClick={closeRace}>Close</Button>
    </div>;
  }
  return <RaceView key={race.id} race={race} onClose={closeRace} />;
}

function effortName(model: string, effort: number) {
  return effortOpts(model).find((o) => o.value === String(effort))?.label ?? null;
}

/** The project's checked-out branch when the race's project is the one open, which is where a merge lands. */
function useTargetBranch(project: string): string | null {
  return useStore((s) => (s.projectInfo && s.projectInfo.git && normPath(s.projectInfo.path) === normPath(project) && s.projectInfo.branch ? s.projectInfo.branch : null));
}

type Dialog = null | "merge" | "cleanup" | "forget" | { budget: string };

export function RaceView({ race, onClose }: { race: Race; onClose: () => void }) {
  useNow(5_000);
  const tasks = useStore((s) => s.tasks);
  const [stats, setStats] = useState<Record<string, RaceAttemptStats>>({});
  const [loading, setLoading] = useState(false);
  const [judgeModel, setJudgeModel] = useState("");
  const [dialog, setDialog] = useState<Dialog>(null);
  const [removeWorktree, setRemoveWorktree] = useState(false);
  const [busy, setBusy] = useState(false);
  const target = useTargetBranch(race.project);
  const options = useModelOptions([race.judge_model, ...(race.judge ? [race.judge.model] : [])]);

  const refresh = useCallback(() => {
    setLoading(true);
    raceApi.detail(race.id)
      .then((d) => {
        if (d?.race) upsertRace(d.race);
        setStats(Object.fromEntries((d?.stats ?? []).map((x) => [x.task_id, x])));
      })
      .catch((e) => flash(String(e)))
      .finally(() => setLoading(false));
  }, [race.id]);
  // On open, and again when the race settles into a new state (checks done,
  // judged): diff stats are a git walk per worktree, not something to poll.
  useEffect(() => { refresh(); }, [refresh, race.state]);

  const ranks = new Map<string, RaceRank>((race.judge?.ranking ?? []).map((r) => [r.task_id, r]));
  const winner = race.winner;
  const winnerTask = winner ? tasks[winner] : undefined;
  const busyState = race.state === "checking" || race.state === "judging";
  const losers = race.attempts.map((a) => a.task_id).filter((id) => id !== winner && tasks[id]);
  const st = RACE_STATE[race.state] ?? RACE_STATE.running;

  const judge = async (force: boolean) => {
    setBusy(true);
    try {
      upsertRace(await raceApi.judge(race.id, judgeModel || null, force));
    } catch (e) {
      if (!force && isBudgetError(e)) setDialog({ budget: budgetMessage(e) });
      else flash(String(e));
    } finally { setBusy(false); }
  };
  const pick = async (id: string | null) => {
    try { upsertRace(await raceApi.pick(race.id, id)); } catch (e) { flash(String(e)); }
  };
  const merge = async () => {
    setBusy(true);
    try {
      const r = await raceApi.merge(race.id);
      upsertRace(r.race);
      flash(r.message || "Winner merged");
    } catch (e) { flash(String(e)); } finally { setBusy(false); }
  };
  const cleanup = async () => {
    const ids = losers;
    let n = 0;
    for (const id of ids) if (await deleteTask(id, removeWorktree)) n++;
    if (ids.length > 1) flash(`Deleted ${n} of ${ids.length} losing chats${removeWorktree ? " · worktrees removed · branches kept" : " · worktrees kept on disk"}`);
  };
  const forget = async () => {
    try { await raceApi.forget(race.id); onClose(); flash("Race forgotten · its chats are still in the sidebar"); } catch (e) { flash(String(e)); }
  };
  const openChat = (id: string) => { onClose(); go("session", { task: id }); };

  const branchOfWinner = winnerTask?.branch || "the winner's branch";
  const into = target ?? "the project's current branch";

  return (
    <div className="race-screen" role="region" aria-label="Race">
      <div className="race-header">
        <div className="race-heading">
          <div className="race-title" title={race.prompt}>{race.prompt || "Race"}</div>
          <div className="race-subtitle">
            {race.attempts.length} attempts · {baseName(race.project)}{race.base_branch ? ` · from ${race.base_branch}` : ""} · judge {modelInfo(race.judge?.model || race.judge_model).name} · {fmt$(raceCost(race, tasks))} total
          </div>
        </div>
        <span className="race-pill" style={{ ["--race-c" as string]: st.color }}>{st.label}</span>
        <Button variant="ghost" onClick={refresh} disabled={loading}>{loading ? "Refreshing…" : "Refresh"}</Button>
        <Button variant="ghost" onClick={onClose}>Close</Button>
      </div>
      <div className="race-body">
        {(race.judge || race.error) && (
          <div className={"race-summary" + (race.judge?.error || race.error ? " bad" : "")}>
            {race.judge?.summary && <div>{race.judge.summary}</div>}
            {race.judge?.error && <div className="race-err">Judge: {race.judge.error}</div>}
            {race.error && <div className="race-err">{race.error}</div>}
            {race.judge && <div className="race-meta">Judged by {modelInfo(race.judge.model).name}{race.judge.cost ? ` · ${fmt$(race.judge.cost)}` : ""}</div>}
          </div>
        )}
        {race.merged && <div className="race-summary good">Merged: {race.merged}</div>}
        <div className="race-grid">
          {race.attempts.map((a, i) => (
            <AttemptCard key={a.task_id} n={i + 1} attempt={a} task={tasks[a.task_id]} stats={stats[a.task_id]} rank={ranks.get(a.task_id)}
              winner={winner === a.task_id} onOpen={() => openChat(a.task_id)} onFolder={() => { const t = tasks[a.task_id]; if (t) openWorktree(t.cwd); }} onPick={() => void pick(winner === a.task_id ? null : a.task_id)} />
          ))}
        </div>
      </div>
      <div className="mf race-actions">
        <Dropdown value={judgeModel} options={[{ value: "", label: `Judge: ${modelInfo(race.judge_model).name}`, hint: "race default" }, ...options]} onChange={setJudgeModel} style={{ fontSize: 12, minWidth: 180 }} />
        <Button onClick={() => void judge(false)} disabled={busy || busyState}>{busyState ? `${st.label}…` : race.judge ? "Re-judge" : "Judge now"}</Button>
        <div style={{ flex: 1 }} />
        <Button variant="ghost" onClick={() => setDialog("forget")}>Forget race</Button>
        <IconButton label="Archive race" onClick={() => void archiveRace(race.id)} disabled={busyState}>{I.archive(14)}</IconButton>
        <Button variant="ghost" onClick={() => { setRemoveWorktree(false); setDialog("cleanup"); }} disabled={!winner || !losers.length}>Clean up losers…</Button>
        <Button variant="primary" onClick={() => setDialog("merge")} disabled={!winner || busy || !!race.merged}>Merge winner into {into}…</Button>
      </div>

      {dialog === "merge" && (
        <ConfirmDialog title="Merge the winner?" confirm="Merge" onClose={() => setDialog(null)} onConfirm={() => void merge()}
          body={<div className="desc">
            Merges <b className="mono">{branchOfWinner}</b> into <b className="mono">{into}</b> with <span className="mono">git merge --no-ff</span>.
            Any uncommitted work in the winner's worktree{winnerTask?.cwd ? <> (<span className="mono">{winnerTask.cwd}</span>)</> : null} is committed first.
            If the project folder has uncommitted changes or the merge conflicts, nothing is merged.
          </div>} />
      )}
      {dialog === "cleanup" && (
        <Modal onClose={() => setDialog(null)} ariaLabel="Clean up losers" style={{ width: "min(440px, 100%)" }}>
          <div className="mh"><span style={{ flex: 1 }}>Delete {losers.length} losing chat{losers.length === 1 ? "" : "s"}?</span></div>
          <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <ul className="race-losers">{losers.map((id) => <li key={id}>{tasks[id]?.title ?? id}{tasks[id]?.worktree ? <span className="mono"> · {tasks[id]?.branch}</span> : null}</li>)}</ul>
            <div style={{ display: "flex", alignItems: "center", gap: 8, color: "var(--mut3)", fontSize: 11.5 }}>
              <Switch label="Also permanently remove these and any uncommitted files in them" checked={removeWorktree} small onChange={setRemoveWorktree} />
              <span>Also remove their worktrees and any uncommitted files in them; branches remain.</span>
            </div>
            {!removeWorktree && <div style={{ fontSize: 11, color: "var(--dim)" }}>Their worktrees stay on disk.</div>}
          </div>
          <div className="mf">
            <div style={{ flex: 1 }} />
            <Button variant="ghost" onClick={() => setDialog(null)}>Cancel</Button>
            <Button variant="primary" style={{ background: "#ff8a8a" }} onClick={() => { setDialog(null); void cleanup(); }}>{removeWorktree ? "Delete chats + worktrees" : "Delete chats; keep worktrees"}</Button>
          </div>
        </Modal>
      )}
      {dialog === "forget" && (
        <ConfirmDialog title="Forget this race?" confirm="Forget" onClose={() => setDialog(null)} onConfirm={() => void forget()}
          body="The race record and its ranking are dropped. The attempt chats and their worktrees are left as they are." />
      )}
      {dialog && typeof dialog === "object" && (
        <ConfirmDialog title="Over the monthly budget" confirm="Judge anyway" onClose={() => setDialog(null)} onConfirm={() => void judge(true)}
          body={`${dialog.budget || "The monthly budget cap is reached."} Running the judge will spend more.`} />
      )}
    </div>
  );
}

function AttemptCard({ n, attempt, task, stats, rank, winner, onOpen, onPick, onFolder }: { n: number; attempt: RaceAttempt; task: TaskSummary | undefined; stats: RaceAttemptStats | undefined; rank: RaceRank | undefined; winner: boolean; onOpen: () => void; onPick: () => void; onFolder: () => void }) {
  const [shown, setShown] = useState<number | null>(null);
  const [showStats, setShowStats] = useState(false);
  const status = task ? chatStatus(task) : "deleted";
  const effort = effortName(attempt.model, attempt.effort);
  const first = rank?.rank === 1;
  return (
    <div className={"race-card" + (first ? " first" : "") + (winner ? " winner" : "")} data-testid={`race-attempt-${n}`}>
      <div className="race-card-head">
        <span className="race-card-n">Attempt {n}</span>
        {winner && <span className="race-badge win">Winner</span>}
        {rank && <span className={"race-badge" + (first ? " top" : "")}>#{rank.rank}{rank.score !== null ? ` · ${rank.score}` : ""}</span>}
      </div>
      <div className="race-card-model">{modelInfo(attempt.model).name}{effort ? <span className="race-dim"> · {effort}</span> : null}</div>
      <div className="race-facts">
        <span style={{ color: task ? statusOf(status, !!task.paused).dot : "var(--dim)" }}>{task ? (task.paused ? "paused" : status) : "deleted"}</span>
        {stats ? stats.error ? <span className="race-err" title={stats.error}>diff unavailable</span>
          : <span className="mono">{stats.files} file{stats.files === 1 ? "" : "s"} <span style={{ color: "var(--diff-add)" }}>+{stats.add}</span> <span style={{ color: "var(--diff-del)" }}>−{stats.del}</span></span>
          : <span className="race-dim">diff …</span>}
        {task && <span>{fmt$(task.usage.cost)}</span>}
        {task && <span>{fmtDuration(attemptElapsed(task))}</span>}
      </div>
      <div className="race-checks">
        {!attempt.checked ? <span className="race-dim">checks not run yet</span>
          : !attempt.checks.length ? <span className="race-dim" title="Nothing this attempt changed has an automatic check, or Diagnostics is off in Settings">checks off</span>
          : attempt.checks.map((c, i) => (
            <div key={i} className="race-check">
              <button type="button" className={"race-check-row " + (c.ok ? "ok" : "bad")} onClick={() => setShown(shown === i ? null : i)} aria-expanded={shown === i} title={c.cmd}>
                <span className="race-check-mark">{c.ok ? "✓" : "✗"}</span>
                <span className="race-check-label">{c.label}</span>
                <span className="race-dim">{c.ok ? "pass" : "fail"}</span>
              </button>
              {shown === i && <pre className="race-check-out">{c.output || "(no output)"}</pre>}
            </div>
          ))}
      </div>
      {showStats && task && <AttemptStats n={n} attempt={attempt} task={task} stats={stats} />}
      {rank?.reason && <div className="race-reason">{rank.reason}</div>}
      <div className="race-card-actions">
        <Button variant="ghost" onClick={onOpen} disabled={!task}>Open chat</Button>
        <Button variant="ghost" onClick={() => setShowStats((v) => !v)} disabled={!task} aria-expanded={showStats}>Stats</Button>
        <Button variant="ghost" onClick={onFolder} disabled={!task?.worktree && !task?.docker} title={task?.cwd ? `Open ${task.cwd} in the file manager` : "This attempt's chat was deleted"}>Open folder</Button>
        <Button variant={winner ? "default" : "ghost"} onClick={onPick} disabled={!task && !winner}>{winner ? "Unpick" : "Pick winner"}</Button>
      </div>
    </div>
  );
}

const num = (n: number) => n.toLocaleString("en-US");

/** The numbers behind one attempt: what the model read and wrote, how much the
 *  cache saved, what it cost per changed line, and how full its context got. */
function AttemptStats({ n, attempt, task, stats }: { n: number; attempt: RaceAttempt; task: TaskSummary; stats: RaceAttemptStats | undefined }) {
  const u = task.usage;
  const info = modelInfo(task.model || attempt.model);
  const effort = effortName(attempt.model, attempt.effort);
  const totalIn = u.input + u.cache_read + u.cache_write;
  const hit = totalIn > 0 ? Math.round((u.cache_read / totalIn) * 100) : 0;
  const ctx = info.context > 0 ? Math.round((u.last_context / info.context) * 100) : null;
  const lines = stats && !stats.error ? stats.add + stats.del : 0;
  const rows: [string, string][] = [
    ["Model", `${info.name}${effort ? ` · ${effort}` : ""}`],
    ["Input tokens", num(u.input)],
    ["Output tokens", num(u.output)],
    ["Cache read / write", `${num(u.cache_read)} / ${num(u.cache_write)}`],
    ["Cache hit", `${hit}%`],
    ["Context used", ctx === null ? num(u.last_context) : `${num(u.last_context)} of ${num(info.context)} · ${ctx}%`],
    ["Cost", fmt$(u.cost)],
    ["Cost per changed line", lines ? fmt$(u.cost / lines) : "—"],
    ["Time", fmtDuration(attemptElapsed(task))],
    ["Price per 1M in / out", `$${info.input_price} / $${info.output_price}`],
  ];
  return (
    <dl className="race-stats" data-testid={`race-stats-${n}`}>
      {rows.map(([k, v]) => <div key={k}><dt>{k}</dt><dd className="mono">{v}</dd></div>)}
    </dl>
  );
}
