import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { createPortal } from "react-dom";
import { Check, FolderOpen } from "lucide-react";
import { ago, api, baseName, normPath, TaskSummary } from "../api";
import { back, chatModel, flash, forgetTask, forward, get, go, loadProject, modelColor, openProject, saveSettings, set, shownAccounts, shownProviders, type State, useNow, useStore, zoom } from "../store";
import { netWindows } from "./Accounts";
import { I, statusOf } from "./icons";
import { openChatStats } from "./ChatStats";
import { useRowDrag, type RowDrag } from "./drag";
import { pickFolder } from "./Home";
import { isPaused, heldByGlobal } from "./Paused";
import { ProviderIcon } from "./ProviderIcon";
import { swarmState } from "./session/Swarm";
import { NeedsYou } from "./NeedsYou";
import { AnchoredPanel, Tooltip, IconButton, Input, MenuRow, MenuScrim, ctxPlace, menuLayer, Pressable, Switch, useAnchoredPanel, useEscapeClose, WindowButton, Kbd } from "./primitives";

const win = () => getCurrentWindow();

export const openAccountsSettings = () => go("settings", { settingsTab: "accounts" });

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
          <span className="mm-name">{baseName(path)}</span>
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
  const list = projectMenuEntries(project, projects ?? []);
  const [pos, setPos] = useState<{ left: number; top: number; width: number; maxHeight: number } | null>(null);

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
  }, [openMenuState, z, list.length]);
  useEffect(() => {
    if (!openMenuState) return;
    const remeasure = () => window.dispatchEvent(new Event("resize"));
    window.addEventListener("resize", remeasure);
    return () => window.removeEventListener("resize", remeasure);
  }, [openMenuState]);
  useEffect(() => { if (openMenuState) setHi(Math.max(0, list.indexOf(project))); }, [openMenuState, project]);
  useEffect(() => {
    if (!openMenuState) return;
    pop.current?.querySelector(".hi")?.scrollIntoView({ block: "nearest" });
  }, [hi, openMenuState]);

  // Escape closes the list and nothing else: the app's global handler would pause a running agent.
  // A right-click menu is open above the list, so it goes first — otherwise the list closes under it
  // and the menu is left floating on its own.
  useEscapeClose(!!openMenuState, () => { if (ctx) setCtx(null); else setOpen(false); });

  const pick = (p: string) => { setOpen(false); if (p && p !== project) void openProject(p); };
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
          <span className="pv">{project ? baseName(project) : "No project"}</span>
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
                    <div style={{ fontWeight: 500, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{baseName(p)}</div>
                    <div style={{ fontSize: 11, color: "var(--mut3)", lineHeight: 1.4, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{p}</div>
                  </div>
                  <span style={{ width: 12, flex: "none", color: "var(--violet)", display: "flex" }}>{p === project ? <Check size={12} strokeWidth={2} /> : null}</span>
                </MenuRow>
              ))}
              {!list.length && <div style={{ padding: "2px 8px 6px", fontSize: 11.5, color: "var(--dim)" }}>No project yet</div>}
              <div className="msep" />
              <MenuRow onClick={openFolder}><span className="cico"><FolderOpen size={13} /></span><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Open folder…</div><div style={{ fontSize: 11, color: "var(--mut3)" }}>Add another project · Ctrl O</div></div></MenuRow>
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
        <span className="cp-name">{baseName(path)}</span>
        {!same && <span className="cp-flag">here</span>}
      </div>
    </Tooltip>
  );
}

export function Titlebar() {
  const canBack = useStore((s) => s.hist.length > 0);
  const canFwd = useStore((s) => s.fwd.length > 0);
  return (
    <div className="titlebar" data-tauri-drag-region>
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
      <div style={{ display: "flex", height: 40 }}>
        <WindowButton label="Minimize" onClick={() => win().minimize()}><div style={{ width: 10, height: 1, background: "currentColor" }} /></WindowButton>
        <WindowButton label="Maximize or restore" onClick={() => win().toggleMaximize()}><div style={{ width: 9, height: 9, border: "1px solid currentColor", borderRadius: 1.5 }} /></WindowButton>
        <WindowButton label="Close window" close onClick={() => win().close()}>
          <div style={{ position: "relative", width: 10, height: 10 }}>
            <div style={{ position: "absolute", left: 4.5, top: -1, width: 1, height: 12, background: "currentColor", transform: "rotate(45deg)" }} />
            <div style={{ position: "absolute", left: 4.5, top: -1, width: 1, height: 12, background: "currentColor", transform: "rotate(-45deg)" }} />
          </div>
        </WindowButton>
      </div>
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
  if (t.status === "running") return 2;
  if (t.status === "done" || t.status === "stopped") return 3;
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
}

export async function deleteTask(id: string, removeWorktree = false) {
  const task = get().tasks[id];
  const result = await api.remove(id, removeWorktree).then(() => true).catch((e) => {
    flash(String(e));
    return false;
  });
  if (!result) return false;
  set((s) => forgetTask(s, id));
  if (task && id === get().task && (get().view === "session" || get().view === "diff")) go("home");
  if (removeWorktree) flash("Chat deleted · worktree removed · branch kept");
  else if (task?.worktree) flash(`Chat deleted · worktree kept at ${task.cwd} (${task.branch})`);
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
  flash(n ? `Archived ${n} chat${n === 1 ? "" : "s"}` : "Those chats are already archived");
}

/** Pin or unpin every ticked chat in one write each, keeping the ticks: pinning
 *  is not destructive and the user is likely to do more than one thing to a
 *  selection. Reports its own failures through `patchTask`. */
async function pinPicked(ids: string[], pinned: boolean) {
  await Promise.all(ids.map((id) => patchTask(id, { pinned })));
}

/** Delete every archived chat at once. Their worktrees are kept on disk. */
export async function deleteArchivedTasks() {
  const before = get();
  const ids = Object.values(before.tasks).filter((task) => task.archived).map((task) => task.id);
  const keptWorktrees = ids.filter((id) => !!before.tasks[id]?.worktree).length;
  const deleted: string[] = [];
  for (const id of ids) {
    const success = await api.remove(id, false).then(() => true).catch((e) => {
      flash(String(e));
      return false;
    });
    if (success) deleted.push(id);
  }
  if (deleted.length) set((s) => {
    let next = s;
    for (const id of deleted) next = { ...next, ...forgetTask(next, id) } as State;
    return next === s ? {} : (next as unknown as Partial<State>);
  });
  if (before.task && deleted.includes(before.task) && (before.view === "session" || before.view === "diff")) go("home");
  const n = deleted.length;
  flash(
    n
      ? `Deleted ${n} archived chat${n === 1 ? "" : "s"}${keptWorktrees ? ` · kept ${keptWorktrees} worktree${keptWorktrees === 1 ? "" : "s"} on disk` : ""}`
      : ids.length ? "Archived chats could not be deleted" : "No archived chats",
  );
}

/** How long a chat has to sit untouched before a sweep will put it away. */
export const ARCHIVE_AGES = [7, 14, 30, 90, 180] as const;

/** Two weeks, the window a sweep opens at: long enough that a chat you return
 *  to after a fortnight is still in the sidebar, short enough that the list does
 *  not become an archive of everything you have ever opened. */
export const DEFAULT_ARCHIVE_AGE = 14;
export const SIDEBAR_CHAT_LIMIT = 100;

export const ageLabel = (days: number) => (days >= 365 ? `${Math.round(days / 365)} years` : days >= 30 ? (days % 30 ? `${days} days` : `${days / 30} month${days / 30 === 1 ? "" : "s"}`) : `${days} day${days === 1 ? "" : "s"}`);

/// Chats a sweep is allowed to put away: the live ones, and the ones the user
/// last *did* something in.
///
/// Archived chats are already out of the way, and a running or waiting one has
/// a live agent in it — `tasks_archive` deliberately hides a chat rather than
/// cancelling it, so sweeping one away would leave work running somewhere the
/// user cannot see. Age comes off `touched_at`, the field the sidebar sorts on:
/// `updated_at` moves on every agent step, so a chat working through a hundred
/// tool calls looks brand new to it and would never age out.
///
/// Exported for tests.
export function staleChats(tasks: Record<string, TaskSummary>, days: number, now = Date.now(), unread: Record<string, boolean> = {}, current: string | null = null, pausedAll = false): TaskSummary[] {
  const cutoff = now - days * 86_400_000;
  return Object.values(tasks).filter((t) => !t.archived && !t.pinned && t.id !== current && !unread[t.id]
    && !isPaused(t, pausedAll) && !["running", "waiting"].includes(t.status)
    && Date.parse(t.touched_at || t.updated_at) < cutoff);
}

/** Oldest eligible chats beyond the sidebar cap, preserving pinned, active, paused, unread and current chats. */
export function excessSidebarChats(tasks: Record<string, TaskSummary>, limit = SIDEBAR_CHAT_LIMIT, unread: Record<string, boolean> = {}, current: string | null = null, pausedAll = false): TaskSummary[] {
  const unarchived = Object.values(tasks).filter((t) => !t.archived);
  const excess = Math.max(0, unarchived.length - limit);
  if (!excess) return [];
  const eligible = unarchived.filter((t) => !t.pinned && t.id !== current && !unread[t.id]
    && !isPaused(t, pausedAll) && !["running", "waiting"].includes(t.status))
    .sort((a, b) => sortKey(a) - sortKey(b));
  return eligible.slice(0, excess);
}

/** Put every chat idle for longer than `days` away. Reports through `flash`. */
export async function archiveStale(days: number) {
  const state = get();
  const current = state.view === "session" || state.view === "diff" ? state.task : null;
  const ids = staleChats(state.tasks, days, Date.now(), state.unread, current, state.settings?.paused_all ?? false).map((t) => t.id);
  if (!ids.length) return flash("No chats idle that long");
  return archivePicked(ids);
}

export async function archiveExcessSidebarChats(ids: string[]) {
  if (!ids.length) return;
  try {
    const n = await api.archiveTasks(ids);
    if (!n) return;
    set((s) => ({ tasks: Object.fromEntries(Object.entries(s.tasks).map(([id, task]) => [id, ids.includes(id) ? { ...task, archived: true, pinned: false } : task])) }));
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
    const list = await api.importTasks(path);
    set((s) => ({ tasks: { ...s.tasks, ...Object.fromEntries(list.map((t) => [t.id, t])) } }));
    flash(`Imported ${list.length} chat${list.length === 1 ? "" : "s"}`);
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
  const unread = useStore((s) => s.unread[t.id]);
  const renaming = useStore((s) => s.renaming === t.id);
  const picked = useStore((s) => !!s.picked[t.id]);
  const selecting = useStore((s) => s.selecting);
  const project = useStore((s) => s.settings?.project ?? "");
  const [arm, setArm] = useState(false);
  const [title, setTitle] = useState(t.title);
  const st = statusOf(t.status, isPaused(t, pausedAll));
  const live = (t.status === "running" || t.status === "waiting") && st.text !== "Paused";
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
  // The dot and the active rail take one colour from here, so they can never
  // disagree: unread is the loudest, then the chat's own status colour. A
  // blocking question or approval is the exception: its red waiting colour is
  // the attention marker, and unread blue would hide the fact that it needs an
  // answer until the backend ends the wait.
  const needsAnswer = dot === "var(--st-wait)" && (t.waiting_kind === "question" || t.waiting_kind === "approval");
  const dotColor = unread && !needsAnswer ? "var(--st-unread)" : dot;
  useEffect(() => { if (renaming) setTitle(t.title); }, [renaming]);
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
      <span style={{ width: 12, flex: "none", display: "grid", placeItems: "center" }}>
        <span className={"dot" + (live ? " pulse" : "")} style={{ background: dotColor, boxShadow: live || unread ? `0 0 7px ${dotColor}` : undefined }} />
      </span>
      {renaming ? (
        <Input className="input rename" autoFocus value={title} onClick={(e) => e.stopPropagation()} onChange={(e) => setTitle(e.currentTarget.value)} onBlur={commit}
          onKeyDown={(e) => { if (e.key === "Enter") commit(); if (e.key === "Escape") { e.stopPropagation(); set({ renaming: null }); } }} />
      ) : (
        <span className="t" style={{ color: titleColor }}>{t.title}</span>
      )}
      {t.status === "waiting" && !arm && (
        <span className="badge">
          {/* A parked chat ("sleep until X") is not waiting *on you*, so an
              "Answer"/"Approve" badge would be a lie — it wakes itself. */}
          {t.waiting_kind === "question" ? "Answer" : t.waiting_kind === "wake" ? "Waiting" : "Approve"}
        </span>
      )}
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
    // Above Archive and Delete, and below Export: it reads the chat rather than
    // changing it, so it belongs with the other "look at this" rows rather than
    // with the ones that alter the sidebar.
    { icon: I.chart(12), label: "Stats…", run: run(() => openChatStats(t.id)) },
    ...((t.status === "running" || t.status === "waiting") ? [t.paused
      ? { icon: I.play(10), label: "Resume", run: run(() => api.resume(t.id).catch((e) => flash(String(e)))) }
      : { icon: I.pause(10), label: "Pause", run: run(() => api.pause(t.id).catch((e) => flash(String(e)))) }] : []),
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
          <span style={{ flex: 1 }}>{delArm ? (removeWorktree ? "Delete chat + worktree" : "Delete chat; keep worktree") : "Delete…"}</span>
        </MenuRow>
        {delArm && t.worktree && (
          <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "5px 9px", color: "var(--mut3)", fontSize: 11 }}>
            <Switch label="Also permanently remove the worktree and its uncommitted files" checked={removeWorktree} small onChange={setRemoveWorktree} />
            <span>Also remove uncommitted files in {t.cwd}; branch {t.branch} will remain.</span>
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
  const [ctx, setCtx] = useState<Ctx>(null);
  const selecting = useStore((s) => s.selecting);
  // The header's "Chats · N picked" suffix, counted the way the selection bar
  // counts: over chats that still exist and are not archived, so a tick on a
  // chat that has since been archived doesn't promise an action on it.
  const selectedN = useStore((s) => Object.values(s.tasks).filter((t) => s.picked[t.id] && !t.archived).length);
  const project = useStore((s) => s.settings?.project ?? "");
  const savedN = useStore((s) => s.settings?.saved_prompts?.length ?? 0);
  const showNeedsYou = useStore((s) => s.settings?.needs_you ?? false);
  // Saved in settings, so closing or reopening the sidebar keeps the choice.
  const allProjects = useStore((s) => s.settings?.all_projects ?? false);
  const pausedAll = useStore((s) => s.settings?.paused_all ?? false);
  const automaticReorder = useStore((s) => s.settings?.automatic_chat_reorder !== false);
  const unread = useStore((s) => s.unread);
  const currentTask = useStore((s) => (s.view === "session" || s.view === "diff") ? s.task : null);
  // Only the selected project's chats (plus whichever chat is open), unless "show all".
  // Memoized: this filtered and sorted the whole list on every render of the
  // sidebar, which is every task event and the 30s clock tick.
  const { hiddenN, visible, pinned, rest } = useMemo(() => {
    const unarchived = Object.values(tasks).filter((t) => !t.archived);
    const inProject = (t: TaskSummary) => chatInProject(t, project, allProjects, cur);
    const visible = unarchived.filter(inProject).sort((a, b) => sortKey(b, automaticReorder) - sortKey(a, automaticReorder));
    const pinned = visible.filter((t) => t.pinned);
    const rest = visible.filter((t) => !t.pinned).sort((a, b) => compareChats(a, b, pausedAll, automaticReorder));
    return {
      hiddenN: unarchived.filter((t) => !inProject(t)).length,
      visible,
      pinned,
      rest,
    };
  }, [tasks, project, allProjects, cur, pausedAll, automaticReorder]);

  // The sidebar cap applies to chats in this project's visible list only. Chats
  // from other folders stay in the project's own list and are never auto-archived here.
  const sidebarTasks = useMemo(() => Object.fromEntries(visible.map((task) => [task.id, task])), [visible]);
  const archiveInFlight = useRef(false);
  useEffect(() => {
    if (archiveInFlight.current) return;
    const excess = excessSidebarChats(sidebarTasks, SIDEBAR_CHAT_LIMIT, unread, currentTask, pausedAll);
    if (!excess.length) return;
    archiveInFlight.current = true;
    void archiveExcessSidebarChats(excess.map((task) => task.id)).finally(() => { archiveInFlight.current = false; });
  }, [sidebarTasks, unread, currentTask, pausedAll]);

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
  const ctxHandlers = useRef(new Map<string, (e: React.MouseEvent) => void>());
  const onCtx = (t: TaskSummary) => {
    let fn = ctxHandlers.current.get(t.id);
    if (!fn) {
      fn = (e: React.MouseEvent) => { e.preventDefault(); setCtx({ t, x: e.clientX, y: e.clientY }); };
      ctxHandlers.current.set(t.id, fn);
    }
    return fn;
  };
  const section = (list: TaskSummary[]) => list.map((t) => (
    <Row key={t.id} t={t} active={t.id === cur} drag={drag} onCtx={onCtx(t)} />
  ));
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
        <IconButton label="Accounts" className={view === "settings" && get().settingsTab === "accounts" ? "on" : ""} onClick={openAccountsSettings}>{I.key(14)}</IconButton>
        <IconButton label="Settings · Ctrl ," className={view === "settings" ? "on" : ""} onClick={() => go("settings")}>{I.gear()}</IconButton>
      </div>
      {ctx && <CtxMenu ctx={ctx} close={() => setCtx(null)} />}
    </div>
  );
}

export function Sidebar() {
  const open = useStore((s) => s.sidebar);
  const [peek, setPeek] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  const hold = (on: boolean, delay: number) => { clearTimeout(timer.current); timer.current = window.setTimeout(() => setPeek(on), delay); };
  useEffect(() => { if (open) setPeek(false); }, [open]);
  return (
    <>
      <aside className="sidebar" style={{ width: open ? 246 : 0 }}>
        <div style={{ opacity: open ? 1 : 0, height: "100%", transition: "opacity .18s ease" }}>{open && <SidebarBody />}</div>
      </aside>
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
  const segs = kinds.filter((p) => shown.includes(p.id)).map((p) => {
    const list = accts.filter((a) => a.kind === p.id);
    const w = netWindows(list)[0];
    const avail = list.filter((a) => a.available).length;
    return { p, left: avail ? Math.max(0, 100 - (w?.used ?? 0)) : 0, avail, n: list.length, label: w?.label ?? "" };
  });
  const toggle = (k: string) => saveSettings({ home_usage: shown.includes(k) ? shown.filter((x) => x !== k) : [...shown, k] });
  return (
    <>
      <Tooltip content={segs.length ? segs.map((s) => `${s.p.account!.short_name}: ${Math.round(s.left)}% left${s.label ? ` (${s.label})` : ""} · ${s.avail}/${s.n} accounts`).join(" · ") + " · click to choose" : accts.length ? "No accounts in this bar · click to choose" : "No subscription accounts"}><Pressable className="sbar" aria-label="Usage in this bar" aria-disabled={!accts.length} onClick={(e) => { if (accts.length) { e.stopPropagation(); setMenu(!menu); } }}>
        {/* Empty renders nothing rather than a "No usage to show" line: the bar
            is a footer affordance, and a dead label there reads as an error.
            The click target stays because it is also the only way back into the
            picker when every account has been toggled off. */}
        {segs.map((s) => (
          <div key={s.p.id} className="useg" style={{ flex: 1 }}><div style={{ width: s.left + "%", background: USAGE_COLOR[s.p.id] ?? "#33d6ff" }} /></div>
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
                  {sg && <span style={{ fontSize: 11, color: "var(--mut3)" }}>{Math.round(sg.left)}% left</span>}
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
