import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { createPortal } from "react-dom";
import { Check, MonitorUp, Sparkles } from "lucide-react";
import { ago, api, baseName, normPath, TaskSummary } from "../api";
import { back, chatModel, flash, forgetTask, forward, get, go, isNarrow, loadProject, modelColor, openProject, requestPause, saveSettings, set, shownAccounts, shownProviders, type State, useNow, useStore, zoom } from "../store";
import { capsOf, netWindows } from "./Accounts";
import { I, chatStatus, isParked, statusOf } from "./icons";
import { openChatStats } from "./ChatStats";
import { openRunTimeline } from "./RunTimeline";
import { openMove } from "./MoveChat";
import { openSaveAsSkill } from "./SaveAsSkill";
import { useRowDrag, type RowDrag } from "./drag";
import { pickFolder } from "./Home";
import { useOpenSources } from "./OpenProject";
import { ProjectName } from "./ProjectName";
import { isPaused, heldByGlobal } from "./Paused";
import { ProviderIcon } from "./ProviderIcon";
import { swarmState } from "./session/Swarm";
import { NeedsYou } from "./NeedsYou";
import { RemoteIndicator } from "./Remote";
import { isRemoteClient } from "../remote";
import { AnchoredPanel, Tooltip, IconButton, Input, MenuRow, MenuScrim, ctxPlace, menuLayer, Pressable, Switch, useAnchoredPanel, useEscapeClose, WindowButton, Kbd } from "./primitives";
import { settingsTabDef, type SettingsTab } from "./settingsTabs";
import { groupRaceRows, useRaces } from "../race";
import { RaceGroup } from "./RaceGroup";

const win = () => getCurrentWindow();

/**
 * Open the Settings page the sidebar footer's shortcut button points at.
 *
 * The button sits beside the usage bar, which is about accounts, so Accounts is
 * the default it has always had — but Settings → Tabs can aim it at any page.
 * An id this build has no page for resolves to Accounts rather than opening a
 * blank settings screen; see `settingsTabDef`.
 */
export const openSidebarShortcut = () => go("settings", { settingsTab: settingsTabDef(get().settings?.sidebar_button_tab).id });

/** One entry in the project menu: the current folder first, then the remembered ones, no duplicates. */
export function projectMenuEntries(current: string, remembered: string[]): string[] {
  const seen: string[] = [];
  for (const p of [current, ...remembered]) {
    if (p && !seen.some((x) => normPath(x) === normPath(p))) seen.push(p);
  }
  return seen;
}

/** The remembered list with one folder dropped, however that path happens to be spelled. */
export function withoutProject(remembered: string[], drop: string): string[] {
  return remembered.filter((p) => normPath(p) !== normPath(drop));
}

/**
 * Drop a folder from the project selector. Only the remembered list changes: the
 * chats in that folder stay on disk and come back if it's ever opened again.
 * Removing the folder you're working in leaves the app with no project, since a
 * hidden folder can't stay selected.
 */
export async function removeProject(path: string) {
  const s = get().settings;
  if (!s) return;
  const current = normPath(s.project) === normPath(path);
  if (!(await saveSettings({ projects: withoutProject(s.projects ?? [], path), project: current ? "" : s.project }))) return;
  if (current) await loadProject("");
  flash(`Removed ${baseName(path)} from projects · its chats are untouched`);
}

/**
 * Right-click a folder in the project list to drop it from the selector.
 *
 * It has to paint over the list it was opened from: both are portalled into the
 * body, so without a layer of its own it would sit under the list (z 90 against
 * 60). The height is measured on mount, so a menu opened on the bottom row
 * turns upwards instead of hanging off the screen — which is where the list's
 * last folder normally is.
 */
export function ProjectCtxMenu({ path, x, y, z, close }: { path: string; x: number; y: number; z: number; close: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [h, setH] = useState(72);
  useLayoutEffect(() => { setH(ref.current?.offsetHeight || 72); }, []);
  return (
    <div style={menuLayer(z)}>
      <MenuScrim layer={90} onClick={close} onContextMenu={(e) => { e.preventDefault(); close(); }} />
      {/* z 91, not the `.pop` default of 41: the scrim below sits at 90 inside this
          same layer, so the menu has to outrank it — otherwise it renders *over* a
          visible menu and swallows every click. */}
      <div ref={ref} className="pop ctx" style={{ ...ctxPlace(x, y, 242, h, z), zIndex: 91, overflowY: "auto" }}>
        <div className="mmodel" aria-disabled="true">
          <span className="cico">{I.folder()}</span>
          <span className="mm-name"><ProjectName path={path} /></span>
        </div>
        <div className="msep" />
        <MenuRow onClick={() => { close(); void removeProject(path).catch((e) => flash(String(e))); }}>
          <span className="cico">{I.trash(12)}</span>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontWeight: 500 }}>Remove from list</div>
            <div style={{ fontSize: 11, color: "var(--mut3)", lineHeight: 1.4 }}>Hides the folder. Chats are kept.</div>
          </div>
        </MenuRow>
      </div>
    </div>
  );
}

/**
 * Whether an open chat's folder is the one the selector is on. Spelled paths
 * are compared the way the sidebar compares them, so `D:\work\app` and
 * `d:/work/app/` are one folder. A chat with no folder of its own counts as
 * the current one: there is nothing to disagree with.
 */
export function chatProjectIsCurrent(chatProject: string, selected: string): boolean {
  return !chatProject || !selected || normPath(chatProject) === normPath(selected);
}

/** Titlebar project selector: the folder new tasks run in, switchable from anywhere in the app. */
function ProjectPicker() {
  const project = useStore((s) => s.settings?.project ?? "");
  const projects = useStore((s) => s.settings?.projects);
  const info = useStore((s) => s.projectInfo);
  const [openMenuState, setOpen] = useState(false);
  const [hi, setHi] = useState(0);
  const [ctx, setCtx] = useState<{ p: string; x: number; y: number } | null>(null);
  const btn = useRef<HTMLDivElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  // Same zoom the other overlays read: the popover lives outside the zoomed app root.
  const z = zoom();
  // Memoised so the effect below can name it as a dependency without recomputing
  // on every render; `projectMenuEntries` reads only these two inputs.
  const list = useMemo(() => projectMenuEntries(project, projects ?? []), [project, projects]);
  const [pos, setPos] = useState<{ left: number; top: number; width: number; maxHeight: number } | null>(null);
  // Bumped when the window resizes while the popover is open. It is a dependency
  // of the layout effect below, which is the thing that recomputes `pos`. There
  // used to be a `resize` listener that called
  // `window.dispatchEvent(new Event("resize"))` — re-dispatching the very event
  // it was subscribed to. The dispatch is synchronous and re-entrant, so the
  // first window resize recursed until the stack overflowed, and it could not
  // move the popover anyway, because nothing in the effect read the window size.
  const [resizeTick, setResizeTick] = useState(0);

  // The titlebar is draggable, so the popover can't be a child of the button.
  // Recompute on open and on resize, so a window shrink can't leave it off-screen.
  useLayoutEffect(() => {
    if (!openMenuState || !btn.current) return;
    const r = btn.current.getBoundingClientRect();
    const vw = window.innerWidth / z, vh = window.innerHeight / z;
    const width = Math.min(300, Math.max(r.width / z, 220));
    const maxH = Math.max(160, vh - r.bottom / z - 16);
    setPos({
      left: Math.max(8, Math.min(r.left / z, vw - width - 8)),
      top: Math.max(8, Math.min(r.bottom / z + 6, vh - 8)),
      width,
      maxHeight: maxH,
    });
  }, [openMenuState, z, list.length, resizeTick]);
  useEffect(() => {
    if (!openMenuState) return;
    const onResize = () => setResizeTick((n) => n + 1);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [openMenuState]);
  useEffect(() => { if (openMenuState) setHi(Math.max(0, list.indexOf(project))); }, [openMenuState, project, list]);
  useEffect(() => {
    if (!openMenuState) return;
    pop.current?.querySelector(".hi")?.scrollIntoView({ block: "nearest" });
  }, [hi, openMenuState]);

  // Escape closes the list and nothing else: the app's global handler would pause a running agent.
  // A right-click menu is open above the list, so it goes first — otherwise the list closes under it
  // and the menu is left floating on its own.
  useEscapeClose(!!openMenuState, () => { if (ctx) setCtx(null); else setOpen(false); });

  const pick = (p: string) => { setOpen(false); if (p && p !== project) void openProject(p); };
  const sources = useOpenSources(openMenuState);
  const openFolder = () => { setOpen(false); void pickFolder().catch((e) => flash(String(e))); };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") { e.preventDefault(); setHi((h) => Math.min(h + 1, list.length - 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setHi((h) => Math.max(h - 1, 0)); }
    else if (e.key === "Enter") { e.preventDefault(); list[hi] !== undefined ? pick(list[hi]) : openFolder(); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setOpen(false); }
  };
  // The branch is worth showing; the word "git" in front of it never is.
  const hint = project ? (info?.git ? info.branch : "") : "Ctrl O";

  return (
    <>
      <Tooltip content="Switch project folder · Ctrl O">
        <div ref={btn} className={"projbtn" + (openMenuState ? " open" : "")} role="combobox" aria-label="Project folder" aria-expanded={openMenuState} aria-haspopup="listbox" tabIndex={0}
          onClick={() => setOpen(!openMenuState)}
          onKeyDown={(e) => { if (!openMenuState && (e.key === "Enter" || e.key === "ArrowDown")) { e.preventDefault(); setOpen(true); } else onKey(e); }}>
          {I.folder()}
          <span className="pv">{project ? <ProjectName path={project} /> : "No project"}</span>
          {hint && <span className="ph">{hint}</span>}
          <span className="pc">{I.chev()}</span>
        </div>
      </Tooltip>
      {/* Open whichever menu is in front: a right-click on a folder supersedes the list. */}
      {(ctx || (openMenuState && pos)) && createPortal(
        ctx
          ? <ProjectCtxMenu path={ctx.p} x={ctx.x} y={ctx.y} z={z} close={() => setCtx(null)} />
          : <div style={{ position: "fixed", inset: 0, zoom: z, zIndex: 60 }}>
            <div className="scrim" onMouseDown={() => setOpen(false)} />
            <div ref={pop} className="pop projpop" role="listbox" aria-label="Projects" style={{ left: pos!.left, top: pos!.top, width: pos!.width, maxHeight: pos!.maxHeight, overflowY: "auto" }} onKeyDown={onKey}>
              <div className="mhead">Project folder</div>
              {list.map((p, i) => (
                <MenuRow key={p} role="option" aria-selected={p === project} className={i === hi ? "hi" : ""} style={{ animationDelay: i * 6 + "ms" }}
                  onMouseMove={() => setHi(i)} onClick={() => pick(p)}
                  onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); setCtx({ p, x: e.clientX, y: e.clientY }); }}>
                  <span className="cico">{I.folder()}</span>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontWeight: 500, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}><ProjectName path={p} /></div>
                    <div style={{ fontSize: 11, color: "var(--mut3)", lineHeight: 1.4, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{p}</div>
                  </div>
                  <span style={{ width: 12, flex: "none", color: "var(--violet)", display: "flex" }}>{p === project ? <Check size={12} strokeWidth={2} /> : null}</span>
                </MenuRow>
              ))}
              {!list.length && <div style={{ padding: "2px 8px 6px", fontSize: 11.5, color: "var(--dim)" }}>No project yet</div>}
              <div className="msep" />
              {sources.map((o) => (
                <MenuRow key={o.id} onClick={() => { setOpen(false); o.run(); }}><span className="cico">{o.icon}</span><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>{o.label}</div><div style={{ fontSize: 11, color: "var(--mut3)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{o.hint}</div></div></MenuRow>
              ))}
            </div>
          </div>,
        document.body,
      )}
    </>
  );
}

/**
 * The folder the open chat is actually working in, next to the project selector.
 *
 * The selector says where *new* tasks go; this says where *this* one went. They
 * are usually the same, but switching projects deliberately leaves the open chat
 * in its own folder, so when they differ the chat is the thing that's still
 * running against the old project — which is exactly when it needs saying.
 */
function ChatProject() {
  const selected = useStore((s) => s.settings?.project ?? "");
  const chat = useStore((s) => (s.task ? s.tasks[s.task] : null));
  const inChat = useStore((s) => s.view === "session" || s.view === "diff");
  // Only in a chat: on Home, the selector is already the answer to the question.
  if (!inChat || !chat) return null;
  const path = chat.project;
  if (!path) return null;
  const same = chatProjectIsCurrent(path, selected);
  // The name alone can't tell two same-named folders apart, so the full path is
  // one hover away, and a mismatching chat offers to switch to it.
  const tip = same
    ? `This chat works in ${path}`
    : `This chat works in ${path} — the selector is on ${selected}. Click to switch.`;
  return (
    <Tooltip content={tip}>
      <div className={"chatproj" + (same ? "" : " off")}
        {...(same ? {} : { role: "button", tabIndex: 0, onClick: () => void openProject(path),
          onKeyDown: (e: React.KeyboardEvent) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); e.stopPropagation(); void openProject(path); } } })}>
        {I.folder()}
        <span className="cp-name"><ProjectName path={path} /></span>
        {!same && <span className="cp-flag">here</span>}
      </div>
    </Tooltip>
  );
}

export function Titlebar() {
  const canBack = useStore((s) => s.hist.length > 0);
  const canFwd = useStore((s) => s.fwd.length > 0);
  // On a chat's Artificts surface the bar tucks up to a thin strip, so the
  // design gets the height; hovering or focusing the strip slides the whole bar
  // back down over the content. It stays mounted throughout: it is the window's
  // drag region and holds the window controls of a frameless window.
  const tucked = useStore((s) => (s.view === "session" && !!s.task && (s.tasks[s.task]?.artifict_open ?? false)) || (s.view === "home" && !!s.home.artificts));
  return (
    <div className={tucked ? "titlebar tucked" : "titlebar"} data-tauri-drag-region>
      <IconButton className="corner" label="Toggle sidebar · Ctrl B" onClick={() => set((s) => ({ sidebar: !s.sidebar }))}>{I.sidebar()}</IconButton>
      <IconButton label="Back" disabled={!canBack} onClick={back}>{I.back()}</IconButton>
      <IconButton label="Forward" disabled={!canFwd} onClick={forward}>{I.fwd()}</IconButton>
      <ProjectPicker />
      <ChatProject />
      <div style={{ flex: 1 }} data-tauri-drag-region />
      <Pressable className="search" onClick={() => set({ pal: true, menu: null })}>
        {I.search()}
        <span style={{ flex: 1, whiteSpace: "nowrap", overflow: "hidden" }}>Search tasks and commands</span>
        <Kbd>Ctrl K</Kbd>
      </Pressable>
      <div style={{ flex: 1 }} data-tauri-drag-region />
      <RemoteIndicator onOpen={() => go("settings", { settingsTab: "remote" })} />
      {/* A remote browser has no window of its own to minimize or close, and
          these would act on the desktop window out from under its user. */}
      {!isRemoteClient() && <div style={{ display: "flex", height: 40 }}>
        <WindowButton label="Minimize" onClick={() => win().minimize()}><div style={{ width: 10, height: 1, background: "currentColor" }} /></WindowButton>
        <WindowButton label="Maximize or restore" onClick={() => win().toggleMaximize()}><div style={{ width: 9, height: 9, border: "1px solid currentColor", borderRadius: 1.5 }} /></WindowButton>
        <WindowButton label="Close window" close onClick={() => win().close()}>
          <div style={{ position: "relative", width: 10, height: 10 }}>
            <div style={{ position: "absolute", left: 4.5, top: -1, width: 1, height: 12, background: "currentColor", transform: "rotate(45deg)" }} />
            <div style={{ position: "absolute", left: 4.5, top: -1, width: 1, height: 12, background: "currentColor", transform: "rotate(-45deg)" }} />
          </div>
        </WindowButton>
      </div>}
    </div>
  );
}

/**
 * Sidebar sort key: recent user activity, or completion time for settled chats.
 * Agent steps do not move a working chat ahead of one the user just opened;
 * when a run finishes, its completion timestamp brings it to the top. A manual
 * position set by dragging takes precedence until the user returns to that chat.
 */
export const sortKey = (t: TaskSummary, automatic = get().settings?.automatic_chat_reorder !== false) => t.order || (automatic
  ? Date.parse((t.status === "done" || t.status === "failed" || t.status === "stopped") ? t.updated_at : (t.touched_at || t.updated_at))
  : Date.parse(t.touched_at || t.updated_at));

/** Chats needing a response, paused chats, and active chats rise above finished work. */
export function chatPriority(t: TaskSummary, pausedAll = false): number {
  if (t.status === "waiting" && (t.waiting_kind === "question" || t.waiting_kind === "approval")) return 0;
  if (isPaused(t, pausedAll)) return 1;
  // A chat parked on a trigger (`wait_for_event`) is still going: it wakes itself,
  // so it belongs with the running chats. Ranking it below `done` sank it to the
  // bottom of the list, which hid the chats the user is most likely waiting for.
  if (t.status === "running" || t.status === "waiting") return 2;
  if (t.status === "done" || t.status === "failed" || t.status === "stopped") return 3;
  return 4;
}

/** Sort by attention first, then retain manual/automatic order within each group. */
export function compareChats(a: TaskSummary, b: TaskSummary, pausedAll = false, automatic = get().settings?.automatic_chat_reorder !== false): number {
  return chatPriority(a, pausedAll) - chatPriority(b, pausedAll) || sortKey(b, automatic) - sortKey(a, automatic);
}

/** New `order` for dropping between two neighbours (either may be missing). */
export function orderBetween(above: TaskSummary | undefined, below: TaskSummary | undefined, automatic = get().settings?.automatic_chat_reorder !== false): number {
  if (above && below) return (sortKey(above, automatic) + sortKey(below, automatic)) / 2;
  if (above) return sortKey(above, automatic) - 60_000;
  if (below) return sortKey(below, automatic) + 60_000;
  return Date.now();
}

/**
 * Sort by a key computed once per chat, not once per comparison.
 *
 * `sortKey` parses up to two ISO timestamps, and a comparator that calls it pays
 * that twice for every comparison — about `2·n·log n` parses where `2·n` would
 * do, so ~1000 of them for an 80-chat list. The sidebar rebuilds and re-sorts
 * this list on every task event from every chat, which is at its worst exactly
 * when a lot of chats are running, so this is the difference between parsing 80
 * dates per event and parsing a thousand.
 *
 * Stable the same way the plain `sort` was: ties keep their input order, so the
 * ordered result is identical.
 */
export function bySortKey(list: TaskSummary[], automatic: boolean, dir: -1 | 1 = -1): TaskSummary[] {
  return list
    .map((t) => [sortKey(t, automatic), t] as const)
    .sort((a, b) => dir * (a[0] - b[0]))
    .map(([, t]) => t);
}



/** Sidebar shows a chat when it belongs to the current project (no project = everything), or when it is the open chat. */
export function chatInProject(t: TaskSummary, project: string, allProjects: boolean, cur: string | null): boolean {
  return allProjects || !project || normPath(t.project) === normPath(project) || t.id === cur;
}


/** Patch a task summary. Reports its own failure, so every caller is safe to
 *  fire and forget; returns whether it landed, for the ones that must not
 *  carry on as if it had. */

async function patchTask(id: string, patch: Parameters<typeof api.update>[1]) {
  try {
    const t = await api.update(id, patch);
    set((s) => ({ tasks: { ...s.tasks, [t.id]: t } }));
    return true;
  } catch (e) {
    flash(String(e));
    return false;
  }
}

export async function archiveTask(id: string, archived = true) {
  // Only leave the open chat if the archive actually happened: navigating on a
  // failed write would drop the user out of a chat that's still there.
  if (!(await patchTask(id, { archived }))) return;
  const s = get();
  if (archived && s.task === id && (s.view === "session" || s.view === "diff")) go("home");
  // Archiving is reversible and used to offer no way back, which is most of why
  // it read as destructive. The toast is the way back.
  if (archived) flash("Chat archived", { label: "Undo", run: () => { void patchTask(id, { archived: false }); } });
}

export async function deleteTask(id: string, removeWorktree = false) {
  const task = get().tasks[id];
  const workers = task?.subs?.filter((s) => s.cwd).length ?? 0;
  const result = await api.remove(id, removeWorktree).then(() => true).catch((e) => {
    flash(String(e));
    return false;
  });
  if (!result) return false;
  set((s) => forgetTask(s, id));
  if (task && id === get().task && (get().view === "session" || get().view === "diff")) go("home");
  const kept = !!task?.worktree || workers > 0;
  if (removeWorktree) flash(kept ? "Chat deleted · worktrees removed · branches kept" : "Chat deleted");
  else if (task?.worktree) flash(`Chat deleted · worktree kept at ${task.cwd} (${task.branch})${workers ? ` · ${workers} worker worktree${workers === 1 ? "" : "s"} kept` : ""}`);
  else if (workers) flash(`Chat deleted · ${workers} worker worktree${workers === 1 ? "" : "s"} kept`);
  return true;
}

/** Put every ticked chat away, then leave select mode with nothing ticked.
 *
 * The open chat is left for home the same way `archiveTask` leaves it for one,
 * and only once the write has landed: navigating on a failed archive would drop
 * the user out of a chat that is still there. The ticks go either way — a chat
 * that stayed in the list must not stay ticked, or the count would keep
 * promising an action on a selection the user can no longer see.
 */
export async function archivePicked(ids: string[]) {
  if (!ids.length) return;
  const before = get().task;
  const n = await api.archiveTasks(ids).catch((e) => {
    // The failure is the news; saying "already archived" on top of it would
    // report the one outcome that did not happen.
    flash(String(e));
    return -1;
  });
  // The ticks go either way: the chats either left the list or the write failed,
  // and a selection of chats that are still there is one the user has to read
  // and undo by hand.
  set({ picked: {}, selecting: false });
  if (n < 0) return;
  if (n && before && ids.includes(before) && (get().view === "session" || get().view === "diff")) go("home");
  flash(n ? `Archived ${n} chat${n === 1 ? "" : "s"}` : "Those chats are already archived", n
    ? { label: "Undo", run: () => { for (const id of ids) void patchTask(id, { archived: false }); } }
    : undefined);
}

/** Pin or unpin every ticked chat in one write each, keeping the ticks: pinning
 *  is not destructive and the user is likely to do more than one thing to a
 *  selection. Reports its own failures through `patchTask`. */
async function pinPicked(ids: string[], pinned: boolean) {
  await Promise.all(ids.map((id) => patchTask(id, { pinned })));
}

/** Delete every archived chat at once. Their worktrees are kept on disk.
 *
 *  One call rather than a loop of deletes: the backend re-derives the archived
 *  set itself (`delete_archived` in `lib.rs`), so a client list that went stale
 *  cannot leave a chat behind or delete one that was just unarchived, and it
 *  preserves worktrees the same way this used to. `api.deleteArchived` used to
 *  have no caller at all, with this loop reimplementing it. */
export async function deleteArchivedTasks() {
  const before = get();
  const ids = Object.values(before.tasks).filter((task) => task.archived).map((task) => task.id);
  // Nothing to do, and no reason to ask the backend: this is the one case where
  // the client's own list is the whole answer.
  if (!ids.length) {
    flash("No archived chats");
    return;
  }
  // Both kinds count: a chat's own worktree, and the worker worktrees an
  // ultrathread run left behind. The count is what the toast promises was kept.
  const keptWorktrees = ids.reduce((n, id) => {
    const t = before.tasks[id];
    return n + (t?.worktree ? 1 : 0) + (t?.subs?.filter((s) => s.cwd).length ?? 0);
  }, 0);
  let removed: number;
  try {
    removed = await api.deleteArchived();
  } catch (e) {
    flash(String(e));
    return;
  }
  // Forget locally whatever we believed was archived: those are exactly the rows
  // whose files the backend just removed.
  set((s) => {
    let next = s;
    for (const id of ids) next = { ...next, ...forgetTask(next, id) } as State;
    return next === s ? {} : (next as unknown as Partial<State>);
  });
  if (before.task && ids.includes(before.task) && (before.view === "session" || before.view === "diff")) go("home");
  flash(
    removed
      ? `Deleted ${removed} archived chat${removed === 1 ? "" : "s"}${keptWorktrees ? ` · kept ${keptWorktrees} worktree${keptWorktrees === 1 ? "" : "s"} on disk` : ""}`
      : "Archived chats could not be deleted",
  );
}

export const SIDEBAR_CHAT_LIMIT = 100;

/** Scheduled run chats have a stable, backend-locked title prefix. */
export function isScheduledChat(task: Pick<TaskSummary, "title">): boolean {
  return typeof task.title === "string" && task.title.startsWith("⏰ ");
}

/** Oldest eligible chats beyond their list cap, preserving pinned, active, paused, unread and current chats. */
export function excessSidebarChats(tasks: Record<string, TaskSummary>, limit = SIDEBAR_CHAT_LIMIT, current: string | null = null, pausedAll = false, scheduled = false): TaskSummary[] {
  const unarchived = Object.values(tasks).filter((t) => !t.archived && isScheduledChat(t) === scheduled);
  const excess = Math.max(0, unarchived.length - limit);
  if (!excess) return [];
  const eligible = unarchived.filter((t) => !t.pinned && t.id !== current && !t.unread
    && !isPaused(t, pausedAll) && !["running", "waiting"].includes(t.status));
  // Oldest first, keyed once per chat: this runs from an effect on every task
  // event while the sidebar is mounted, and it only ever returns the oldest
  // `excess` — so the cheap length check above already covers the common case.
  // The flag is read explicitly because `bySortKey` takes it as an argument
  // where the old call let `sortKey` read its own default.
  const automatic = get().settings?.automatic_chat_reorder !== false;
  const archiveCount = Math.min(excess, eligible.length);
  return bySortKey(eligible, automatic, 1).slice(0, archiveCount);
}

export async function archiveExcessSidebarChats(ids: string[]) {
  if (!ids.length) return;
  try {
    const n = await api.archiveTasks(ids);
    if (!n) return;
    set((s) => ({ tasks: Object.fromEntries(Object.entries(s.tasks).map(([id, task]) => [id, ids.includes(id) ? { ...task, archived: true, pinned: false } : task])) }));
    // Deliberately silent. Announcing the sweep put a banner and an "Undo" in
    // front of every launch that trimmed the list, for a tidy-up nobody asked
    // for; the chats it puts away are still in the sidebar's Archived list,
    // which is where they are looked for. The failure path below still speaks,
    // because a sweep that did *not* happen is the user's news and a successful
    // one is not.
  } catch (e) {
    flash(String(e));
  }
}

export async function exportTasks(ids: string[] | "all") {
  // `ids` is either the literal "all" or the ticked chat ids, and callers only
  // get here with at least one — but it is a plain array, so the read is checked
  // rather than assumed and an empty list falls to the "all" name.
  const one = ids === "all" ? undefined : ids[0];
  const name = one === undefined ? `openleash-chats-${new Date().toISOString().slice(0, 10)}.json` : `${(get().tasks[one]?.title ?? "chat").replace(/[^\w\- ]+/g, "").trim().slice(0, 40) || "chat"}.openleash.json`;
  const path = await save({ defaultPath: name, filters: [{ name: "OpenLeash chats", extensions: ["json"] }] }).catch((e) => { flash(String(e)); return null; });
  if (!path) return;
  try {
    // A selection goes in one file named after the app, not after its first
    // chat: "export these three" is not one chat, and it is certainly not all of
    // them. The single-chat path stays because its file name is the chat's.
    if (ids === "all") flash(`Exported ${await api.exportAll(path, false)} chats`);
    else if (ids.length > 1) flash(`Exported ${await api.exportTasks(ids, path)} chats`);
    else { await api.exportTask(one!, path); flash("Chat exported"); }
  } catch (e) {
    flash(String(e));
  }
}

/** Write every archived chat to one file, whatever its title or status.
 *
 *  The one export that starts from the archive rather than the list: archived
 *  chats are hidden from the sidebar by design, so the usual "export these"
 *  paths cannot reach them, and this is the button under the list that is
 *  literally a list of them. Goes through `tasks_export` like the selection
 *  export — one file of full chat bodies, not the summaries the settings page
 *  shows — and takes the ids off the store fresh, so a chat restored a moment
 *  ago is not swept into a backup nobody asked to include it. */
export async function exportArchived() {
  const ids = Object.values(get().tasks).filter((t) => t.archived).map((t) => t.id);
  const path = await save({ defaultPath: `openleash-archive-${new Date().toISOString().slice(0, 10)}.json`, filters: [{ name: "OpenLeash chats", extensions: ["json"] }] }).catch((e) => { flash(String(e)); return null; });
  if (!path) return;
  try {
    const n = await api.exportTasks(ids, path);
    flash(n ? `Exported ${n} archived chat${n === 1 ? "" : "s"}` : "Nothing archived");
  } catch (e) {
    flash(String(e));
  }
}

// ───────────────────────── chat selection ─────────────────────────

/** Tick or untick one chat. Pure so the selection rules are testable on their own. */
export function togglePick(picked: Record<string, true>, id: string): Record<string, true> {
  if (picked[id]) {
    const next = { ...picked };
    delete next[id];
    return next;
  }
  return { ...picked, [id]: true };
}

/** Chats a selection can reach: archived chats are out of the way. */
export function messageable(t: TaskSummary): boolean {
  return !t.archived;
}

/** Tick every messageable chat, or clear the ticks when they're all ticked already. */
export function pickAll(picked: Record<string, true>, list: TaskSummary[]): Record<string, true> {
  const targets = list.filter(messageable).map((t) => t.id);
  if (targets.length > 0 && targets.every((id) => picked[id])) {
    const next = { ...picked };
    for (const id of targets) delete next[id];
    return next;
  }
  const next = { ...picked };
  for (const id of targets) next[id] = true;
  return next;
}

/** How wide "Message all chats" reaches. */
export interface MassScope {
  /** Also reach chats frozen by a pause. Off by default: a paused chat is parked
   *  mid-run, so a message aimed at the chats that are working right now lands in
   *  a queue nobody is reading yet, and the send reads as a failure. */
  includePaused: boolean;
  /** Reach chats in other project folders too. Off by default: the common case is
   *  "tell the agents working on this project", and a cross-project send is the
   *  one that can wake something unrelated. */
  allProjects: boolean;
}

/**
 * The chats "Message all chats" reaches: the main agent of every chat that is
 * working right now in the current project, plus frozen ones if asked for.
 *
 * "Working" is `running` and `waiting`, not `idle` — an idle chat has no agent to
 * receive anything, and the turn a fresh message would start is the user's own to
 * begin in the chat where it belongs. A frozen chat counts as working, since it
 * *was* interrupted rather than finished, which is why it needs its own opt-in
 * rather than being folded in silently.
 *
 * Archived chats are never in here: they are out of the way by definition, and
 * the same rule the sidebar selection uses.
 */
export function massTargets(list: TaskSummary[], project: string, pausedAll: boolean, scope: MassScope): TaskSummary[] {
  return list
    .filter((t) => messageable(t))
    .filter((t) => (scope.allProjects || chatProjectIsCurrent(t.project, project)))
    .filter((t) => {
      const frozen = isPaused(t, pausedAll);
      if (scope.includePaused) return true;
      // A frozen chat is not swept in even though it was interrupted rather than
      // finished: it sits in `paused` *because* it was working, and letting it in
      // by accident would be the default the opt-in exists to remove.
      return !frozen && (t.status === "running" || t.status === "waiting");
    });
}

export async function importTasks() {
  const path = await open({ multiple: false, filters: [{ name: "OpenLeash chats", extensions: ["json"] }] }).catch((e) => { flash(String(e)); return null; });
  if (typeof path !== "string") return;
  try {
    const { chats: list, warnings } = await api.importTasks(path);
    set((s) => ({ tasks: { ...s.tasks, ...Object.fromEntries(list.map((t) => [t.id, t])) } }));
    flash(`Imported ${list.length} chat${list.length === 1 ? "" : "s"}${warnings.length ? `. ${warnings[0]}${warnings.length > 1 ? ` (and ${warnings.length - 1} more)` : ""}` : ""}`);
  } catch (e) {
    flash(String(e));
  }
}

/** Point a chat at another folder on this machine. Only the chat's working location changes:
 * no files are copied or deleted. The backend refuses while the chat is working. */
export async function changeChatFolder(id: string) {
  const folder = await open({ directory: true, multiple: false, title: "Choose this chat's work folder" }).catch((e) => { flash(String(e)); return null; });
  if (typeof folder !== "string") return;
  try {
    await api.changeFolder(id, folder);
    flash(`Chat moved to ${baseName(folder)}`);
  } catch (e) {
    flash(String(e));
  }
}

type Ctx = { t: TaskSummary; x: number; y: number } | null;

/** One chat in the sidebar. Memoized, with `drag` and `onCtx` held stable by
 *  the caller: a swarm of agents running republishes every chat's summary, and
 *  without this each of those events re-rendered all 80 mounted rows to redraw
 *  one line. Rows whose own summary is untouched now skip entirely. */
const Row = memo(function Row({ t, active, onCtx, drag }: { t: TaskSummary; active: boolean; onCtx: (e: React.MouseEvent) => void; drag: DragApi }) {
  // This row's own clock, so the "3m ago" text stays fresh without a global
  // tick re-rendering the whole sidebar.
  useNow();
  const pausedAll = useStore((s) => s.settings?.paused_all ?? false);
  // The blue dot is the backend's mark on the summary (`TaskSummary.unread`), not
  // this window's: it survives a reload, and the phone's browser sees the same
  // one. The chat that is on screen right now is the exception — you are looking
  // at it, so its dot does not shout. Read off the row's own `t` rather than a
  // subscription: the summary is the prop this row re-renders for.
  const onScreen = useStore((s) => (s.view === "session" || s.view === "diff") && s.task === t.id);
  const unread = !!t.unread && !onScreen;
  const renaming = useStore((s) => s.renaming === t.id);
  const picked = useStore((s) => !!s.picked[t.id]);
  const selecting = useStore((s) => s.selecting);
  const project = useStore((s) => s.settings?.project ?? "");
  const [arm, setArm] = useState(false);
  const [title, setTitle] = useState(t.title);
  // A chat parked by `wait_for_event` is waiting on a trigger, not on you, so it
  // is drawn by its own quiet status: the red "Needs you" and the attention
  // pulse belong to the waits only the user can end. See `chatStatus`.
  const parked = isParked(t);
  const shown = chatStatus(t);
  const st = statusOf(shown, isPaused(t, pausedAll));
  const live = (shown === "running" || shown === "waiting") && st.text !== "Paused";
  // A chat from another folder is listed (while "show all" is on, or because it
  // is the one being read) but belongs to somewhere else: it steps back a shade
  // and a notch, so this folder's own chats are the ones that read as solid.
  const elsewhere = !chatProjectIsCurrent(t.project, project);
  // Same row either way, only the ramp position changes — a faded running chat
  // is still the brightest thing in a faded row, so live still reads as live.
  const titleColor = elsewhere ? (live ? "var(--mut)" : "var(--mut3)") : live ? "#e4e4e7" : "var(--mut)";
  // A running chat glows in its model's color (Claude = orange). Read through
  // `chatModel`, not `t.model`: a swap made from the picker mid-turn lands in
  // `pending` and waits for the next message, which is what the composer's `*`
  // badge says, and the dot was painting the model the chat was leaving.
  const dot = st.text === "Running" ? modelColor(chatModel(t)) : st.dot;
  // The swarm state still drives the row's class (and its pulse), but it no
  // longer picks the dot's colour. It used to: a live swarm lit the dot violet,
  // which meant an ultrathread chat was violet for the whole run and no swap of
  // its model changed anything you could see — the one row that has to name what
  // a chat is spending money on was the one row painted a mode colour. The mode
  // is still on the row, in the badge and the pin and the header underline; the
  // dot itself belongs to the model, and a settled swarm falls back to the same
  // status colour any other row gets.
  const swarm = t.ultra ? swarmState(t, heldByGlobal(t, pausedAll)) : null;
  // The status dot reports what the chat is doing; unread is a separate mark,
  // never a replacement colour. Otherwise an unread run or wait looks done until
  // opening it hides the unread mark and reveals its actual status.
  const blockingWait = t.waiting_kind === "question" || t.waiting_kind === "approval";
  const needsAnswer = blockingWait || (t.status === "waiting" && !parked);
  const isPausedState = st.text === "Paused";
  // A finished chat has no status to hide behind, so its unread state is the
  // full dot itself, in the unread colour, glowing as it did before. Running and
  // waiting chats keep their status colour and get the corner speck instead.
  const doneUnread = unread && shown === "done" && !isPausedState;
  const showUnreadMark = unread && !needsAnswer && !isPausedState && !doneUnread;
  const dotColor = isPausedState ? dot : needsAnswer ? statusOf("waiting", false).dot : doneUnread ? "var(--st-unread)" : dot;
  // The newest title, for the rename effect: it must seed the box when `renaming`
  // turns on, but must NOT re-seed on every title change while the user types.
  const titleRef = useRef(t.title);
  titleRef.current = t.title;
  useEffect(() => { if (renaming) setTitle(titleRef.current); }, [renaming]);
  const commit = () => { set({ renaming: null }); if (title.trim() && title !== t.title) void patchTask(t.id, { title }); };
  // In select mode a click picks the chat instead of opening it.
  const click = (e: React.MouseEvent) => {
    if (renaming) return;
    if (selecting || e.metaKey || e.ctrlKey) { set((s) => ({ picked: togglePick(s.picked, t.id) })); return; }
    go("session", { task: t.id });
  };
  return (
    <Pressable
      className={"srow task" + (swarm ? " ultra " + swarm : "") + (active ? " on" : "") + (elsewhere ? " elsewhere" : "") + (picked ? " picked" : "") + (drag.over === t.id ? " dropover" : "") + (drag.dragging === t.id ? " dragging" : "")}
      style={{ ["--srow-accent" as string]: dotColor }}
      data-dragrow={t.id}
      onPointerDown={renaming || selecting ? undefined : drag.press(t.id)}
      onClick={drag.click(click)}
      onContextMenu={onCtx}
      onMouseLeave={() => setArm(false)}
    >
      {selecting && <span className="pickbox" aria-hidden="true">{picked && I.close(9)}</span>}
      <span style={{ width: 12, flex: "none", display: "grid", placeItems: "center", position: "relative" }}>
        <span className={"dot" + (live ? " pulse" : "")} style={{ background: dotColor, boxShadow: live || doneUnread ? `0 0 7px ${dotColor}` : undefined }} />
        {showUnreadMark && <span className="unread-mark" title="Unread" aria-label="Unread chat" />}
      </span>
      {renaming ? (
        <Input className="input rename" autoFocus value={title} onClick={(e) => e.stopPropagation()} onChange={(e) => setTitle(e.currentTarget.value)} onBlur={commit}
          onKeyDown={(e) => { if (e.key === "Enter") commit(); if (e.key === "Escape") { e.stopPropagation(); set({ renaming: null }); } }} />
      ) : (
        <span className="t" style={{ color: titleColor }}>{t.title}</span>
      )}
      {needsAnswer || parked ? (
        <span className={"badge" + (parked ? " quiet" : "")}>
          {/* A parked chat waits on a trigger, not on you; only an explicit
              question or approval gets an actionable label. */}
          {t.waiting_kind === "question" ? "Answer" : t.waiting_kind === "approval" ? "Approve" : "Waiting"}
        </span>
      ) : null}
      {!renaming && (arm ? (
        <Pressable className="confirm" onClick={(e) => { e.stopPropagation(); void archiveTask(t.id); }}>Archive</Pressable>
      ) : (
        <>
          <span className="tm">{t.pinned ? "" : ago(t.updated_at)}</span>
          {t.pinned && <span className="pin">{I.pin(10)}</span>}
          {/* Pin is not destructive, so it toggles straight away — no armed confirm like Archive. */}
          <Tooltip content={t.pinned ? "Unpin" : "Pin to top"}><Pressable className="rowact" onClick={(e) => { e.stopPropagation(); void patchTask(t.id, { pinned: !t.pinned }); }}>{I.pin(12)}</Pressable></Tooltip>
          <Tooltip content="Archive"><Pressable className="rowact" onClick={(e) => { e.stopPropagation(); setArm(true); }}>{I.archive(12)}</Pressable></Tooltip>
        </>
      ))}
    </Pressable>
  );
});

/** The reorder hook's surface, narrowed to the two ids and the handlers a row
 *  needs. See `ui/drag.ts` for why this isn't HTML5 drag and drop. */
type DragApi = Pick<RowDrag, "dragging" | "over" | "press" | "click">;

function CtxMenu({ ctx, close }: { ctx: NonNullable<Ctx>; close: () => void }) {
  const [delArm, setDelArm] = useState(false);
  const [removeWorktree, setRemoveWorktree] = useState(false);
  const pickedN = useStore((s) => Object.keys(s.picked).length);
  const t = ctx.t;
  const x = Math.min(ctx.x, window.innerWidth - 220);
  const y = Math.min(ctx.y, window.innerHeight - 290);
  const run = (f: () => unknown) => () => { close(); void f(); };
  // Ultrathread worker worktrees are this chat's too, so the delete choice has
  // to cover them: a worktree-per-worker run keeps its own checkout in each
  // top-level worker, and the chat's own `worktree` flag is false when the chat
  // itself runs in the project folder. Without them here the toggle never
  // appeared and every worker worktree was left on disk with no way to say
  // otherwise from this menu.
  const workerWts = (t.subs ?? []).filter((s) => s.cwd);
  // What the second click removes, named one piece at a time, because the same
  // flag drives all of it: the chat's own checkout, every worker's checkout, and
  // — for a docker chat — the workspace on disk (`purge_task`). A docker chat
  // can hold worker worktrees too (each is a worktree of its clone), so leaving
  // the workspace out of the text would delete something the user was never told
  // about.
  const several = workerWts.length > 0;
  const bits = [
    t.worktree ? `uncommitted files in ${t.cwd}` : null,
    several ? `${workerWts.length} worker worktree${workerWts.length === 1 ? "" : "s"}` : null,
    t.docker ? "its workspace on disk" : null,
  ].filter((s): s is string => !!s);
  // What Delete says before and after the second click. A chat with no worktree
  // of any kind used to read "Delete chat; keep worktree", promising to keep
  // something it never had.
  const keepWhat = [t.worktree || several ? "worktrees" : null, t.docker ? "workspace" : null].filter(Boolean).join(" and ");
  const keepLabel = keepWhat ? `Delete chat; keep ${keepWhat}` : "Delete chat";
  const delLabel = removeWorktree
    ? t.worktree || several
      ? "Delete chat + worktrees"
      : "Delete chat + workspace"
    : keepLabel;
  // Entering select mode from here also ticks the chat you right-clicked. The
  // mode alone left you in a list with boxes on it and nothing chosen, which is
  // the same as not having selected anything — the menu was the one place the
  // user had already said which chat they meant.
  const startSelecting = () => set((s) => ({ selecting: true, picked: s.picked[t.id] ? s.picked : togglePick(s.picked, t.id) }));
  const items: { icon: React.ReactNode; label: string; k?: string; danger?: boolean; run: () => void }[] = [
    { icon: I.check(12), label: pickedN ? "Add to selection…" : "Select chats…", run: run(startSelecting) },
    { icon: I.close(10), label: pickedN > 1 ? `Message ${pickedN} chats…` : "Message this chat…", run: run(() => set({ mass: true })) },
    { icon: I.pin(12), label: t.pinned ? "Unpin" : "Pin to top", run: run(() => patchTask(t.id, { pinned: !t.pinned })) },
    { icon: I.edit(12), label: "Rename", run: run(() => set({ renaming: t.id })) },
    // Only worth a row when it can do something: a chat the agent is allowed to
    // name has nothing to unlock.
    ...(t.titled ? [{ icon: I.edit(12), label: "Let the agent name it", run: run(() => patchTask(t.id, { untitled: true })) }] : []),
    { icon: I.copy(12), label: "Duplicate", run: run(() => api.duplicate(t.id).then((d) => set((s) => ({ tasks: { ...s.tasks, [d.id]: d } }))).catch((e) => flash(String(e)))) },
    { icon: I.download(12), label: "Export…", run: run(() => exportTasks([t.id])) },
    // Only opens the box; drafting is a second, deliberate click inside it.
    { icon: <Sparkles size={12} strokeWidth={1.7} aria-hidden="true" />, label: "Save as skill…", run: run(() => openSaveAsSkill(t.id)) },
    { icon: <MonitorUp size={12} strokeWidth={1.7} aria-hidden="true" />, label: "Move to…", run: run(() => openMove(t.id)) },
    // Above Archive and Delete, and below Export: it reads the chat rather than
    // changing it, so it belongs with the other "look at this" rows rather than
    // with the ones that alter the sidebar.
    { icon: I.chart(12), label: "Stats…", run: run(() => openChatStats(t.id)) },
    { icon: I.gantt(12), label: "Run timeline…", run: run(() => openRunTimeline(t.id)) },
    ...((t.status === "running" || t.status === "waiting") ? [t.paused
      ? { icon: I.play(10), label: "Resume", run: run(() => api.resume(t.id).catch((e) => flash(String(e)))) }
      : { icon: I.pause(10), label: "Pause", run: run(() => requestPause(t.id)?.catch((e) => flash(String(e)))) }] : []),
    { icon: I.archive(12), label: "Archive", run: run(() => archiveTask(t.id)) },
  ];
  return (
    <>
      <MenuScrim onClick={close} onContextMenu={(e) => { e.preventDefault(); close(); }} />
      <div className="pop ctx" style={{ left: x, top: y, width: 208 }}>
        {items.map((m, i) => (
          <MenuRow key={m.label} style={{ animationDelay: i * 8 + "ms" }} onClick={m.run}>
            <span className="cico">{m.icon}</span><span style={{ flex: 1 }}>{m.label}</span>
          </MenuRow>
        ))}
        <div className="msep" />
        <MenuRow className={delArm ? "danger" : ""} style={{ animationDelay: items.length * 8 + "ms", color: "#ff8a8a" }}
          onClick={() => {
            if (!delArm) setDelArm(true);
            else run(() => deleteTask(t.id, removeWorktree))();
          }}>
          <span className="cico">{I.trash(12)}</span>
          <span style={{ flex: 1 }}>{delArm ? delLabel : "Delete…"}</span>
        </MenuRow>
        {delArm && bits.length > 0 && (
          <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "5px 9px", color: "var(--mut3)", fontSize: 11 }}>
            <Switch label="Also permanently remove these and any uncommitted files in them" checked={removeWorktree} small onChange={setRemoveWorktree} />
            <span>Also remove {bits.join(" and ")}{t.worktree || several ? "; branches remain" : ""}.</span>
          </div>
        )}
      </div>
    </>
  );
}

/**
 * What you can do to the ticked chats, standing in for the section header's
 * "Select" once selection is on.
 *
 * The ticked chats are named by id in the store, so this bar is the only place
 * that has to reconcile a selection with what is actually in the list: a chat
 * deleted on another screen, or archived one at a time, is still ticked until
 * something prunes it. Every action below runs off the pruned set, so a stale
 * tick costs nothing and is never counted in a toast.
 *
 * Archive is the one action in the header, and it arms before it fires — the
 * same two-click bargain a row's own Archive makes, and for the same reason: one
 * mis-press on a small grey word should not empty the sidebar. Everything else
 * is reversible or only reads, so it goes behind the dots.
 */
function SelectionBar() {
  const tasks = useStore((s) => s.tasks);
  const picked = useStore((s) => s.picked);
  // Sorted by recency so a list of ticked chats reads the way the sidebar does.
  const ids = Object.keys(picked)
    .filter((id) => tasks[id] && messageable(tasks[id]))
    .sort((a, b) => (sortKey(tasks[b]!) - sortKey(tasks[a]!)));
  const n = ids.length;
  const pinnedN = ids.filter((id) => tasks[id]!.pinned).length;
  // Whether Archive is waiting on its second click.
  const [arm, setArm] = useState(false);
  const { anchor, toggle, close } = useAnchoredPanel();
  // An arm left over from a selection the user then unticked would greet the
  // next one already armed, so a bar with nothing in it disarms.
  useEffect(() => { if (!n) setArm(false); }, [n]);
  const done = () => { setArm(false); set({ selecting: false, picked: {} }); };
  const run = (f: () => unknown) => () => { setArm(false); close(); void f(); };
  const noun = `${n} chat${n === 1 ? "" : "s"}`;
  return (
    <>
      {n > 0 ? (
        <div style={{ display: "flex", alignItems: "center", gap: 5 }}>
          {arm ? (
            <Pressable className="confirm" onClick={() => { setArm(false); void archivePicked(ids); }}>Archive {n}</Pressable>
          ) : (
            <Tooltip content="Put the ticked chats away · they stay readable in Settings › Chats">
              <button className="shint" style={shint} onClick={() => setArm(true)}>{I.archive(11)} Archive {n}</button>
            </Tooltip>
          )}
          <Tooltip content="More actions for the ticked chats">
            <button className="shint" style={{ ...shint, padding: "0 1px" }} aria-label={`More actions for ${noun}`} aria-expanded={!!anchor} onClick={toggle}><MoreIcon /></button>
          </Tooltip>
          <button className="shint" style={shint} onClick={done}>Done</button>
        </div>
      ) : (
        // Nothing ticked, so there is no action to offer — say what to do
        // instead. A span rather than a disabled button, because the header's
        // controls are the "Select" affordance and a greyed-out row of them
        // reads as a broken control rather than as an empty selection.
        <span style={{ fontSize: 10, color: "var(--mut3)" }}>Tick chats · Esc to cancel</span>
      )}
      <AnchoredPanel anchor={anchor} onClose={close} width={196}>
        {[
          { icon: I.close(10), label: `Message ${noun}…`, run: () => set({ mass: true }) },
          { icon: I.pin(12), label: pinnedN === n ? `Unpin ${noun}` : `Pin ${noun}`, run: () => pinPicked(ids, pinnedN !== n) },
          { icon: I.download(12), label: `Export ${noun}…`, run: () => exportTasks(ids) },
        ].map((m, i) => (
          <MenuRow key={m.label} style={{ animationDelay: i * 8 + "ms" }} onClick={run(m.run)}>
            <span className="cico">{m.icon}</span><span style={{ flex: 1 }}>{m.label}</span>
          </MenuRow>
        ))}
      </AnchoredPanel>
    </>
  );
}

/** The page's own glyph, at the size the other footer buttons draw at. */
function ShortcutIcon({ tab }: { tab: SettingsTab }) {
  const { Icon } = tab;
  return <Icon size={14} strokeWidth={1.7} aria-hidden="true" />;
}

/** Three dots, drawn rather than imported: the header wants the glyph without a
 *  label in a 10px slot, and the whole bar is not worth an icon dependency. */
const MoreIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true" fill="currentColor">
    <circle cx="5" cy="12" r="1.6" /><circle cx="12" cy="12" r="1.6" /><circle cx="19" cy="12" r="1.6" />
  </svg>
);

/** The header's small inline buttons. `.shint` already carries the size and the
 *  show-on-hover rule; these are the inline overrides it was written without,
 *  kept in one place so a second caller cannot drift from the first. */
const shint: CSSProperties = { background: "none", border: 0, padding: 0, cursor: "pointer", display: "flex", alignItems: "center", gap: 3 };

function SidebarBody({ peek }: { peek?: boolean }) {
  const tasks = useStore((s) => s.tasks);
  const cur = useStore((s) => ((s.view === "session" || s.view === "diff") ? s.task : null));
  const view = useStore((s) => s.view);
  // Race attempts are drawn under one header per race (`groupRaceRows`).
  const races = useRaces((s) => s.races);
  const [ctx, setCtx] = useState<Ctx>(null);
  const selecting = useStore((s) => s.selecting);
  // The header's "Chats · N picked" suffix, counted the way the selection bar
  // counts: over chats that still exist and are not archived, so a tick on a
  // chat that has since been archived doesn't promise an action on it.
  //
  // Guarded on `selecting`, the only condition under which the value is read.
  // This selector returns a primitive, so it never triggers a re-render either
  // way — but it walks every chat, and unguarded it did that on every store
  // write while the sidebar was mounted, for a number nothing was reading.
  const selectedN = useStore((s) => s.selecting
    ? Object.values(s.tasks).filter((t) => s.picked[t.id] && !t.archived).length
    : 0);
  const project = useStore((s) => s.settings?.project ?? "");
  const savedN = useStore((s) => s.settings?.saved_prompts?.length ?? 0);
  const showPullRequests = useStore((s) => s.settings?.show_pull_requests ?? false);
  const showSchedules = useStore((s) => s.settings?.show_schedules ?? false);
  const showNeedsYou = useStore((s) => s.settings?.needs_you ?? false);
  // Saved in settings, so closing or reopening the sidebar keeps the choice.
  const allProjects = useStore((s) => s.settings?.all_projects ?? false);
  const pausedAll = useStore((s) => s.settings?.paused_all ?? false);
  // The footer shortcut button follows Settings → Tabs. Subscribed rather
  // than read through `get()`: the label, the glyph and the lit state all come
  // off the page it points at, so changing the choice has to repaint the footer.
  const shortcut = settingsTabDef(useStore((s) => s.settings?.sidebar_button_tab));
  const settingsTab = useStore((s) => s.settingsTab);
  const automaticReorder = useStore((s) => s.settings?.automatic_chat_reorder !== false);
  const currentTask = useStore((s) => (s.view === "session" || s.view === "diff") ? s.task : null);
  // Only the selected project's chats (plus whichever chat is open), unless "show all".
  // Memoized: this filtered and sorted the whole list on every render of the
  // sidebar, which is every task event and the 30s clock tick.
  const { hiddenN, visible, pinned, rest } = useMemo(() => {
    const unarchived = Object.values(tasks).filter((t) => !t.archived);
    const inProject = (t: TaskSummary) => chatInProject(t, project, allProjects, cur);
    const visible = bySortKey(unarchived.filter((t) => !isScheduledChat(t) && inProject(t)), automaticReorder);
    const pinned = visible.filter((t) => t.pinned);
    // Attention first, then this group's own order. Decorate-sort-undecorate for
    // the same reason as `bySortKey`: `compareChats` reads two sort keys per
    // comparison, so calling it from the comparator cost ~n·log n of them.
    // Lower priority wins; within a priority, the newer key wins.
    const rest = visible
      .filter((t) => !t.pinned)
      .map((t) => [chatPriority(t, pausedAll), -sortKey(t, automaticReorder), t] as const)
      .sort((a, b) => a[0] - b[0] || a[1] - b[1])
      .map(([, , t]) => t);
    return {
      hiddenN: unarchived.filter((t) => !inProject(t)).length,
      visible,
      pinned,
      rest,
    };
  }, [tasks, project, allProjects, cur, pausedAll, automaticReorder]);

  // The normal chat cap applies to this project's visible list only. Scheduled
  // chats have their own 100-chat cap, counted across every project.
  const sidebarTasks = useMemo(() => Object.fromEntries(visible.map((task) => [task.id, task])), [visible]);
  const scheduledTasks = useMemo(() => Object.fromEntries(Object.values(tasks).filter((task) => !task.archived && isScheduledChat(task)).map((task) => [task.id, task])), [tasks]);
  const archiveInFlight = useRef(false);
  const scheduledArchiveInFlight = useRef(false);
  useEffect(() => {
    if (archiveInFlight.current) return;
    // The sweep reads the chats' own `unread` marks (`excessSidebarChats`), so a
    // chat waiting to be looked at is never swept away; `sidebarTasks` covers the
    // dependency, because a changed mark arrives as a changed summary.
    const excess = excessSidebarChats(sidebarTasks, SIDEBAR_CHAT_LIMIT, currentTask, pausedAll);
    if (!excess.length) return;
    archiveInFlight.current = true;
    void archiveExcessSidebarChats(excess.map((task) => task.id)).finally(() => { archiveInFlight.current = false; });
  }, [sidebarTasks, currentTask, pausedAll]);
  useEffect(() => {
    if (scheduledArchiveInFlight.current) return;
    const excess = excessSidebarChats(scheduledTasks, SIDEBAR_CHAT_LIMIT, currentTask, pausedAll, true);
    if (!excess.length) return;
    scheduledArchiveInFlight.current = true;
    void archiveExcessSidebarChats(excess.map((task) => task.id)).finally(() => { scheduledArchiveInFlight.current = false; });
  }, [scheduledTasks, currentTask, pausedAll]);

  // Memoized, and so is every row's `onCtx`. These were rebuilt on each render,
  // so they were new props on all 80 chat rows every time the sidebar rendered
  // — which an agent's every status event did, re-rendering the whole list to
  // redraw one row.
  //
  // The drop turns a (dragged, target) pair into a position. `press`/`click`
  // stay referentially stable, which is what keeps the memoized rows from
  // re-rendering on every task event.
  const onDrop = useCallback((dragId: string, targetId: string) => {
    const moving = tasks[dragId];
    const target = tasks[targetId];
    if (!moving || !target || moving.id === target.id) return;
    // Dropping onto a row puts the dragged task just above it, in that row's section.
    const list = (target.pinned ? pinned : rest).filter((x) => x.id !== moving.id);
    const idx = list.findIndex((x) => x.id === target.id);
    void patchTask(moving.id, { order: orderBetween(list[idx - 1], list[idx]), pinned: target.pinned });
  }, [pinned, rest, tasks]);
  const hook = useRowDrag(onDrop);
  const drag: DragApi = useMemo(() => ({ dragging: hook.dragging, over: hook.over, press: hook.press, click: hook.click }), [hook.dragging, hook.over, hook.press, hook.click]);
  // One handler per chat, kept in a ref-keyed cache: a new arrow per row was a
  // new prop on every row, so the list re-rendered whole on any change.
  //
  // The handler must NOT close over the `t` it was created with. The cache makes
  // the *first* render's summary permanent for that id, so a menu built from a
  // captured copy offered stale actions off a stale status: a chat that started
  // running after the first right-click had no Pause row, one that had finished
  // still did, and Delete read `worktree` from whenever the row first appeared.
  // So it reads the live summary through `get()` at click time, which is the
  // identity the cache exists for without the frozen data. A chat that has since
  // gone is not worth a menu, so the click is dropped.
  const ctxHandlers = useRef(new Map<string, (e: React.MouseEvent) => void>());
  const onCtx = (t: TaskSummary) => {
    let fn = ctxHandlers.current.get(t.id);
    if (!fn) {
      const id = t.id;
      fn = (e: React.MouseEvent) => {
        e.preventDefault();
        const live = get().tasks[id];
        if (live) setCtx({ t: live, x: e.clientX, y: e.clientY });
      };
      ctxHandlers.current.set(id, fn);
    }
    return fn;
  };
  const section = (list: TaskSummary[]) => groupRaceRows(list, races).map((e) => e.kind === "task"
    ? <Row key={e.t.id} t={e.t} active={e.t.id === cur} drag={drag} onCtx={onCtx(e.t)} />
    : <RaceGroup key={"race:" + e.race.id} race={e.race}>{e.tasks.map((t) => <Row key={t.id} t={t} active={t.id === cur} drag={drag} onCtx={onCtx(t)} />)}</RaceGroup>);
  return (
    <div className={"sidebar-in" + (peek ? " peek" : "")}>
      <div style={{ padding: "2px 6px 4px 0" }}>
        <Pressable className={"srow" + (view === "home" ? " on" : "")} style={{ fontWeight: 500, color: "#e4e4e7" }} onClick={() => go("home")}>
          {I.newTask()}<span className="t">New task</span><span className="tm">Ctrl N</span>
        </Pressable>
        {showNeedsYou && <NeedsYou />}
        {savedN > 0 && (
          <Pressable className={"srow" + (view === "saved" ? " on" : "")} style={{ color: "var(--mut)" }} onClick={() => go("saved")}>
            {I.bookmark()}<span className="t">Saved prompts</span><span className="tm">{savedN}</span>
          </Pressable>
        )}
        {showPullRequests && <Pressable className={"srow" + (view === "pulls" ? " on" : "")} style={{ color: "var(--mut)" }} onClick={() => go("pulls")}>
          {I.branch()}<span className="t">Pull requests</span>
        </Pressable>}
        {showSchedules && <Pressable className={"srow" + (view === "schedules" ? " on" : "")} style={{ color: "var(--mut)" }} onClick={() => go("schedules")}>
          {I.clock()}<span className="t">Schedules</span>
        </Pressable>}
      </div>
      {/* Both sections live inside this one element, so a row dragged out of
          "Chats" and into the pinned block still hit-tests against the same
          measured set. */}
      <div ref={hook.listRef} style={{ flex: 1, overflowY: "auto", overflowX: "hidden", padding: "6px 6px 8px 0" }}>
        {pinned.length > 0 && (
          <>
            <div className="shead"><span>Pinned</span><span>{pinned.length}</span></div>
            {section(pinned)}
          </>
        )}

        <div className="shead" style={{ paddingTop: pinned.length ? 14 : 8 }}>
          <span>Chats{selecting && selectedN > 0 && <span style={{ color: "var(--mut3)" }}> · {selectedN} picked</span>}</span>
          {selecting ? (
            <SelectionBar />
          ) : (
            <Tooltip content="Tick several chats to archive, export or message them all at once">
              <button className="shint" style={{ background: "none", border: 0, cursor: "pointer", display: "flex", alignItems: "center", gap: 3 }} onClick={() => set({ selecting: true })}>
                {I.check(11)} Select
              </button>
            </Tooltip>
          )}
        </div>

        {section(rest)}
        {visible.length === 0 && <div style={{ padding: "4px 9px", color: "var(--dim)" }}>No tasks yet</div>}
      </div>
      {project && (hiddenN > 0 || allProjects) && (
        <Tooltip content={allProjects ? "Show only this project's chats" : "Chats from other project folders are hidden"}>
          <Pressable className={"projfilter" + (allProjects ? " on" : "")} aria-pressed={allProjects} onClick={() => void saveSettings({ all_projects: !allProjects })}>
            {I.folder()}
            <span className="t">{allProjects ? "Only this project" : `${hiddenN} in other project${hiddenN === 1 ? "" : "s"} · show`}</span>
          </Pressable>
        </Tooltip>
      )}
      <div style={{ flex: "none", display: "flex", alignItems: "center", gap: 2, padding: "6px 6px 2px 0", borderTop: "1px solid rgba(255,255,255,0.04)" }}>
        <div style={{ flex: 1, minWidth: 0, position: "relative", display: "flex", alignItems: "center", padding: "0 7px" }}>
          <UsageBar />
        </div>
        <IconButton label={shortcut.label} className={view === "settings" && settingsTab === shortcut.id ? "on" : ""} onClick={openSidebarShortcut}><ShortcutIcon tab={shortcut} /></IconButton>
        <IconButton label="Settings · Ctrl ," className={view === "settings" ? "on" : ""} onClick={() => go("settings")}>{I.gear()}</IconButton>
      </div>
      {ctx && <CtxMenu ctx={ctx} close={() => setCtx(null)} />}
    </div>
  );
}

export function Sidebar() {
  // While a chat is on its Artificts surface the rail is forced collapsed rather
  // than genuinely closed: the user's own preference is left alone in the store,
  // so it comes back the moment that chat's surface closes, and the closed state
  // is already the one that keeps the left-edge peek available.
  const sidebarOpen = useStore((s) => s.sidebar);
  // Only while that chat is actually on screen: `task` outlives a trip to the
  // new-task screen, and keying on it alone left the rail stuck collapsed after
  // pressing New chat from a studio.
  const artifictMode = useStore((s) => (s.view === "session" && s.task ? s.tasks[s.task]?.artifict_open ?? false : s.view === "home" && !!s.home.artificts));
  const open = sidebarOpen && !artifictMode;
  const [peek, setPeek] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  const hold = (on: boolean, delay: number) => { clearTimeout(timer.current); timer.current = window.setTimeout(() => setPeek(on), delay); };
  useEffect(() => { if (open) setPeek(false); }, [open]);
  // On a phone the sidebar is a drawer over the content (App.css), so picking a
  // chat or a screen from it closes it — otherwise every tap leaves the user
  // looking at the list they just chose from instead of what they chose.
  const view = useStore((s) => s.view);
  const task = useStore((s) => s.task);
  useEffect(() => { if (isNarrow() && get().sidebar) set({ sidebar: false }); }, [view, task]);
  return (
    <>
      <aside className={"sidebar" + (open ? " open" : "")} style={{ width: open ? 246 : 0 }}>
        <div style={{ opacity: open ? 1 : 0, height: "100%", transition: "opacity .18s ease" }}>{open && <SidebarBody />}</div>
      </aside>
      {/* Only shown on a phone, where the open sidebar covers the content: a tap
          beside the drawer closes it. */}
      {open && <div className="sidebar-scrim" aria-hidden="true" onClick={() => set({ sidebar: false })} />}
      {!open && (
        <>
          {/* Hover the left edge to peek at the sidebar without opening it. */}
          <div className="peekzone" onMouseEnter={() => hold(true, 90)} onMouseLeave={() => hold(false, 350)} />
          <div className={"peekpanel" + (peek ? " show" : "")} onMouseEnter={() => hold(true, 0)} onMouseLeave={() => hold(false, 250)}>
            {peek && <SidebarBody peek />}
          </div>
        </>
      )}
    </>
  );
}

/** Sidebar footer bar: one segment per chosen subscription showing how much is left; spend if none. */
/** Subscription meters keep fixed brand colors in every theme. */
const USAGE_COLOR: Record<string, string> = { claude: "#ff8a3d", codex: "#3da9ff", "opencode-go": "#fbbf24" };

function UsageBar() {
  const providers = shownProviders(useStore((s) => s.providers));
  const settings = useStore((s) => s.settings);
  const accts = shownAccounts(useStore((s) => s.accounts));
  const [menu, setMenu] = useState(false);
  const shown = settings?.home_usage ?? providers.filter((p) => p.account).map((p) => p.id);
  const kinds = providers.filter((p) => p.account && accts.some((a) => a.kind === p.id));
  const meters = kinds.map((p) => {
    const list = accts.filter((a) => a.kind === p.id);
    const w = netWindows(list)[0];
    const avail = list.filter((a) => a.available).length;
    // Caps are the user's own doing, so the footer says so where it is visible
    // from every screen: a segment whose accounts are all held by caps is drawn
    // amber, the way the lines in Settings → Accounts are, and one that is only
    // partly capped says how many in its tooltip.
    const capped = list.filter((a) => capsOf(a).some((c) => c.holding)).length;
    // Routable is not the same as measured: a missing reading must never look
    // like a fresh, full allowance. Unavailable accounts still show 0% left.
    const left = !avail ? 0 : w ? Math.max(0, 100 - w.used) : null;
    const text = left === null ? "Usage unavailable" : `${Math.round(left)}% left`;
    return { p, left, text, avail, n: list.length, label: w?.label ?? "", capped, on: shown.includes(p.id) };
  });
  const segs = meters.filter((m) => m.on);
  const toggle = (k: string) => saveSettings({ home_usage: shown.includes(k) ? shown.filter((x) => x !== k) : [...shown, k] });
  return (
    <>
      <Tooltip content={segs.length ? segs.map((s) => `${s.p.account!.short_name}: ${s.text}${s.label ? ` (${s.label})` : ""} · ${s.avail}/${s.n} accounts${s.capped ? ` · ${s.capped} held by a cap` : ""}`).join(" · ") + " · click to choose" : accts.length ? "No accounts in this bar · click to choose" : "No subscription accounts"}><Pressable className="sbar" aria-label="Usage in this bar" aria-disabled={!accts.length} onClick={(e) => { if (accts.length) { e.stopPropagation(); setMenu(!menu); } }}>
        {/* Empty renders nothing rather than a "No usage to show" line: the bar
            is a footer affordance, and a dead label there reads as an error.
            The click target stays because it is also the only way back into the
            picker when every account has been toggled off. */}
        {/* Hidden segments stay mounted and collapse to nothing (App.css .useg.off),
            so toggling one in the menu slides the bar apart instead of snapping. */}
        {meters.map((s) => (
          <div key={s.p.id} aria-hidden={!s.on || undefined} className={"useg" + (s.on ? "" : " off") + (s.capped > 0 && s.capped === s.n ? " capped" : "")}><div style={{ width: (s.left ?? 0) + "%", background: USAGE_COLOR[s.p.id] ?? "#33d6ff" }} /></div>
        ))}
      </Pressable></Tooltip>
      {menu && (
        <>
          <MenuScrim onClick={(e) => { e.stopPropagation(); setMenu(false); }} />
          <div className="pop umenu" onClick={(e) => e.stopPropagation()}>
            <div className="mhead">Show in this bar</div>
            {kinds.map((p) => {
              const sg = segs.find((x) => x.p.id === p.id);
              return (
                <MenuRow key={p.id} onClick={() => toggle(p.id)}>
                  <ProviderIcon provider={p.id} size={14} />
                  <span style={{ flex: 1 }}>{p.account!.display_name}</span>
                  {sg && <span className="umenu-left">{sg.text}</span>}
                  <Switch label={`Show ${p.account!.display_name}`} checked={shown.includes(p.id)} small />
                </MenuRow>
              );
            })}
            <div className="msep" />
            <MenuRow onClick={() => { setMenu(false); go("settings", { settingsTab: "accounts" }); }}><span style={{ flex: 1 }}>Accounts…</span></MenuRow>
          </div>
        </>
      )}
    </>
  );
}
