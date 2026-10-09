import { useEffect, useState } from "react";
import { ago, api, baseName, oneLine, type TaskSummary } from "../api";
import { chatModel, flash, go, modelColor, useStore } from "../store";
import { sortKey } from "./Chrome";
import { I, StatusIcon } from "./icons";
import { Button, Input, Pressable, Tooltip } from "./primitives";

/**
 * A chat is frozen when it carries its own pause, or when the global pause is on
 * and this chat hasn't opted out of it. A global pause only freezes chats that
 * were actually working, so a finished or idle chat under it isn't "paused".
 *
 * The backend has the same rule in `Harness::held_by_global` — these two have to
 * agree, or a chat the UI calls idle sits parked in the backend with nothing on
 * screen saying so.
 *
 * The sidebar and the session header work this out inline; this is the one
 * place the list needs it as a value, and the tests need it on its own.
 */
export function isPaused(t: TaskSummary, pausedAll: boolean): boolean {
  return !!t.paused || heldByGlobal(t, pausedAll);
}

/** The global pause's half of {@link isPaused}, for callers that also test `t.paused`. */
export function heldByGlobal(t: TaskSummary, pausedAll: boolean): boolean {
  return pausedAll && !t.unpaused && (t.status === "running" || t.status === "waiting");
}

/**
 * Whether the global flag is holding anything at all.
 *
 * `paused_all` is a sticky flag that lives in `settings.json` and is re-set on
 * every quit, but what it *does* is narrow: the backend only freezes chats that
 * are working, the same rule `heldByGlobal` mirrors. So a chat that finished,
 * was stopped, or was never working drops out of the freeze while the flag
 * stays on — and a flag left over from an earlier "Pause all" (or from the
 * freeze a quit leaves behind) ends up holding nothing.
 *
 * Reading the flag raw is what made the home banner claim "Everything is
 * paused" over a paused list that said "Nothing is frozen right now": the banner
 * and the list disagreed, because only the list asked what the flag was holding.
 * A flag that holds nothing is not a pause, so callers that announce a global
 * pause test this instead of `paused_all`, and the two can never disagree.
 */
export function globalPauseHolds(tasks: TaskSummary[], pausedAll: boolean): boolean {
  return pausedAll && tasks.some((t) => heldByGlobal(t, true));
}

/**
 * The banner's headline, in the order the states actually happen.
 *
 * `settling` comes before everything on purpose. Between "Pause all" being
 * pressed and the backend reporting how many commands are still in flight, the
 * counts read zero — so the banner used to fall through to "Everything is
 * paused", the one wording that reads as *nothing is happening, quit the app*,
 * for a second or two on every pause. Anything not yet known is a loading
 * state, and is named as one.
 */
export function pauseHeadline(o: { settling: boolean; draining: number; pausedAll: boolean; pausedN: number }): string {
  if (o.settling) return "Loading… please wait";
  if (o.draining > 0) return `Pausing… waiting on ${o.draining} command${o.draining === 1 ? "" : "s"} to finish`;
  return o.pausedAll ? "Everything is paused" : `${o.pausedN} task${o.pausedN === 1 ? "" : "s"} paused`;
}

/** Why a chat is frozen, in the words the session's own pause banner uses. */
export function pauseReason(t: TaskSummary, pausedAll: boolean): string {
  if (t.busy > 0) return `Pausing… waiting on ${t.busy} command${t.busy === 1 ? "" : "s"} to finish`;
  const kind = t.paused?.kind;
  if (kind === "exhausted") return "Paused · every model failed";
  if (kind === "closed") return "Paused · the app closed mid-run";
  if (kind === "error") return "Paused · the agent hit an error";
  if (t.paused) return "Paused";
  return pausedAll ? "Paused by Pause all" : "Paused";
}

export function Paused() {
  const tasks = useStore((s) => s.tasks);
  const pausedAll = useStore((s) => s.settings?.paused_all ?? false);
  const globalReason = useStore((s) => s.settings?.paused_reason ?? "");
  const [msg, setMsg] = useState("");
  // Which row's discard is waiting on a second click. It gives up the chat's
  // frozen work, so it arms rather than fires, the way the saved-prompts delete does.
  const [arm, setArm] = useState<string | null>(null);
  // Whether "Dismiss all" is waiting on its second click.
  const [armAll, setArmAll] = useState(false);
  // Chats ticked to be lifted, so the user can thaw a few instead of all of them.
  // Own local state, not the sidebar's `picked`: this list is about resuming, and
  // a selection that outlived the view (or leaked into a mass message) would be a
  // trap. Cleared whenever a chat leaves the list, so a stale tick can never
  // resume something that is no longer on screen.
  const [picked, setPicked] = useState<Record<string, true>>({});
  // The chat you paused last is at the top: ordered by when the user last acted,
  // the same rule as the sidebar (see `sortKey`).
  const list = Object.values(tasks)
    .filter((t) => isPaused(t, pausedAll))
    .sort((a, b) => sortKey(b) - sortKey(a));
  const ids = list.map((t) => t.id);
  // One string, not an array: `ids` is rebuilt every render, so it can never be a
  // dependency. Its contents are the only thing the effect below cares about.
  const roster = ids.join("|");
  const chosen = ids.filter((id) => picked[id]);
  // `chosen` only ever names chats still on the list, so a tick can't outlive its
  // row. Drop the dead keys too, or a chat that is resumed and later paused
  // again would come back pre-ticked.
  useEffect(() => {
    const live = new Set(roster.split("|").filter(Boolean));
    setPicked((p) => {
      const kept = Object.keys(p).filter((id) => live.has(id));
      return kept.length === Object.keys(p).length ? p : Object.fromEntries(kept.map((id) => [id, true as const]));
    });
  }, [roster]);
  const tick = (id: string) => setPicked((p) => {
    const next = { ...p };
    if (next[id]) delete next[id];
    else next[id] = true;
    return next;
  });
  // Lift one chat. This is `task_resume`, not `resume_all`, even under a global
  // pause: resuming one chat used to go through Resume all, which dropped the
  // global flag and woke every chat — so opening a frozen chat and resuming it
  // was the one action that unfroze everything the user had left frozen.
  // `task_resume` opts this chat out on its own; only "Resume all" clears the flag.
  const lift = (t: TaskSummary) => api.resume(t.id).catch((e) => flash(String(e)));
  // Lift just the ticked chats. `tasksResume` walks the ids rather than dropping
  // the global flag, so the chats left unticked stay frozen.
  const liftPicked = () => {
    if (!chosen.length) return;
    void api.tasksResume(chosen, msg).then((n) => {
      setPicked({});
      setMsg("");
      flash(n ? `Resumed ${n} chat${n === 1 ? "" : "s"}` : "Nothing left to resume");
    }).catch((e) => flash(String(e)));
  };
  // Discard: give up on the frozen work instead of lifting it. The run is
  // cancelled and the chat lands in the stopped state, so it leaves this list —
  // the transcript stays, and the agent picks up again from wherever you continue.
  const discard = (t: TaskSummary) => {
    setArm(null);
    void api.dismissPause(t.id).then(() => flash(`Discarded ${oneLine(t.title, 40)} · stopped`)).catch((e) => flash(String(e)));
  };
  // Discard every frozen chat, or just the ticked ones.
  //
  // The other half of "Resume all", and the one that was missing: a screen full
  // of frozen chats is usually a screen full of chats the user no longer wants,
  // and discarding them meant pressing the trash on every row. It arms rather
  // than fires, like every other discard here — the frozen work is cancelled and
  // there is no way back into the frozen state.
  const discardMany = (ids: string[]) => {
    setArmAll(false);
    void api.tasksDismissPause(ids).then((n) => {
      setPicked({});
      flash(n ? `Discarded ${n} chat${n === 1 ? "" : "s"} · stopped` : "Nothing left to discard");
    }).catch((e) => flash(String(e)));
  };
  return (
    <div style={{ flex: 1, overflow: "auto", padding: "24px 28px" }}>
      <div style={{ maxWidth: 860, margin: "0 auto", display: "flex", flexDirection: "column", gap: 14 }}>
        <div style={{ display: "flex", alignItems: "flex-end", gap: 24, flexWrap: "wrap", animation: "olIn .28s cubic-bezier(.22,1,.36,1) both" }}>
          <div style={{ flex: 1, minWidth: 200 }}>
            <div style={{ fontSize: 20, fontWeight: 600, letterSpacing: "-0.025em" }}>Paused chats</div>
            <div className="secondary-text" style={{ marginTop: 3 }}>
              {list.length
                ? `${list.length} frozen · click one to open it, or tick it to resume just that one${pausedAll ? " · everything is paused" : ""}`
                : "Nothing is frozen right now"}
            </div>
          </div>
          {list.length > 0 && (
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              {chosen.length > 0 && (
                <>
                  <Button onClick={liftPicked} title={`Resume only the ${chosen.length} ticked chat${chosen.length === 1 ? "" : "s"}`}>
                    {I.play(10)}Resume {chosen.length}
                  </Button>
                  <Button variant="ghost" onClick={() => setPicked({})}>Clear</Button>
                </>
              )}
              {armAll ? (
                <Tooltip content={`Cancel the frozen work in ${chosen.length ? `${chosen.length} ticked chat${chosen.length === 1 ? "" : "s"}` : `all ${list.length} chat${list.length === 1 ? "" : "s"}`} and stop them`}>
                  <Pressable className="saveddel armed" onClick={() => discardMany(chosen.length ? chosen : ids)} aria-label="Confirm dismissing">
                    Discard {chosen.length || list.length}
                  </Pressable>
                </Tooltip>
              ) : (
                <Tooltip content={chosen.length
                  ? `Cancel the frozen work in the ${chosen.length} ticked chat${chosen.length === 1 ? "" : "s"} and stop them`
                  : `Cancel the frozen work in all ${list.length} chat${list.length === 1 ? "" : "s"} and stop them`}>
                  <Button variant="ghost" onClick={() => setArmAll(true)} aria-label="Dismiss all">{I.trash(11)}Dismiss all</Button>
                </Tooltip>
              )}
              <Button style={{ background: "#fbbf24", color: "#1b1405", border: 0, fontWeight: 600 }}
                onClick={() => api.resumeAll(msg).then(() => { setMsg(""); flash("Resumed everything"); }).catch((e) => flash(String(e)))}>
                {I.play(10)}Resume all
              </Button>
            </div>
          )}
        </div>
        {list.length > 0 && (
          <Input value={msg} onChange={(e) => setMsg(e.currentTarget.value)}
            placeholder="Optional message to every agent when they resume" aria-label="Message for agents when they resume" />
        )}
        {pausedAll && globalReason && <div style={{ fontSize: 11.5, color: "var(--mut3)" }}>{globalReason}</div>}
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {list.map((t) => (
            <div key={t.id} className={"savedrow" + (picked[t.id] ? " picked" : "")} onClick={() => go("session", { task: t.id })}
              onMouseLeave={() => arm === t.id && setArm(null)}
              onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); go("session", { task: t.id }); } }}
              role="button" tabIndex={0}>
              {/* Tick to choose what the "Resume N" button lifts. Separate from
                  the row's own click, which opens the chat. */}
              <span className={"pickbox" + (picked[t.id] ? " on" : "")} aria-hidden="true" onClick={(e) => { e.stopPropagation(); tick(t.id); }}>{picked[t.id] && I.close(9)}</span>
              <span className="savedglyph" style={{ background: "rgba(251,191,36,0.12)", color: "#fbbf24" }}><StatusIcon status="paused" size={13} /></span>
              <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 3 }}>
                <span className="savedtext">{oneLine(t.title, 200)}</span>
                <span style={{ display: "flex", alignItems: "center", gap: 7, fontSize: 11, color: "var(--dim)" }}>
                  <Tooltip content={t.paused?.reason || (pausedAll ? globalReason : "Paused")}>
                    <span style={{ color: "#c9b37a" }}>{pauseReason(t, pausedAll)}</span>
                  </Tooltip>
                  {t.paused && <span style={{ fontVariantNumeric: "tabular-nums" }}>{ago(t.paused.since)} ago</span>}
                  {t.branch && <span className="mono" style={{ fontSize: 10.5 }}>{t.branch}</span>}
                  {t.project && <Tooltip content={t.project}><span className="savedtag">{baseName(t.project)}</span></Tooltip>}
                  <span className="mono" style={{ color: modelColor(chatModel(t)) }}>{chatModel(t).split("/").pop()}</span>
                </span>
              </div>
              <Pressable className="saveddel" style={{ opacity: 1 }} onClick={(e) => { e.stopPropagation(); void lift(t); }}
                aria-label={`Resume ${oneLine(t.title, 40)}`}>{I.play(12)}</Pressable>
              <Tooltip content="Turn this pause into a stop: frozen work is cancelled and the agent is told when you continue">
                <Pressable className="saveddel" onClick={(e) => { e.stopPropagation(); void api.dismissPause(t.id).catch((err) => flash(String(err))); }}
                  aria-label={`Stop ${oneLine(t.title, 40)} instead of resuming`}>{I.close(11)}</Pressable>
              </Tooltip>
              {/* Discarding is the way a frozen chat leaves this list, so it gets
                  a control of its own rather than a second name for the stop
                  button. It arms first: the run is cancelled and the chat goes
                  to stopped, and there is no way back into the frozen state. */}
              <Tooltip content={arm === t.id ? "Click again to discard this chat's frozen work" : "Discard: cancel the frozen work and leave this chat stopped"}>
                {arm === t.id ? (
                  <Pressable className="saveddel armed" onClick={(e) => { e.stopPropagation(); discard(t); }}
                    aria-label={`Discard ${oneLine(t.title, 40)} and stop it`}>Discard</Pressable>
                ) : (
                  <Pressable className="saveddel" onClick={(e) => { e.stopPropagation(); setArm(t.id); }} onFocus={() => setArm(t.id)}
                    aria-label={`Discard ${oneLine(t.title, 40)}: cancel its frozen work and stop it`}>{I.trash(12)}</Pressable>
                )}
              </Tooltip>
            </div>
          ))}
        </div>
        {!list.length && (
          <div className="empty">
            No chat is paused. Freeze one from its header (or press Pause all) and it turns up here.
          </div>
        )}
      </div>
    </div>
  );
}
