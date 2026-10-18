// Schedules: recurring agent runs. Each fire starts an ordinary chat titled
// "⏰ <name>" — the backend owns the clock and the list
// (src-tauri, `~/.openleash/schedules.json`); this screen only edits it and
// shows what the backend last published on `ol://schedules`.
import { useEffect, useMemo, useState } from "react";
import {
  ago, api, baseName, clampEffort, EFFORTS, effortLabel, effortSteps, normPath, PERMS, permId,
  type Perm, type Schedule, type ScheduleDraft, type ScheduleRun, type ScheduleTiming, type Settings, type Weekday,
} from "../api";
import { defaultMode } from "../workspaces";
import { flash, go, isConnected, loadSchedules, modelInfo, receiveSchedules, shownProviders, useNow, useStore } from "../store";
import { isScheduledChat } from "./Chrome";
import { I } from "./icons";
import { Button, ChoiceChip, ConfirmDialog, Dropdown, Input, Modal, Segmented, Switch, TextArea, TextButton, type Opt } from "./primitives";

export const WEEKDAYS: readonly [Weekday, string][] = [["mon", "Mon"], ["tue", "Tue"], ["wed", "Wed"], ["thu", "Thu"], ["fri", "Fri"], ["sat", "Sat"], ["sun", "Sun"]];
const DAY_NAME = new Map<Weekday, string>(WEEKDAYS);

/** A schedule's timing in words: "Every 30 minutes", "Daily at 09:00", "Mon, Fri at 18:30". */
export function describeTiming(t: ScheduleTiming): string {
  if (t.kind === "interval") {
    const n = Math.max(1, Math.round(t.every));
    const unit = t.unit === "hours" ? "hour" : "minute";
    return n === 1 ? `Every ${unit}` : `Every ${n} ${unit}s`;
  }
  if (t.kind === "daily") return `Daily at ${t.time}`;
  // Ordered Mon → Sun however they were ticked, and duplicates dropped.
  const days = WEEKDAYS.map(([d]) => d).filter((d) => t.days.includes(d));
  if (!days.length) return `Weekly at ${t.time} · no days picked`;
  if (days.length === 7) return `Daily at ${t.time}`;
  if (days.join() === "mon,tue,wed,thu,fri") return `Weekdays at ${t.time}`;
  if (days.join() === "sat,sun") return `Weekends at ${t.time}`;
  return `${days.map((d) => DAY_NAME.get(d) ?? d).join(", ")} at ${t.time}`;
}

/** A backend timestamp in local time, short: "Fri 17 Oct, 09:00". */
export function fmtWhen(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString([], { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}

const agoText = (iso: string) => { const a = ago(iso); return a === "now" ? "just now" : `${a} ago`; };

/** What a new schedule starts from: the user's own defaults, so nothing about a
 *  schedule is more permissive than the new chat they would have started by hand. */
export function newScheduleDraft(settings: Settings | null, homeModel: string): ScheduleDraft {
  return {
    name: "", enabled: true, prompt: "", project: settings?.project ?? "",
    model: homeModel || settings?.recent_models?.[0] || settings?.model || "",
    effort: settings?.effort ?? 2,
    // The default permission mode is the one new chats use (`settings.perm`),
    // shown the way the permission picker shows it (reviewer stays reviewer).
    perm: permId(settings?.perm),
    plan: false,
    workspace: defaultMode(settings) === "worktree" ? "worktree" : "local",
    base_branch: "",
    timing: { kind: "daily", time: "09:00" },
    catch_up: true,
  };
}

/** The editable part of a saved schedule — its own values, not the defaults. */
export function toDraft(s: Schedule): ScheduleDraft {
  return {
    id: s.id, name: s.name, enabled: s.enabled, prompt: s.prompt, project: s.project, model: s.model, effort: s.effort,
    perm: s.perm, plan: s.plan, workspace: s.workspace, base_branch: s.base_branch, timing: s.timing, catch_up: s.catch_up,
  };
}

const STATUS: Record<ScheduleRun["status"], { label: string; color: string }> = {
  started: { label: "Started", color: "var(--st-done)" },
  skipped: { label: "Skipped", color: "var(--st-wait)" },
  failed: { label: "Failed", color: "var(--st-fail)" },
};

function PermPill({ perm }: { perm: Perm }) {
  const p = PERMS.find((x) => x.id === permId(perm));
  if (!p) return null;
  return <span className="badge quiet" style={{ color: p.color, background: "transparent", border: `1px solid ${p.color}55` }}>{p.name}</span>;
}

/** Opens the chat a run started, when that chat is still around. */
function ChatLink({ id }: { id: string | null }) {
  const exists = useStore((s) => !!id && !!s.tasks[id]);
  if (!id || !exists) return null;
  return <TextButton link onClick={() => go("session", { task: id })}>Open chat</TextButton>;
}

function RunLine({ run }: { run: ScheduleRun }) {
  const st = STATUS[run.status];
  return <div className="desc" style={{ display: "flex", gap: 8, alignItems: "baseline", flexWrap: "wrap" }}>
    <span style={{ fontVariantNumeric: "tabular-nums" }}>{fmtWhen(run.at)}</span>
    <span style={{ color: st.color }}>{st.label}</span>
    {run.message && <span style={{ minWidth: 0 }}>{run.message}</span>}
    <ChatLink id={run.task_id} />
  </div>;
}

function Row({ s, paused, onEdit, onDelete }: { s: Schedule; paused: boolean; onEdit: () => void; onDelete: () => void }) {
  const [hist, setHist] = useState(false);
  const [busy, setBusy] = useState(false);
  const toggle = (enabled: boolean) => {
    void api.scheduleSetEnabled(s.id, enabled).then(() => loadSchedules()).catch((e) => flash(String(e)));
  };
  const runNow = async () => {
    setBusy(true);
    try {
      const next = await api.scheduleRunNow(s.id);
      void loadSchedules();
      const task = next.last_task_id;
      if (next.last_status === "started") flash(`Started ⏰ ${next.name}`, task ? { label: "Open", run: () => go("session", { task }) } : undefined);
      else flash(next.last_message || (next.last_status === "skipped" ? "Skipped" : "Didn't start"));
    } catch (e) {
      flash(String(e));
    } finally { setBusy(false); }
  };
  const last = s.last_status ? STATUS[s.last_status] : null;
  return <div className="sgroup" data-schedule={s.id}>
    <div className="srowx" style={{ alignItems: "flex-start" }}>
      <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 3 }}>
        <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
          <span style={{ fontWeight: 550 }}>{s.name}</span>
          <PermPill perm={s.perm} />
          {s.plan && <span className="badge quiet">Plan</span>}
          {s.workspace === "worktree" && <span className="badge quiet">Worktree</span>}
        </div>
        <div className="desc">
          {describeTiming(s.timing)} · <span title={s.project}>{s.project ? baseName(s.project) : "No folder"}</span>
        </div>
        <div className="desc">
          {!s.enabled ? "Off" : paused ? "Paused · all schedules are paused" : s.next_run_at ? <>Next run {fmtWhen(s.next_run_at)}</> : "Not scheduled"}
        </div>
        {last && s.last_run_at && <div className="desc" style={{ display: "flex", gap: 6, alignItems: "baseline", flexWrap: "wrap" }}>
          <span>Last run {agoText(s.last_run_at)}:</span>
          <span style={{ color: last.color }}>{last.label}</span>
          {s.last_message && <span>{s.last_message}</span>}
          <ChatLink id={s.last_task_id} />
        </div>}
      </div>
      <div style={{ display: "flex", gap: 6, alignItems: "center", flex: "none" }}>
        <Button disabled={busy} onClick={() => void runNow()} aria-label={`Run ${s.name} now`}>{busy ? "Starting…" : "Run now"}</Button>
        <Button variant="ghost" onClick={onEdit} aria-label={`Edit ${s.name}`}>Edit</Button>
        <Button variant="ghost" onClick={onDelete} aria-label={`Delete ${s.name}`}>{I.trash(12)}</Button>
        <Switch label={`Enable ${s.name}`} checked={s.enabled} onChange={toggle} />
      </div>
    </div>
    {s.history.length > 0 && <div className="srowx" style={{ display: "block", minHeight: 0 }}>
      <TextButton aria-expanded={hist} onClick={() => setHist(!hist)}>{hist ? "▾" : "▸"} History ({s.history.length})</TextButton>
      {hist && <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: 6 }}>
        {[...s.history].reverse().map((r, i) => <RunLine key={r.at + i} run={r} />)}
      </div>}
    </div>}
  </div>;
}

const field = { display: "flex", flexDirection: "column", gap: 6 } as const;
const label = { fontWeight: 500, fontSize: 12.5 } as const;

/** Create / edit dialog. Validation is the backend's; its message is shown as-is. */
export function ScheduleForm({ initial, onClose }: { initial: ScheduleDraft; onClose: () => void }) {
  const settings = useStore((s) => s.settings);
  const models = useStore((s) => s.models);
  const provs = useStore((s) => s.providers);
  const [d, setD] = useState<ScheduleDraft>(initial);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  const patch = (p: Partial<ScheduleDraft>) => setD((x) => ({ ...x, ...p }));
  const timing = d.timing;
  const time = timing.kind === "interval" ? "09:00" : timing.time;

  const projectOpts = useMemo(() => {
    const out: Opt[] = [];
    const seen = new Set<string>();
    for (const p of [d.project, settings?.project ?? "", ...(settings?.projects ?? [])]) {
      if (!p || seen.has(normPath(p))) continue;
      seen.add(normPath(p));
      out.push({ value: p, label: baseName(p), hint: p });
    }
    return out;
  }, [d.project, settings?.project, settings?.projects]);

  // The connected models, plus the one this schedule already names so a model
  // since disabled doesn't silently vanish from its own form.
  const modelOpts = useMemo(() => {
    const on = new Set(shownProviders(provs).filter((p) => p.enabled && isConnected(p)).map((p) => p.id));
    const all = models.filter((m) => on.has(m.provider) && m.enabled !== false);
    if (d.model && !all.some((m) => m.id === d.model)) all.unshift(modelInfo(d.model));
    return all.map((m) => ({ value: m.id, label: m.name || m.id, group: provs.find((p) => p.id === m.provider)?.name ?? m.provider }));
  }, [models, provs, d.model]);

  const mi = modelInfo(d.model);
  const steps = effortSteps(mi);
  const effortOpts = steps.map((v) => ({ value: String(v), label: effortLabel(mi, v) ?? EFFORTS[v] ?? String(v) }));
  const perm = PERMS.find((p) => p.id === permId(d.perm));

  const save = async () => {
    if (!d.name.trim()) { setErr("Give the schedule a name."); return; }
    if (!d.prompt.trim()) { setErr("Write the prompt the schedule should send."); return; }
    if (!d.project) { setErr("Pick the project folder it runs in."); return; }
    setBusy(true); setErr("");
    try {
      await api.scheduleSave({ ...d, name: d.name.trim() });
      void loadSchedules();
      flash(d.id ? "Schedule saved" : "Schedule created");
      onClose();
    } catch (e) {
      setErr(String(e));
    } finally { setBusy(false); }
  };

  const setKind = (kind: ScheduleTiming["kind"]) => {
    if (kind === timing.kind) return;
    if (kind === "interval") patch({ timing: { kind, every: 60, unit: "minutes" } });
    else if (kind === "daily") patch({ timing: { kind, time } });
    else patch({ timing: { kind, days: ["mon", "tue", "wed", "thu", "fri"], time } });
  };
  const setTime = (t: string) => {
    if (!t) return;
    if (timing.kind === "daily") patch({ timing: { ...timing, time: t } });
    else if (timing.kind === "weekly") patch({ timing: { ...timing, time: t } });
  };

  return <Modal onClose={onClose} ariaLabel={d.id ? "Edit schedule" : "New schedule"} style={{ width: "min(620px, 100%)" }}>
    <div className="mh"><span style={{ flex: 1 }}>{d.id ? "Edit schedule" : "New schedule"}</span></div>
    <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 14, maxHeight: "70vh", overflowY: "auto" }}>
      <div style={field}><span style={label}>Name</span>
        <Input aria-label="Schedule name" value={d.name} placeholder="Nightly dependency check" onChange={(e) => patch({ name: e.currentTarget.value })} /></div>
      <div style={field}><span style={label}>Prompt</span>
        <TextArea aria-label="Schedule prompt" rows={4} value={d.prompt} placeholder="What the agent should do each time" onChange={(e) => patch({ prompt: e.currentTarget.value })} style={{ resize: "vertical", minHeight: 80 }} /></div>
      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12 }}>
        <div style={field}><span style={label}>Project folder</span>
          <Dropdown placeholder="Project folder" value={d.project} options={projectOpts} onChange={(project) => patch({ project })} /></div>
        <div style={field}><span style={label}>Model</span>
          <Dropdown placeholder="Model" value={d.model} options={modelOpts} onChange={(model) => patch({ model, effort: clampEffort(effortSteps(modelInfo(model)), d.effort) })} /></div>
        {effortOpts.length > 0 && <div style={field}><span style={label}>Effort</span>
          <Dropdown placeholder="Effort" search={false} value={String(clampEffort(steps, d.effort))} options={effortOpts} onChange={(v) => patch({ effort: Number(v) })} /></div>}
        <div style={field}><span style={label}>Workspace</span>
          <Segmented label="Workspace" value={d.workspace} onChange={(workspace) => patch({ workspace })} style={{ width: "100%" }}
            options={[{ value: "local", label: "Local", hint: "Work directly in the project folder" }, { value: "worktree", label: "Worktree", hint: "Isolated git worktree on its own branch" }]} /></div>
      </div>
      <div style={field}>
        <span style={label}>Permission mode</span>
        <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
          <Dropdown placeholder="Permission mode" search={false} value={permId(d.perm)} options={PERMS.map((p) => ({ value: p.id, label: p.name }))} onChange={(v) => patch({ perm: permId(v) })} style={{ width: 180, flex: "0 0 180px" }} />
          <span className="desc" style={{ color: "var(--hint)", fontSize: 11.5 }}>{perm?.desc}</span>
        </div>
        <div className="settings-note" role="note">Runs unattended with this permission mode. Approvals wait in Needs you — nothing is auto-approved.</div>
        {permId(d.perm) === "turbo" && d.workspace === "local" && <div className="settings-note" role="note" style={{ color: "var(--red)", display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
          Full Access runs anything without asking, with nobody watching. A worktree keeps its changes on their own branch.
          <TextButton link onClick={() => patch({ workspace: "worktree" })}>Use Worktree</TextButton>
        </div>}
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <span style={{ ...label, flex: 1 }}>Plan mode<div className="desc" style={{ fontWeight: 400, color: "var(--hint)", fontSize: 11.5 }}>Each run plans first and waits for your approval in Needs you.</div></span>
        <Switch label="Plan mode" checked={d.plan} onChange={(plan) => patch({ plan })} />
      </div>
      <div style={field}>
        <span style={label}>When</span>
        <Segmented label="Repeat" value={timing.kind} onChange={setKind}
          options={[{ value: "interval", label: "Every…" }, { value: "daily", label: "Daily" }, { value: "weekly", label: "Weekly" }]} />
        {timing.kind === "interval" && <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <span className="desc">Every</span>
          <Input aria-label="Every" type="number" min={1} value={String(timing.every)} style={{ width: 80 }}
            onChange={(e) => { const n = Math.floor(Number(e.currentTarget.value)); if (Number.isFinite(n)) patch({ timing: { ...timing, every: Math.max(1, n) } }); }} />
          <Segmented label="Unit" value={timing.unit} onChange={(unit) => patch({ timing: { ...timing, unit } })}
            options={[{ value: "minutes", label: "Minutes" }, { value: "hours", label: "Hours" }]} />
        </div>}
        {timing.kind === "weekly" && <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          {WEEKDAYS.map(([day, name]) => <ChoiceChip key={day} selected={timing.days.includes(day)}
            onClick={() => patch({ timing: { ...timing, days: timing.days.includes(day) ? timing.days.filter((x) => x !== day) : WEEKDAYS.map(([w]) => w).filter((w) => w === day || timing.days.includes(w)) } })}>{name}</ChoiceChip>)}
        </div>}
        {timing.kind !== "interval" && <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <span className="desc">At</span>
          <Input aria-label="Time" type="time" value={time} style={{ width: 120 }} onChange={(e) => setTime(e.currentTarget.value)} />
          <span className="desc" style={{ color: "var(--hint)" }}>local time</span>
        </div>}
        <div className="desc" style={{ color: "var(--hint)", fontSize: 11.5 }}>{describeTiming(timing)}</div>
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <span style={{ ...label, flex: 1 }}>Catch up once if missed while closed<div className="desc" style={{ fontWeight: 400, color: "var(--hint)", fontSize: 11.5 }}>If the app was closed at run time, run once when it opens — never more than once, however many were missed.</div></span>
        <Switch label="Catch up once if missed while closed" checked={d.catch_up} onChange={(catch_up) => patch({ catch_up })} />
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <span style={{ ...label, flex: 1 }}>Enabled</span>
        <Switch label="Enabled" checked={d.enabled} onChange={(enabled) => patch({ enabled })} />
      </div>
      {err && <div role="alert" className="settings-note" style={{ color: "var(--red)" }}>{err}</div>}
    </div>
    <div className="mf">
      <div style={{ flex: 1 }} />
      <Button variant="ghost" onClick={onClose}>Cancel</Button>
      <Button variant="primary" disabled={busy} onClick={() => void save()}>{busy ? "Saving…" : d.id ? "Save schedule" : "Create schedule"}</Button>
    </div>
  </Modal>;
}

/** The Schedules screen. Shown from the sidebar when Settings → Tabs →
 *  "Show Schedules" is on; hiding the tab never pauses anything. */
export function Schedules() {
  const show = useStore((s) => s.settings?.show_schedules ?? false);
  const state = useStore((s) => s.schedules);
  const settings = useStore((s) => s.settings);
  const homeModel = useStore((s) => s.home.model);
  const tasks = useStore((s) => s.tasks);
  const scheduledTasks = useMemo(() => Object.values(tasks).filter((task) => !task.archived && isScheduledChat(task)), [tasks]);
  const [editing, setEditing] = useState<ScheduleDraft | null>(null);
  const [deleting, setDeleting] = useState<Schedule | null>(null);
  useNow();
  useEffect(() => { if (show) void loadSchedules(); }, [show]);

  if (!show) {
    // The tab was switched off while this screen was open (or reached by Back).
    return <div style={{ flex: 1, overflow: "auto", padding: "24px 28px" }}>
      <div style={{ maxWidth: 860, margin: "0 auto", display: "flex", flexDirection: "column", gap: 12 }}>
        <div className="empty">The Schedules tab is hidden. Schedules that are enabled keep running.</div>
        <div style={{ display: "flex", gap: 8 }}>
          <Button onClick={() => go("settings", { settingsTab: "tabs" })}>Open Settings</Button>
          <Button variant="ghost" onClick={() => go("home")}>New task</Button>
        </div>
      </div>
    </div>;
  }

  const list = state?.schedules ?? [];
  const paused = state?.paused ?? false;
  const setPaused = (p: boolean) => {
    void api.schedulesSetPaused(p).then(receiveSchedules).catch((e) => flash(String(e)));
  };
  const remove = (s: Schedule) => {
    void api.scheduleDelete(s.id).then(() => { void loadSchedules(); flash("Schedule deleted"); }).catch((e) => flash(String(e)));
  };

  return <div style={{ flex: 1, overflow: "auto", padding: "24px 28px" }}>
    <div style={{ maxWidth: 860, margin: "0 auto", display: "flex", flexDirection: "column", gap: 14 }}>
      <div style={{ display: "flex", alignItems: "flex-end", gap: 24, flexWrap: "wrap" }}>
        <div style={{ flex: 1, minWidth: 200 }}>
          <div style={{ fontSize: 20, fontWeight: 600, letterSpacing: "-0.025em" }}>Schedules</div>
          <div className="secondary-text" style={{ marginTop: 3 }}>Recurring agent runs. Each run starts a separate scheduled chat, listed below.</div>
        </div>
        <Button variant="primary" onClick={() => setEditing(newScheduleDraft(settings, homeModel))}>New schedule</Button>
      </div>
      {list.length > 0 && <div className="sgroup"><div className="srowx">
        <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Pause all schedules</div><div className="desc">Nothing fires while paused. Each schedule keeps its own on/off switch.</div></div>
        <Switch label="Pause all schedules" checked={paused} onChange={setPaused} />
      </div></div>}
      {state === null && <div className="empty">Loading…</div>}
      {state !== null && !list.length && <div className="empty" style={{ lineHeight: 1.6 }}>
        No schedules yet. A schedule sends a prompt to a new chat on a timer — every N minutes or hours, daily, or on chosen weekdays — in the folder, model and permission mode you pick.
        If the previous run is still going, that run is skipped. A run missed while the app was closed can catch up once when it opens.
        A folder you have not answered the trust prompt for is refused until you open it and decide.
      </div>}
      {list.map((s) => <Row key={s.id} s={s} paused={paused} onEdit={() => setEditing(toDraft(s))} onDelete={() => setDeleting(s)} />)}
      <div className="settings-section-label" style={{ marginTop: 10 }}>Scheduled chats <span className="desc">({scheduledTasks.length}/100)</span></div>
      {scheduledTasks.length === 0
        ? <div className="empty">Scheduled runs will appear here when they start.</div>
        : scheduledTasks.map((task) => <div className="sgroup" key={task.id}><div className="srowx">
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>{task.title}</div><div className="desc">{task.project}</div></div>
          <Button onClick={() => go("session", { task: task.id })}>Open chat</Button>
        </div></div>)}
    </div>
    {editing && <ScheduleForm initial={editing} onClose={() => setEditing(null)} />}
    {deleting && <ConfirmDialog title={`Delete “${deleting.name}”?`} body="It stops running. Chats it already started are kept." confirm="Delete" danger
      onConfirm={() => remove(deleting)} onClose={() => setDeleting(null)} />}
  </div>;
}
