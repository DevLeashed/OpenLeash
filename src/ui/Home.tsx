import { open } from "@tauri-apps/plugin-dialog";
import { ago, api, baseName } from "../api";
import { useEffect, useState } from "react";
import { chatModel, flash, go, modelColor, openProject, set, useNow, useStore } from "../store";
import { Composer, openMenu } from "./Composer";
import { sortKey } from "./Chrome";
import { I, StatusIcon, statusOf } from "./icons";
import { globalPauseHolds, heldByGlobal, isPaused, pauseHeadline } from "./Paused";
import { MorphText, Tooltip, Button, ChipButton, Input, Pressable, TextButton } from "./primitives";

export async function pickFolder() {
  const dir = await open({ directory: true, multiple: false, title: "Open project folder" });
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
  const recent = Object.values(tasks).filter((t) => t.project === project && !t.archived).sort((a, b) => sortKey(b) - sortKey(a)).slice(0, 5);
  const wtOn = home.worktree && info?.git;
  const pausedAll = useStore((s) => s.settings?.paused_all ?? false);
  const [msg, setMsg] = useState("");
  const [quiet, setQuiet] = useState(false);
  // Set the instant "Pause all" is pressed, and cleared once the banner has shown
  // its loading state (see the effect below). The press is optimistic, so for a
  // moment the UI has the flag and no counts; this is what keeps that gap from
  // being rendered as a claim that the pause is already done.
  const [settlingPause, setSettlingPause] = useState(false);
  //
  // It doubles as the button's own acknowledgement. `pause_all` does not flip the
  // flag until it has walked every chat, reset `unpaused`, saved each to disk and
  // recounted what was in flight — which is seconds on a machine with a few dozen
  // chats, all of it after the IPC round trip. Until then the chip is still enabled
  // and unchanged, so pressing it twice sends the whole sweep twice and the second
  // press overwrites the first press's failure handling.
  // How long the loading state has been showing. Off until a pause is pressed.
  const [floorPassed, setFloorPassed] = useState(false);
  // Whether the chip is mid-press. Separate from `settlingPause` because that one
  // is also set by a pause that landed on its own (a tray Quit freezes every chat),
  // and the button must not sit disabled claiming to be working on a press the user
  // never made. Kept true until the flag is back, so the acknowledgement is the
  // whole round trip rather than one frame.
  const [pausing, setPausing] = useState(false);
  const all = Object.values(tasks);
  const live = all.filter((t) => t.status === "running" || t.status === "waiting").length;
  const pausedN = all.filter((t) => t.paused).length;
  // Paused but commands still finishing: the pause is in progress.
  const draining = all.filter((t) => (t.paused || heldByGlobal(t, pausedAll)) && t.busy > 0).reduce((n, t) => n + t.busy, 0);
  // The banner is about a pause that is holding something, so it is gated on that
  // and not on the flag: `paused_all` survives a quit and outlives the chats it
  // froze, and a flag that holds nothing announced "Everything is paused" over a
  // paused list reading "Nothing is frozen right now" — the user got that with
  // nothing paused, and believed it.
  const heldAll = globalPauseHolds(all, pausedAll);
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
            {project ? <>What should we build in <TextButton className="project-link" onClick={(e) => openMenu("folder", e)}>{baseName(project)}</TextButton>?</> : <><TextButton className="project-link" onClick={pickFolder}>Open a project</TextButton> to get started</>}
          </h1>
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
            </div>
          )}
          <Composer mode="home" />
          <div style={{ display: "flex", alignItems: "center", gap: 2, padding: "0 6px", flexWrap: "wrap" }}>
            <Tooltip content={info?.memory.length ? `Instructions loaded into every task: ${info.memory.join(", ")}` : undefined}><ChipButton onClick={(e) => (project ? openMenu("folder", e) : pickFolder())}>{I.folder()}{project ? baseName(project) : "Open folder…"}{!!info?.memory.length && <span className="membadge">{info.memory.length} doc{info.memory.length === 1 ? "" : "s"}</span>}</ChipButton></Tooltip>
            {info?.git && (
              <Tooltip content="Run in an isolated git worktree on its own branch"><ChipButton style={{ color: wtOn ? "var(--violet)" : undefined }} onClick={() => set((s) => ({ home: { ...s.home, worktree: !s.home.worktree } }))}>
                {I.monitor()}{wtOn ? "New worktree" : "Local checkout"}
              </ChipButton></Tooltip>
            )}
            {info?.git && <ChipButton className="mono" style={{ fontSize: 11.5 }} onClick={(e) => openMenu("branch", e)}>{I.branch()}{home.branch || info.branch}</ChipButton>}
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
          {recent.map((t) => {
            // `st` and the icon read the same question: is this chat frozen? They
            // used to answer it two ways, so under "Pause all" the row said
            // "Paused" while the icon beside it was still the live running one.
            const paused = isPaused(t, pausedAll);
            const st = statusOf(t.status, paused);
            return (
              <Pressable key={t.id} className="jrow" onClick={() => go("session", { task: t.id })}>
                <StatusIcon status={paused ? "paused" : t.status} color={modelColor(chatModel(t))} />
                <span style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", fontWeight: 500, color: "#d4d4d8" }}>{t.title}</span>
                <span className="mono" style={{ fontSize: 11, color: "var(--dim)" }}>{t.branch}</span>
                <MorphText className="task-state">{t.status === "done" || t.status === "idle" ? ago(t.updated_at) : st.text}</MorphText>
              </Pressable>
            );
          })}
        </div>
      </div>
    </>
  );
}

