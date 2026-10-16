import { open } from "@tauri-apps/plugin-dialog";
import { WorkspacePicker } from "./Workspaces";
import { ProjectName } from "./ProjectName";
import { ago, api, oneLine } from "../api";
import { useEffect, useMemo, useState } from "react";
import { chatModel, flash, go, isConnected, modelColor, openProject, set, useNow, useStore } from "../store";
import { Composer, openMenu } from "./Composer";
import { bySortKey } from "./Chrome";
import { I, StatusIcon, chatStatus, statusOf } from "./icons";
import { globalPauseHolds, heldByGlobal, isPaused, pauseHeadline } from "./Paused";
import { MorphText, Tooltip, Button, ChipButton, IconButton, Input, Pressable, TextButton } from "./primitives";

/** Prompts for a chat that has nothing in it yet.
 *
 *  They fill the composer rather than sending: the first screen should offer a
 *  starting point instead of an empty box, and the user should still get to
 *  edit what they are about to run. */
const STARTERS = [
  { label: "Explain this repository", prompt: "Explain what this project is, how it is laid out, and how to build and test it." },
  { label: "Find and fix a failing test", prompt: "Run the test suite and fix the first failure you find." },
  { label: "Review my uncommitted changes", prompt: "Review my uncommitted changes and tell me what is wrong with them." },
  { label: "Write project instructions", prompt: "/init" },
];

/** What has to be true before a first task can run.
 *
 *  Rendered only while something is missing, so it teaches once and then gets
 *  out of the way. Its reason for existing: the recurring complaint against
 *  tools in this field is discovering a missing prerequisite *halfway through a
 *  task*, and the fix is to say it before one starts. */
function FirstRun() {
  const project = useStore((s) => s.settings?.project ?? "");
  const info = useStore((s) => s.projectInfo);
  const providers = useStore((s) => s.providers);
  const modelReady = providers.some((p) => p.enabled && isConnected(p));
  const steps = [
    { id: "model", done: modelReady, label: "Connect a model provider", hint: "Add an API key, or sign in to a subscription", cta: "Open Models", run: () => go("settings", { settingsTab: "models" }) },
    { id: "folder", done: !!project, label: "Open the project you want to work in", hint: "Agents work on real files, so they need a folder", cta: "Open folder", run: () => void pickFolder() },
    // Only once there is a folder to decide about.
    { id: "trust", done: !!info?.trusted, label: "Trust this folder", hint: "Until you decide, its instructions, skills, agents, hooks and MCP servers stay off", cta: "Decide", run: () => set({ trustPrompt: true }) },
  ].filter((s) => s.id !== "trust" || !!project);
  if (steps.every((s) => s.done)) return null;
  return (
    <div className="firstrun">
      <div style={{ fontWeight: 600 }}>Before the first task</div>
      {steps.map((s) => (
        <div key={s.id} style={{ display: "flex", alignItems: "center", gap: 9, minHeight: 34 }}>
          <span aria-hidden="true" style={{ width: 12, color: s.done ? "var(--green)" : "var(--dim)" }}>{s.done ? "✓" : "•"}</span>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ color: s.done ? "var(--mut3)" : "var(--fg)" }}>{s.label}{s.done && <span className="sr-only"> — done</span>}</div>
            {!s.done && <div className="desc">{s.hint}</div>}
          </div>
          {!s.done && <Button variant="ghost" style={{ height: 24, flex: "none" }} onClick={s.run}>{s.cta}</Button>}
        </div>
      ))}
    </div>
  );
}

export async function pickFolder() {  const dir = await open({ directory: true, multiple: false, title: "Open project folder" });
  if (typeof dir !== "string") return;
  await openProject(dir);
}

export function Home() {
  useNow();
  const project = useStore((s) => s.settings?.project ?? "");
  const info = useStore((s) => s.projectInfo);
  const home = useStore((s) => s.home);
  const tasks = useStore((s) => s.tasks);
  const savedN = useStore((s) => s.settings?.saved_prompts?.length ?? 0);
  // "Recent" means the chats the user was last in, not the ones whose agents
  // happened to run most recently — same rule as the sidebar.
  //
  // Memoized, like the sidebar's copy of the same filter/sort and for the same
  // reason: `tasks` changes on every task event from every chat, so this screen
  // re-rendered — and re-sorted the whole chat list — each time a background
  // agent reported a step, for a list that only changes when a chat is touched.
  // The reorder setting is a subscription rather than something `sortKey` reads
  // off the live store: inside a memo that would not be a dependency.
  const automaticReorder = useStore((s) => s.settings?.automatic_chat_reorder !== false);
  const recent = useMemo(
    () => bySortKey(Object.values(tasks).filter((t) => t.project === project && !t.archived), automaticReorder).slice(0, 5),
    [tasks, project, automaticReorder],
  );
  const pausedAll = useStore((s) => s.settings?.paused_all ?? false);
  const [msg, setMsg] = useState("");
  const [quiet, setQuiet] = useState(false);
  // Set the instant "Pause all" is pressed, and cleared once the banner has shown
  // its loading state (see the effect below). The press is optimistic, so for a
  // moment the UI has the flag and no counts; this is what keeps that gap from
  // being rendered as a claim that the pause is already done.
  const [settlingPause, setSettlingPause] = useState(false);
  //
  // It doubles as the button's own acknowledgement. The backend now puts the
  // flag up first and walks the chats after it (see `pause_all_chats` in
  // `src-tauri/src/lib.rs`), so what the chip covers is the IPC round trip rather
  // than that walk — but it covers all of it: until the flag lands the chip is
  // still enabled and unchanged, so pressing it twice would send the whole thing
  // twice and the second press would overwrite the first press's failure handling.
  // How long the loading state has been showing. Off until a pause is pressed.
  const [floorPassed, setFloorPassed] = useState(false);
  // Whether the chip is mid-press. Separate from `settlingPause` because that one
  // is also set by a pause that landed on its own (a tray Quit freezes every chat),
  // and the button must not sit disabled claiming to be working on a press the user
  // never made. Kept true until the flag is back, so the acknowledgement is the
  // whole round trip rather than one frame.
  const [pausing, setPausing] = useState(false);
  // One pass, memoized. Every one of these walks the whole chat list, and
  // `tasks` changes on every task event from every chat, so the home screen paid
  // four full scans plus a sort each time any agent reported a step.
  const { live, pausedN, draining } = useMemo(() => {
    const all = Object.values(tasks);
    return {
      live: all.filter((t) => t.status === "running" || t.status === "waiting").length,
      pausedN: all.filter((t) => t.paused).length,
      // Paused but commands still finishing: the pause is in progress.
      draining: all.filter((t) => (t.paused || heldByGlobal(t, pausedAll)) && t.busy > 0).reduce((n, t) => n + t.busy, 0),
    };
  }, [tasks, pausedAll]);
  const all = useMemo(() => Object.values(tasks), [tasks]);
  // The banner is about a pause that is holding something, so it is gated on that
  // and not on the flag: `paused_all` survives a quit and outlives the chats it
  // froze, and a flag that holds nothing announced "Everything is paused" over a
  // paused list reading "Nothing is frozen right now" — the user got that with
  // nothing paused, and believed it.
  const heldAll = globalPauseHolds(all, pausedAll);
  // Every live background command, across every chat. A pause does not stop
  // these — a dev server or a watcher is meant to outlive one, which is why the
  // backend leaves them out of the `busy` count the headline is made of — so a
  // banner saying "Everything is paused" has to name them, or it reports nothing
  // running while a server is still up and rebuilding on every save.
  //
  // Read off the summaries rather than this window's own `bg` map: that map only
  // covers the chats this window has seen an event for, and a job that exits on
  // its own is not always re-emitted, so it can hold a dead one as `running`. The
  // backend recounts on every start and exit and publishes, so this list is the
  // same in every client.
  // A summary the store already holds (or a test fixture) can predate the field,
  // so a missing list has to read as nothing known rather than take the home
  // screen down with it.
  const liveJobs = useMemo(
    () => all.flatMap((t) => (t.bg_now ?? []).filter((b) => b.running).map((b) => ({ task: t, job: b })))
      // Oldest first: the server nobody remembers starting is the one worth
      // seeing at the top of a list you are scanning on your way out.
      .sort((a, b) => a.job.started.localeCompare(b.job.started)),
    [all],
  );
  // Whether any pause at all is showing, which is what the banner and its pill
  // are gated on. Same rule: an inert flag is not a pause.
  const anyPaused = heldAll || pausedN > 0;
  // "Pause all" only lands once the backend has recounted every chat's
  // in-flight commands, and the press is optimistic until that arrives. In the
  // gap the counts are 0, which the banner would otherwise read as "the pause is
  // done" — the one state that looks like the app has given up. A pause is never
  // shown as settled while the answer is still on its way.
  const settling = settlingPause && !floorPassed && (heldAll || pausedN > 0);
  // The press is optimistic and the backend's recount follows it, so this is the
  // window where the flag is in but the counts are not — the exact gap that used
  // to render as "Everything is paused". A floor rather than "clear when the flag
  // lands", because clearing it in the same commit as the flag arrives would make
  // it a one-frame flash, which is no better than the lie it replaces. A press
  // that fails clears `settlingPause` outright, so a refused pause cannot strand
  // the banner on "Loading…".
  useEffect(() => {
    if (!settlingPause) {
      setFloorPassed(false);
      return;
    }
    const t = setTimeout(() => setFloorPassed(true), 500);
    return () => clearTimeout(t);
  }, [settlingPause]);
  // The press is acknowledged until the flag comes back, which is the whole of the
  // backend's sweep. A press that fails clears it at the call site instead, so a
  // refused pause cannot leave a chip stuck mid-press with nothing behind it.
  useEffect(() => {
    if (pausedAll) setPausing(false);
  }, [pausedAll]);
  return (
    <>
      {I.logo()}
      <div className="home">
        <div style={{ flex: 1, minHeight: 40 }} />
        <div style={{ width: "100%", maxWidth: 660, display: "flex", flexDirection: "column", gap: 14 }}>
          <h1>
            {project ? <>What should we build in <TextButton className="project-link" onClick={(e) => openMenu("folder", e)}><ProjectName path={project} /></TextButton>?</> : <><TextButton className="project-link" onClick={pickFolder}>Open a project</TextButton> to get started</>}
          </h1>
          <FirstRun />
          {anyPaused && quiet && (
            <Tooltip content="Show the pause banner again"><Pressable className="pausepill" onClick={() => setQuiet(false)}>
              {I.pause(9)}<span>{settling ? "Pausing…" : heldAll ? "All paused" : `${pausedN} paused`}</span>
              <Pressable className="pp-list" onClick={(e) => { e.stopPropagation(); go("paused"); }}>List</Pressable>
              <Pressable className="pp-go" onClick={(e) => { e.stopPropagation(); api.resumeAll().catch((er) => flash(String(er))); }}>{I.play(8)}Resume</Pressable>
            </Pressable></Tooltip>
          )}
          {anyPaused && !quiet && (
            <div className="pausebar big">
              <span className="pz">{I.pause(12)}</span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontWeight: 600, color: "#fde68a" }}>{pauseHeadline({ settling, draining, pausedAll: heldAll, pausedN })}</div>
                <Input style={{ width: "100%", marginTop: 6, height: 28, background: "rgba(0,0,0,0.25)" }} placeholder="Optional message to every agent when they resume" value={msg} onChange={(e) => setMsg(e.currentTarget.value)} />
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 4, alignItems: "stretch" }}>
                <Button style={{ background: "#fbbf24", color: "#1b1405", border: 0, fontWeight: 600 }} onClick={() => api.resumeAll(msg).then(() => { setMsg(""); flash("Resumed everything"); }).catch((e) => flash(String(e)))}>{I.play(10)}Resume all</Button>
                <Button variant="ghost" style={{ height: 22, fontSize: 11, justifyContent: "center" }} title="Open the list of paused chats" onClick={() => { setQuiet(true); go("paused"); }}>View all</Button>
                {draining > 0 && <Button variant="ghost" style={{ height: 22, fontSize: 11, justifyContent: "center" }} title="Kill running commands now; agents are told to rerun them after resuming" onClick={() => api.forcePauseAll().catch((e) => flash(String(e)))}>Force pause</Button>}
                {/* Opening the list also shrinks the banner to its small pill,
                    which carries List and Resume buttons of its own. */}
              </div>
              {/* Last in the bar and full width, so the jobs read as part of the
                  pause rather than as one more of its actions. */}
              {liveJobs.length > 0 && (
                <div className="pb-jobs">
                  <div className="pb-jobs-h">
                    <span style={{ fontWeight: 600 }}>Still running</span>
                    <span>{liveJobs.length} background command{liveJobs.length === 1 ? "" : "s"} · not stopped by the pause</span>
                  </div>
                  {liveJobs.map(({ task, job }) => (
                    <div key={`${task.id}/${job.id}`} className="pb-job" role="button" tabIndex={0} onClick={() => go("session", { task: task.id })}
                      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); go("session", { task: task.id }); } }}>
                      <StatusIcon status="running" size={11} />
                      <span className="mono pb-job-cmd" title={job.cmd}>{job.cmd}</span>
                      <span className="pb-job-chat" title={task.title}>{oneLine(task.title, 28)}</span>
                      <span className="pb-job-up" title={`Started ${new Date(job.started).toLocaleString()}`}>up {ago(job.started)}</span>
                      <IconButton label={`Stop ${job.cmd}`} style={{ width: 20, height: 20 }} onClick={(e) => { e.stopPropagation(); void api.bgKill(task.id, job.id).catch((err) => flash(String(err))); }}><StatusIcon status="stopped" size={11} /></IconButton>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
          <Composer mode="home" />
          <div style={{ display: "flex", alignItems: "center", gap: 2, padding: "0 6px", flexWrap: "wrap" }}>
            <Tooltip content={info?.memory.length ? `Instructions loaded into every task: ${info.memory.join(", ")}` : undefined}><ChipButton onClick={(e) => (project ? openMenu("folder", e) : pickFolder())}>{I.folder()}{project ? <ProjectName path={project} /> : "Open folder…"}{!!info?.memory.length && <span className="membadge">{info.memory.length} doc{info.memory.length === 1 ? "" : "s"}</span>}</ChipButton></Tooltip>
            {project && <WorkspacePicker project={project} git={!!info?.git} />}
            {info?.git && <ChipButton className="mono home-branch" style={{ fontSize: 11.5 }} onClick={(e) => openMenu("branch", e)}>{I.branch()}<span className="home-branch-name">{home.branch || info.branch}</span></ChipButton>}
            <div style={{ flex: 1 }} />
            {!pausedAll && live > pausedN && (
              <Tooltip content={pausing ? "Pausing every agent…" : "Freeze every running agent (e.g. before shutting down)"}>
                <ChipButton
                  // Disabled for the whole round trip, not just the click: without
                  // it a second press re-runs the sweep and the button gave no sign
                  // the first one had been heard, which is the whole complaint.
                  disabled={pausing}
                  // Stable across the press, so the button keeps its identity while
                  // its label changes: assistive tech reads the same thing twice,
                  // and it gives the render tests something to hold on to.
                  aria-label="Pause all agents"
                  onClick={() => {
                    setPausing(true);
                    setSettlingPause(true);
                    api.pauseAll().catch((e) => { setSettlingPause(false); setPausing(false); flash(String(e)); });
                  }}
                >
                  {I.pause(10)}{pausing ? "Pausing…" : "Pause all"}
                </ChipButton>
              </Tooltip>
            )}
          </div>
        </div>
        <div style={{ flex: 1, minHeight: 40 }} />
        {savedN > 0 && (
          <div style={{ width: "100%", maxWidth: 660, display: "flex", alignItems: "center", justifyContent: "flex-end", padding: "0 10px 6px", fontSize: 11, color: "var(--dim)" }}>
            <TextButton style={{ color: "var(--mut2)" }} onClick={() => go("saved")}>All saved prompts</TextButton>
          </div>
        )}
        <div style={{ width: "100%", maxWidth: 660, display: "flex", flexDirection: "column", gap: 1, paddingBottom: 28 }}>
          {recent.length > 0 && (
            <div style={{ display: "flex", alignItems: "center", padding: savedN > 0 ? "10px 10px 6px" : "0 10px 6px", fontSize: 11, color: "var(--dim)" }}>
              <span style={{ fontWeight: 500 }}>Jump back in</span>
            </div>
          )}
          {!recent.length && (
            <>
              <div style={{ display: "flex", alignItems: "center", padding: savedN > 0 ? "10px 10px 6px" : "0 10px 6px", fontSize: 11, color: "var(--dim)" }}>
                <span style={{ fontWeight: 500 }}>Try one of these</span>
              </div>
              {STARTERS.map((s) => (
                <Pressable key={s.label} className="jrow" onClick={() => set({ draft: s.prompt })}>
                  <span style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", fontWeight: 500, color: "#d4d4d8" }}>{s.label}</span>
                  <span style={{ fontSize: 11, color: "var(--dim)" }}>Fill the box</span>
                </Pressable>
              ))}
            </>
          )}
          {recent.map((t) => {
            // `st` and the icon read the same question: is this chat frozen? They
            // used to answer it two ways, so under "Pause all" the row said
            // "Paused" while the icon beside it was still the live running one.
            const paused = isPaused(t, pausedAll);
            // Same verdict the sidebar draws: a chat parked on a trigger is not
            // waiting on the user, so it must not arrive here red either.
            const shown = chatStatus(t);
            const st = statusOf(shown, paused);
            return (
              <Pressable key={t.id} className="jrow" onClick={() => go("session", { task: t.id })}>
                <StatusIcon status={paused ? "paused" : shown} color={modelColor(chatModel(t))} />
                <span style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", fontWeight: 500, color: "#d4d4d8" }}>{t.title}</span>
                <span className="mono" style={{ fontSize: 11, color: "var(--dim)" }}>{t.branch}</span>
                <MorphText className="task-state">{shown === "done" || shown === "idle" ? ago(t.updated_at) : st.text}</MorphText>
              </Pressable>
            );
          })}
        </div>
      </div>
    </>
  );
}

