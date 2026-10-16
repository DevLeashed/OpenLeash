// App state: a tiny external store fed by IPC calls and live backend events.
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { createToastLedger, describe as attentionText, notify, surfaces, type Attention, type Needs } from "./notify";import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { defaultMode } from "./workspaces";
import { isRemoteClient, type SshHostView } from "./remote";
import { AccountView, AgentDef, AppUpdateStatus, api, Assist, baseName, Bg, Boot, FileRef, Item, ModelInfo, normPath, Perm, ProviderPreset, ProviderView, SavedPrompt, Settings, SkillDef, SubInfo, TaskSummary, UltraX, withRequiredAgents, WslDistro, xOn } from "./api";

export type View = "home" | "session" | "diff" | "saved" | "pulls" | "paused" | "settings";
export type Menu = null | "plus" | "perm" | "branch" | "folder" | "model" | "assist" | "agents" | "route";

export interface State {
  ready: boolean;
  bootError: string | null;
  appUpdate: AppUpdateStatus;
  appUpdateDismissed: string | null;
  appUpdateBusy: boolean;
  appUpdateActionError: string | null;
  settings: Settings | null;
  providers: ProviderView[];
  providerPresets: ProviderPreset[];
  models: ModelInfo[];
  shell: string;
  dataDir: string;
  monthSpend: number;
  /** Whether this install opted into the Claude subscription integration by
   *  putting a file named `ALLOW_CLAUDE` beside the executable. Decided by the
   *  backend at boot and never editable from the app: it gates what the UI
   *  offers, and `add_account` refuses the import anyway. */
  claudeAllowed: boolean;
  tasks: Record<string, TaskSummary>;
  items: Record<string, Item[]>;
  /** Session messages shown immediately while task_send is completing. */
  pendingMessages: Record<string, Item[]>;
  /** task id → sub id → transcript */
  subItems: Record<string, Record<string, Item[]>>;
  accounts: AccountView[];
  agents: AgentDef[];
  skills: SkillDef[];
  /** Subagent whose transcript drawer is open. */
  subOpen: string | null;
  /** Task whose sidebar title is being edited. */
  renaming: string | null;
  /** Model picker target: the task's main agent, or one subagent. */
  pickFor: { sub: string } | null;
  bg: Record<string, Bg[]>;
  /** task id → artifict id → the version id most recently published for it.
   *  Bumped by the backend on every artifict write (from the agent's own tools
   *  as well as the workspace IPC), so an open Artificts view reloads a
   *  revision the moment the agent makes it instead of waiting for a manual
   *  Refresh. Only ids are held: the content is always re-read through
   *  `artifict_get`, so a stale preview is never rendered from an event. */
  artificts: Record<string, Record<string, string>>;
  view: View;
  hist: { view: View; task: string | null }[];
  /** Screens popped off `hist` by Back, newest last. Forward replays them and
   *  clears itself, so a fresh navigation never leaves a stale Forward armed. */
  fwd: { view: View; task: string | null }[];
  task: string | null;
  settingsTab: string;
  /** Open the provider setup dialog after navigating to Settings, Models. */
  modelSetupOpen: boolean;
  /** ULTRATHREAD X ladder editor. */
  ultraXOpen: boolean;
  draft: string;
  sessionDrafts: Record<string, string>;
  /** Images waiting to send, keyed by task id or "new-chat". */
  attach: Record<string, string[]>;
  /** Files attached by path, same keys. Image metadata is frontend-only: it
   *  links a path to its thumbnail, including when two images have equal bytes. */
  attachFiles: Record<string, (FileRef & { imageDataUrl?: string; imageIndex?: number })[]>;
  // composer options for new tasks
  home: { plan: boolean; ultra: boolean; ultra_wt: boolean; ultra_x: UltraX | null; perm: Perm; model: string; effort: number; worktree: boolean; docker: boolean; branch: string; subagents: boolean; assist: Assist; agents: string[]; route: string };
  projectInfo: { path: string; exists: boolean; git: boolean; branch: string; branches: string[]; memory: string[]; docs: string[]; disabled_docs: string[]; trusted: boolean; decided: boolean } | null;
  sidebar: boolean;
  details: boolean;
  pal: boolean;
  /** Find-in-this-chat bar. */
  find: boolean;
  /** Term find opens with already in the box, so a click that carried a word
   *  (a swarm-tree row, a search result) doesn't have to be retyped. */
  findSeed: string;
  /** Batch model swap dialog. */
  swap: boolean;
  /** Notes the user put on selected text in a chat, keyed by task id. They live
   *  in the store rather than in the pill that made them: the pill is gone the
   *  moment the note is saved, and these have to ride along in the composer,
   *  survive a chat switch and be re-read after a restart. */
  notes: Record<string, Note[]>;
  menu: Menu;
  menuAnchor: DOMRect | null;
  /** Folder whose right-click menu is open, and where. */
  projCtx: { p: string; x: number; y: number } | null;
  toast: string | null;
  /** The one action the current toast offers, if any. Cleared with the toast. */
  toastAction: ToastAction | null;
  /** Why a chat's transcript could not be loaded, and which chat it was.
   *
   *  Paired with the id on purpose: one chat at a time is open, but a failure
   *  for the chat the user just left must not be shown against the one they are
   *  looking at now. Session renders this instead of a bare "Task not found". */
  openError: { id: string; why: string } | null;
  /** Set when loading a project's agents/skills catalog fails, so an empty
   *  panel can be told apart from "none configured". Cleared on the next success. */
  catalogError: string | null;
  /** In-app attention notices, newest last. Only raised while the window is focused. */
  notices: Notice[];
  /** Chats ticked for a mass message. */
  picked: Record<string, true>;
  /** Select mode: rows show their tick box and a click picks instead of opening. */
  selecting: boolean;
  /** Mass message composer dialog. */
  mass: boolean;
  /** The mass message is the "Message all chats" one — every agent working
   *  right now — rather than the sidebar's send to the ticked chats. */
  massAll: boolean;
  /** Fork box: the chat being forked, and nothing else — the mode is picked inside it. */
  fork: string | null;
  /** "Send to another machine" box: the chat being sent, or null. */
  transfer: string | null;
  /** Per-chat stats panel: the chat whose numbers are on screen, or null. An id
   *  rather than the record, because the numbers have to be fetched fresh and
   *  this must survive the chat being deleted underneath it. */
  chatStats: string | null;
  /** The folder-trust dialog is open for the current project. Raised when a
   *  folder nobody has decided about is opened, because "no decision" means
   *  "untrusted" and the user has to be told that rather than left wondering why
   *  their AGENTS.md is not being read. */
  trustPrompt: boolean;
  /** "Open project" dialog beyond the native folder picker: choose a WSL
   *  distro, clone a repository URL, or a folder on an SSH host. */
  openFrom: "wsl" | "clone" | "ssh" | null;
  /** Installed WSL distros, as last listed by the backend (empty off Windows). */
  wsl: WslDistro[];
  /** Saved SSH hosts (Settings → Remote), for naming `ssh://` projects by
   *  host name. Null until first loaded (`loadSshHosts`). */
  sshHosts: SshHostView[] | null;
}

/** An in-app attention notice: the window is focused, so an OS toast would be noise. */
export interface Notice {
  key: string;
  task_id: string;
  needs: Needs;
  title: string;
  body: string;
  at: number;
}

/**
 * A note on text the user selected in a chat: the quoted words, the comment they
 * wrote about them, and the item they were pointing at.
 *
 * The quote is carried through to the model verbatim rather than pointed at by id:
 * the model has no transcript of its own to resolve an item id against, so a note
 * that said only "item abc123" would arrive as nothing at all. `item` is still
 * kept, because it is what lets the transcript draw the note under the message it
 * belongs to.
 */
export interface Note {
  id: string;
  /** Item the selection was in, so the note lands under the right message. */
  item: string;
  text: string;
  body: string;
}


let state: State = {
  ready: false, bootError: null, settings: null, providers: [], providerPresets: [], models: [], shell: "", dataDir: "", monthSpend: 0, claudeAllowed: false,
  tasks: {}, items: {}, pendingMessages: {}, bg: {}, subItems: {}, artificts: {}, accounts: [], agents: [], skills: [], subOpen: null, pickFor: null, renaming: null,
  view: "home", hist: [], fwd: [], task: null, settingsTab: "general", modelSetupOpen: false, ultraXOpen: false,
  draft: "", sessionDrafts: {}, attach: {}, attachFiles: {},
  home: { plan: false, ultra: false, ultra_wt: false, ultra_x: null, perm: "turbo", model: "anthropic/claude-opus-5", effort: 2, worktree: false, docker: false, branch: "", subagents: true, assist: "default", agents: ["explore", "general"], route: "" },
  projectInfo: null,
  // A phone opens on the content: there the sidebar is a drawer over it.
  sidebar: !isNarrow(), details: typeof window === "undefined" || window.innerWidth >= 1100,
  pal: false, find: false, findSeed: "", swap: false, notes: {}, menu: null, menuAnchor: null, projCtx: null, toast: null, toastAction: null, openError: null, catalogError: null,
  notices: [], picked: {}, selecting: false, mass: false, massAll: false, fork: null, transfer: null, chatStats: null,
  appUpdate: { status: "idle", version: null, error: null, can_install: false },
  appUpdateDismissed: null, appUpdateBusy: false, appUpdateActionError: null,
  trustPrompt: false,
  openFrom: null,
  wsl: [],
  sshHosts: null,
};

const subs = new Set<() => void>();
export const get = () => state;
export function set(patch: Partial<State> | ((s: State) => Partial<State>)) {
  const p = typeof patch === "function" ? patch(state) : patch;
  if (!(Object.keys(p) as (keyof State)[]).some((key) => !Object.is(state[key], p[key]))) return;
  state = { ...state, ...p };
  // Notify now, not in a microtask. Deferring this broke every controlled text
  // field: React restores a controlled field's value at the end of the event
  // batch, from the props of the render it just committed, so a store write that
  // had not re-rendered yet made React put the *previous* value back. In the
  // composer that cleared a character as you typed it and left the caret at the
  // end of the line, so editing the middle of a message kept throwing you to the
  // end — most visibly while an agent was streaming, since a `task` event was
  // landing mid-sentence. Whether the jump happened came down to whether the
  // microtask had run before React's restore, hence "sometimes".
  //
  // The bursts this used to smooth over are now batched where they are made, not
  // here: streamed deltas are folded on a frame (`bufferDelta`/`flushDeltas`),
  // and a repeated task summary is dropped by `shallowEqualTask`. Unchanged
  // patches are discarded above so they don't run every mounted selector.
  subs.forEach((f) => f());
}
let appUpdateRevision = 0;
export const appUpdateKey = (status: AppUpdateStatus) => `${status.status}:${status.version ?? ""}`;
export function receiveAppUpdate(status: AppUpdateStatus) {
  appUpdateRevision++;
  set((s) => ({ appUpdate: status, appUpdateActionError: null,
    appUpdateDismissed: appUpdateKey(s.appUpdate) === appUpdateKey(status) ? s.appUpdateDismissed : null }));
}
export function dismissAppUpdate() { set({ appUpdateDismissed: appUpdateKey(get().appUpdate) }); }
export function appUpdateTasksActive(tasks: Record<string, TaskSummary>) {
  return Object.values(tasks).some((t) => t.status === "running" || t.status === "waiting" || t.busy > 0);
}
/** Only user gestures call these commands; the backend owns periodic work. */
export async function runAppUpdate(action: "check" | "download" | "install") {
  if (get().appUpdateBusy) return;
  if (action === "install" && (appUpdateTasksActive(get().tasks) || get().appUpdate.status !== "ready" || !get().appUpdate.can_install)) return;
  set({ appUpdateBusy: true, appUpdateActionError: null });
  const revision = appUpdateRevision;
  try {
    if (action === "install") await api.appUpdateInstall();
    else {
      const status = await (action === "check" ? api.appUpdateCheck() : api.appUpdateDownload());
      if (revision === appUpdateRevision) receiveAppUpdate(status);
    }
  } catch (error) {
    set({ appUpdateActionError: String(error) });
  } finally { set({ appUpdateBusy: false }); }
}

export function useStore<T>(sel: (s: State) => T): T {
  // The third argument is the server snapshot: without it React refuses to render
  // store-backed components at all in `renderToStaticMarkup`, which the tests use.
  return useSyncExternalStore(subscribe, () => sel(state), () => sel(state));
}
/** Subscribes to store notifications. Returns an unsubscribe fn. */
export function subscribe(f: () => void) {
  subs.add(f);
  return () => { subs.delete(f); };
}

/**
 * Share an in-flight pause request for a chat. Status summaries arrive over a
 * separate event channel, so rapid Esc presses or clicks can otherwise queue
 * identical IPC calls before the first summary reaches the UI.
 */
const pendingPauseRequests = new Map<string, Promise<void>>();
export function requestPause(taskId: string): Promise<void> | null {
  if (pendingPauseRequests.has(taskId)) return null;
  const request = api.pause(taskId);
  pendingPauseRequests.set(taskId, request);
  const clear = () => {
    if (pendingPauseRequests.get(taskId) === request) pendingPauseRequests.delete(taskId);
  };
  void request.then(clear, clear);
  return request;
}

/**
 * The broadcast open in a chat, and which agents have taken it on. Null while
 * the look-up is still on its way, and when the chat has never broadcast.
 */
/** A clock that re-renders its caller once a minute, for relative timestamps
 *  like "3m ago". Far cheaper than the old `set({})` tick, which replaced the
 *  root state object and re-ran every mounted store selector — re-rendering the
 *  sidebar and the home screen, which then re-sorted the whole chat list.
 *
 *  One interval drives every caller, not one per caller. Each `Row` in the
 *  sidebar wants a clock, and 80 rows meant 80 timers and 80 staggered
 *  re-renders; the interval is refcounted, so the first caller starts it and
 *  the last one to unmount stops it. Callers still only re-render *themselves*
 *  on a tick — that granularity is deliberate, and it's what keeps a tick from
 *  re-rendering the whole sidebar. */
const nowSubs = new Set<() => void>();
let nowTimer: ReturnType<typeof setInterval> | null = null;
/** The period the shared interval runs at. Every caller in practice wants this
 *  one; a caller asking for something else keeps its own timer (below) rather
 *  than dragging every other subscriber off its cadence. */
const nowMsActive = 30_000;

export function useNow(ms = 30_000) {
  const [, setTick] = useState(0);
  useEffect(() => {
    // Each caller bumps its own counter. The bump has to stay an *increment*:
    // setting a constant would make the state identical on the second tick,
    // React would bail out, and the clock would quietly stop.
    const tick = () => setTick((n: number) => n + 1);
    nowSubs.add(tick);
    if (nowMsActive !== ms) {
      // One shared interval can only run at one rate, and the dominant one is
      // 30s. A caller asking for a different period gets its own timer rather
      // than dragging every other row off the sidebar's cadence.
      const id = setInterval(tick, ms);
      return () => {
        nowSubs.delete(tick);
        clearInterval(id);
      };
    }
    if (nowTimer === null) {
      nowTimer = setInterval(() => {
        // Copied first: a subscriber's own tick can mount or unmount others.
        for (const f of [...nowSubs]) f();
      }, nowMsActive);
    }
    return () => {
      nowSubs.delete(tick);
      if (!nowSubs.size && nowTimer !== null) {
        clearInterval(nowTimer);
        nowTimer = null;
      }
    };
  }, [ms]);
}

/**
 * A value that only propagates to the component every `ms`, and always lands.
 *
 * For text that arrives in a stream. A growing string is handed down as-is, so
 * whatever is done with it — a markdown parse, a layout read — is redone in full
 * every time it changes, and a fast stream pays for the whole value dozens of
 * times a second. This holds the newest value back until `ms` have passed, so
 * that work runs a few times a second over text that has stopped changing. The
 * final value is never withheld: a finished message always renders in full.
 */
export function useThrottled<T>(value: T, ms: number): T {
  const [held, setHeld] = useState(value);
  const latest = useRef(value);
  const at = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  latest.current = value;

  useEffect(() => {
    if (value === held) return;
    const now = Date.now();
    const wait = ms - (now - at.current);
    // Nothing to catch up on: publish straight away, so the first chunk of a
    // reply and any big jump between frames are never delayed.
    if (wait <= 0) {
      at.current = now;
      setHeld(value);
      return;
    }
    // A timer is already counting down to the next publish; the value it will
    // publish is read from `latest`, so newer text costs nothing here.
    if (timer.current) return;
    timer.current = setTimeout(() => {
      timer.current = null;
      at.current = Date.now();
      setHeld(latest.current);
    }, wait);
    return () => {
      if (timer.current) {
        clearTimeout(timer.current);
        timer.current = null;
      }
    };
  }, [value, ms, held]);

  // Nothing to publish once this is gone, so drop any pending timer: the
  // component is unmounted and there is no longer anywhere to show the text.
  // Anything still unwritten is in `latest`, and the message it belonged to is
  // already stored whole by the backend, so re-opening the chat shows it.
  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
  }, []);

  return held;
}

/** Pauses share one toast ledger slot across every chat. "Pause all", a quota
 *  wall or a dead provider freezes many chats in the same instant, and a toast
 *  each for what is one event buried the user. The first still names a chat and
 *  opens it; the rest are in the sidebar and the in-app notices. */
/** The toast, and optionally one action it offers.
 *
 *  An action makes the difference between a message and a way back: archiving a
 *  chat is reversible, and a reversible thing with no visible undo reads as
 *  destructive. Closures here capture ids, so the action has to stop being
 *  clickable when the toast goes — see `flash`. */
export type ToastAction = { label: string; run: () => void };

const PAUSE_TOAST_KEY = "*";
let toastTimer: number | undefined;
/** Show a transient message. `action` adds one button to it. */
export function flash(t: string, action?: ToastAction) {
  clearTimeout(toastTimer);
  set({ toast: t, toastAction: action ?? null });
  // A toast that never expires would leave the message pinned forever, so a
  // caller that *can* time out must. Not every caller can: `flash` is reached
  // from node-environment tests and from the store's own error paths, where
  // there is no window to schedule against.
  if (typeof window === "undefined") return;
  // Longer when there is something to click, because 2.6s is long enough to
  // read a sentence and not long enough to act on one.
  toastTimer = window.setTimeout(() => set({ toast: null, toastAction: null }), action ? 7000 : 2600);
}

/**
 * Is the window in front of the user?
 *
 * `document.hasFocus()` is the wrong signal for a tray app: a window hidden to
 * the tray (or minimised) has no OS focus to report, and on Windows it keeps
 * answering `true`, so every attention event was routed to an in-app notice in
 * a window nobody can see. The OS knows, so ask it — the main window is the only
 * one this app opens, and `core:window:default` already grants these commands.
 *
 * Cached rather than awaited: the attention listener is synchronous, and an
 * event can land in the same tick the window is clicked or unhidden. `onFocus`
 * and the visibility polling below keep the cache honest between events.
 */
let inFront = typeof document === "undefined" || document.hasFocus();

/** Live window state, for the attention listener. Exported for tests. */
export const windowInFront = () => inFront;

/**
 * Pure: does the window count as in front? Focus alone is not enough — a window
 * hidden to the tray or minimised can still report focus, and a notice nobody
 * can see is worse than a toast that can interrupt. Exported for tests.
 */
export const inFrontNow = (focused: boolean, visible: boolean, minimized: boolean) => focused && visible && !minimized;

/** Poll as well as listen: hiding to the tray is not a focus event, so nothing
 *  fires when the window leaves the screen that way. */
export async function watchWindowState() {
  // No Tauri window (tests, or a plain browser preview): keep the DOM answer
  // rather than throwing out of boot.
  let w: ReturnType<typeof getCurrentWindow>;
  try {
    w = getCurrentWindow();
  } catch {
    return;
  }
  const refresh = async (focused: boolean) => {
    const [visible, minimized] = await Promise.all([w.isVisible().catch(() => true), w.isMinimized().catch(() => false)]);
    const wasInFront = inFront;
    inFront = inFrontNow(focused, visible, minimized);
    // Back in the window: the notices are in view, so the toasts raised while
    // you were away have done their job and the next event is news again.
    if (inFront && !wasInFront) toasts.forgetAll();
  };
  const poll = window.setInterval(() => { void w.isFocused().then((f) => refresh(f)).catch(() => {}); }, 1000);
  trackDetach(() => window.clearInterval(poll));
  try {
    await refresh(await w.isFocused().catch(() => inFront));
    trackDetach(await w.onFocusChanged(({ payload }) => void refresh(payload)));
  } catch {
    // The poll above still keeps the cache current if the listener can't attach.
  }
}

/** Dismiss an in-app notice; `open` jumps straight to the chat it was about. */
export function dismissNotice(key: string, open = false) {
  const n = get().notices.find((x) => x.key === key);
  set((s) => ({ notices: s.notices.filter((x) => x.key !== key) }));
  if (open && n) go("session", { task: n.task_id });
}

/** The chat a clicked toast names, or null when it names nothing usable.
 *
 *  A toast click reaches us from the Windows shell, having been through the
 *  toast's own `launch` argument — another process decided this string. It is
 *  checked against the shape `agent::new_id` mints (12 hex chars) before being
 *  used as a task id, so a value that is not one of ours cannot become a
 *  navigation. Mirrors the backend's own check in `win::parse_activation`; both
 *  sides validate, because the argument is untrusted at both ends.
 */
export const clickedChat = (taskId: string | undefined): string | null =>
  taskId && /^[0-9a-f]{12}$/.test(taskId) ? taskId : null;

/** Raise a desktop toast through the backend, which is what makes it clickable. *
 *  Goes through `api.toast` rather than `sendNotification` because the plugin's
 *  desktop toast carries no `launch` argument naming the chat — a click on it
 *  has nothing to activate. The backend builds the document and owns the
 *  activator that turns a click back into a navigation (`win::install`), so the
 *  chat id goes with the toast rather than being looked up afterwards.
 *
 *  Swallows its own failure: a toast that could not be raised is a state the
 *  in-app notice already covers, and an unhandled rejection here would take the
 *  attention handler down with it.
 */
const raiseToast = (title: string, body: string, taskId: string) => {
  void api.toast(taskId, title, body).catch(() => { /* in-app badge still shows it */ });
};

/** Which OS toasts are still waiting to be looked at. See `ToastLedger`. */
const toasts = createToastLedger();

function openCurrentChat() {
  const { view, task } = get();
  if ((view === "session" || view === "diff") && task) {
    toasts.forgetChat(task);
    void openTask(task);
    // Opening acknowledges unread state in the shared backend, including for
    // another desktop window or remote browser. Blocking requests stay unread
    // until answered; only this window's notice card is cleared here.
    set((s) => ({ notices: s.notices.filter((n) => n.task_id !== task) }));
  }
}

export function go(view: View, extra: Partial<State> = {}) {
  set((s) => ({
    view, menu: null, pal: false,
    // Any new navigation invalidates the forward stack, the way a browser's
    // does: once you leave the screen Back came from, Forward no longer has a
    // meaning. Without this, Back, then somewhere new, then Forward would jump
    // you back into a branch you already walked away from.
    fwd: [],
    hist: s.view !== view || (extra.task && extra.task !== s.task) ? [...s.hist, { view: s.view, task: s.task }].slice(-20) : s.hist,
    ...extra,
  }));
  openCurrentChat();
}
export function back() {
  const h = state.hist;
  if (!h.length) return;
  const p = h[h.length - 1];
  if (!p) return;
  // The screen being left goes on the forward stack, so Back and Forward are
  // exact mirrors rather than two lists that drift apart.
  set({ view: p.view, task: p.task, hist: h.slice(0, -1), fwd: [...state.fwd, { view: state.view, task: state.task }].slice(-20), menu: null });
  openCurrentChat();
}

/** Re-take the screen Back came from. A no-op when there is nothing to replay. */
export function forward() {
  const f = state.fwd;
  if (!f.length) return;
  const p = f[f.length - 1];
  if (!p) return;
  set({ view: p.view, task: p.task, fwd: f.slice(0, -1), hist: [...state.hist, { view: state.view, task: state.task }].slice(-20), menu: null });
  openCurrentChat();
}

/** Open a chat's transcript, background runs and saved draft.
 *  A failed `task_get` is reported once and recorded in `openError`, so a chat
 *  that never arrives can say why instead of spinning on "Loading conversation…". */
export async function openTask(id: string) {
  // Cleared up front so a retry does not flash the previous failure's message
  // while the request is in flight; set again below if this one fails too.
  if (get().openError) set({ openError: null });
  let r: Awaited<ReturnType<typeof api.taskOpen>>;
  try {
    r = await api.taskOpen(id);
  } catch (e) {
    const why = String(e);
    set({ openError: { id, why } });
    flash(`Couldn't open that chat · ${why}`);
    return;
  }
  // A backend that answered with something other than a page leaves nothing to
  // show; say so once rather than throwing inside the store update below.
  if (!r?.summary || !Array.isArray(r.items)) {
    set({ openError: { id, why: "The harness returned no transcript for this chat." } });
    flash("Couldn't open that chat · the harness returned no transcript");
    return;
  }
  // Publish the newest transcript page before reading drafts or agent history.
  // The page itself is applied as it arrives (it is the newest `PAGE` items, so
  // nothing in it can be staler than what the chat holds), but the summary is
  // folded in: a run that ended while this request was in flight published its
  // "done" already, and writing this snapshot over it left the chat spinning
  // with nothing to correct it. See `newerSummary`.
  set((s) => ({ tasks: { ...s.tasks, [id]: newerSummary(s.tasks[id], r.summary) }, items: { ...s.items, [id]: r.items }, bg: { ...s.bg, [id]: r.bg } }));
  historyCursor.set(id, r.cursor);
  if (r.cursor) setTimeout(() => {
    const timeline = document.querySelector<HTMLElement>(".timeline");
    if (timeline && state.task === id) void loadPreviousHistory(id, timeline);
  }, 0);
  if (state.sessionDrafts[id] === undefined) {
    void api.draftGet(id).then((d) => {
      if (d && state.sessionDrafts[id] === undefined) set((s) => ({ sessionDrafts: { ...s.sessionDrafts, [id]: d } }));
    }).catch(() => {});
  }
  // Everything the app was holding for the chats you are *not* looking at goes
  // now that this one is loaded.
  //
  // This store used to be a cache that only ever grew: `items[id]` and
  // `subItems[id]` were created on the first event for a chat and stayed until
  // the chat was deleted, so a session that ran six chats in the background
  // held six full transcripts — plus every subagent's, for a swarm whose
  // drawer was never opened — for as long as the app ran. Nothing read them
  // after the fact: the transcript is only mounted for `state.task`, the
  // pending-question list and the queued-message list both live inside the open
  // chat's panel, and switching back re-runs `task_get`, which is the same
  // request the open cost. Back/Forward are the one exception and they are
  // handled below.
  set((s) => evictTranscripts(s, id));
}

/** Drop every transcript that isn't `keepId` and isn't still on a nav stack.
 *
 *  A nav-stack entry is a chat the user can return to with one keystroke, and
 *  `back()` deliberately does not refetch — it trusts the transcript is already
 *  there. Evicting one of those would silently show an empty chat, which is why
 *  this keeps them, and why that can't itself become the leak: both stacks are
 *  already sliced to their last 20 entries.
 *
 *  Runs in the same pass that loaded the new chat, so it never leaves the app
 *  holding more than it did before.
 *
 *  Exported for tests: what counts as droppable is the whole point.
 */
export function evictTranscripts(s: State, keepId: string): Partial<State> {
  // A nav entry's `task` is null for the Home screen — nothing to keep there,
  // and a null key would never match a chat id anyway.
  const pinned = new Set<string>([keepId, ...s.hist.map((h) => h.task), ...s.fwd.map((h) => h.task)].filter((id): id is string => !!id));
  // Dropped one map at a time, and only rebuilt if something went: a plain
  // `delete` mutates in place, which zustand can't see, so the copy has to be
  // made only when there is something to remove.
  const drop = <T,>(m: Record<string, T>): { next: Record<string, T>; n: number } => {
    const doomed = Object.keys(m).filter((k) => !pinned.has(k));
    if (!doomed.length) return { next: m, n: 0 };
    const next = { ...m };
    for (const k of doomed) delete next[k];
    return { next, n: doomed.length };
  };
  const items = drop(s.items);
  const subItems = drop(s.subItems);
  const bg = drop(s.bg);
  const artificts = drop(s.artificts);
  if (!items.n && !subItems.n && !bg.n && !artificts.n) return {};
  return {
    ...(items.n ? { items: items.next } : {}),
    ...(subItems.n ? { subItems: subItems.next } : {}),
    ...(bg.n ? { bg: bg.next } : {}),
    ...(artificts.n ? { artificts: artificts.next } : {}),
  };
}

export async function loadProject(path: string) {
  if (!path) return set({ projectInfo: null, catalogError: null });
  try {
    const info = await api.project(path);
    // Guard against a stale answer: two project switches in flight can resolve
    // out of order, and the settings panel turns this inventory into per-file
    // switches whose writes go to the *current* project. Rendering one folder's
    // files under another's name would let a toggle record a rule against the
    // wrong path. `info.path` is what the backend actually answered about; when
    // it is absent the answer is applied as before rather than dropped, so this
    // can never turn a working load into a silently empty panel.
    if (info.path && normPath(info.path) !== normPath(get().settings?.project ?? "")) return;
    set((s) => ({ projectInfo: info, home: { ...s.home, branch: info.branch } }));

  } catch {
    set({ projectInfo: null });
    return;
  }
  // The agents/skills panels render whatever these last returned, so a silent
  // failure would read as "none configured". A project load is not a moment
  // that wants a toast, and it happens on every switch: record the reason
  // instead, and let the panels that depend on it show it inline.
  api.agents(path)
    .then((agents) => set({ agents, catalogError: null }))
    .catch((e) => { console.warn(`openleash: agents for ${path} failed:`, e); set({ catalogError: `Agents: ${String(e)}` }); });
  api.skills(path)
    .then((skills) => set({ skills }))
    .catch((e) => { console.warn(`openleash: skills for ${path} failed:`, e); set((s) => ({ catalogError: s.catalogError ?? `Skills: ${String(e)}` })); });
}

/**
 * Write settings and publish the result. Every call site in the app fires this
 * from an event handler with no `.catch` of its own, so a rejected write used
 * to become an unhandled rejection and a toggle that silently didn't stick.
 * The one place that reports the failure is here: it toasts, then resolves with
 * `null` so callers can tell "not saved" from "saved". Callers that need to
 * undo a local change they already made check for the null.
 */
export async function saveSettings(patch: Partial<Settings>): Promise<Settings | null> {
  let settings: Settings;
  try {
    settings = await api.settings(patch);
  } catch (e) {
    flash(`Couldn't save settings · ${String(e)}`);
    return null;
  }
  set({ settings });
  return settings;
}

// One Tauri listener set for the life of the app. React StrictMode (dev)
// mounts, unmounts and remounts <App/>, so its boot effect runs twice; without
// a guard the second boot() stacked a second "ol://event" listener and every
// streamed delta was appended twice — i.e. every word showed up doubled
// ("Hi there there!! How How can can ...", first chunk via "item" staying
// single). Keep boot idempotent and expose disposeBoot for effect cleanup.
//
// `attachGen` is the part `listenersAttached` alone could not do. The attach is
// asynchronous, so between `listenersAttached = true` and the `listen` calls
// actually resolving there is a window where a remount's disposeBoot() clears
// the flag and the next boot() starts attaching a second set — the first set
// then lands too, and both stay live. Generation numbers make the outcome
// order-independent instead of depending on which await happens to win:
// disposeBoot() bumps the generation, and an attach that was already in flight
// when it did tears itself down on arrival instead of installing.
let bootInflight: Promise<void> | null = null;
let listenersAttached = false;
let attachGen = 0;
let detachListeners: Array<() => void> = [];

function trackDetach(u: unknown) {
  if (typeof u === "function") detachListeners.push(u as () => void);
}

/** Remove Tauri listeners so a remount / HMR / retry never stacks duplicates. */
export function disposeBoot() {
  // Invalidate any attach still in flight, so it can undo itself when it lands.
  attachGen++;
  for (const f of detachListeners) {
    try { f(); } catch { /* already detached */ }
  }
  detachListeners = [];
  listenersAttached = false;
  bootInflight = null;
}

/** Forget a chat completely: its summary, transcript, drafts and attachments.
 *  Deleting a chat used to drop only `tasks[id]`, so the store kept the whole
 *  transcript of every chat ever deleted for the life of the process. */
export function forgetTask(s: State, id: string): Partial<State> {
  const tasks = { ...s.tasks };
  delete tasks[id];
  const items = { ...s.items };
  const hadItems = delete items[id];
  const subItems = { ...s.subItems };
  const hadSubs = delete subItems[id];
  const bg = { ...s.bg };
  const hadBg = delete bg[id];
  const artificts = { ...s.artificts };
  const hadArtificts = delete artificts[id];
  const sessionDrafts = { ...s.sessionDrafts };
  const hadDraft = delete sessionDrafts[id];
  const attach = { ...s.attach };
  const hadAttach = delete attach[id];
  const attachFiles = { ...s.attachFiles };
  const hadFiles = delete attachFiles[id];
  // Notes went here too. They are the one thing `forgetTask` never dropped, so a
  // deleted chat's annotations stayed in memory for the life of the process —
  // each holding a quote of up to 6000 characters, and nothing else in the app
  // has any way to reach them, because there is no longer a chat to open.
  const notes = { ...s.notes };
  const hadNotes = delete notes[id];
  return {
    tasks,
    ...(hadItems ? { items } : {}),
    ...(hadSubs ? { subItems } : {}),
    ...(hadBg ? { bg } : {}),
    ...(hadArtificts ? { artificts } : {}),
    ...(hadDraft ? { sessionDrafts } : {}),
    ...(hadAttach ? { attach } : {}),
    ...(hadFiles ? { attachFiles } : {}),
    ...(hadNotes ? { notes } : {}),
    ...(s.task && !tasks[s.task] ? { task: null, view: "home" as const } : {}),
  };
}

/** True when a task summary says nothing new. Exported for tests. */
export function shallowEqualTask(a: TaskSummary | undefined, b: TaskSummary): boolean {
  if (!a) return false;
  const ka = Object.keys(a) as (keyof TaskSummary)[];
  const kb = Object.keys(b) as (keyof TaskSummary)[];
  if (ka.length !== kb.length) return false;
  for (const k of kb) {
    const x = a[k], y = b[k];
    // `subs` and `todos` are rebuilt on every summary; compare the parts the UI
    // reads so an identical resend counts as unchanged.
    if (k === "subs" || k === "todos") {
      if (!sameAgents(x, y)) return false;
    } else if (k === "rev") {
      // Bookkeeping, not something the UI renders: two identical resends differ
      // in `rev` by construction, and treating that as a change would undo the
      // whole point of this check — a quiet chat republishing its summary would
      // re-render the sidebar and transcript every time.
    } else if (x !== y) return false;
  }
  return true;
}

/**
 * Which of two summaries of the same chat to hold: the later state, always.
 *
 * A chat's summary reaches the store by two routes — pushed as a live `task`
 * event, or read back as a snapshot (`app_boot`'s list, `task_open` when a chat
 * is opened, the reconciliation below). A snapshot is built at one instant and
 * applied whenever it lands, so it races every event published while its request
 * was in flight. Applying it unconditionally put the chat back where it was
 * *before* those events, and nothing republished it: a chat that finished while
 * you were opening it sat at "running" with a spinner that never stopped, one
 * whose question you answered sat at "waiting", and the sidebar went on showing
 * it, until something unrelated published that chat again. Reloading re-ran the
 * same race, which is why it came back "sometimes" and sometimes outlived the
 * reload too.
 *
 * `rev` is stamped on every summary the backend builds (see `SUMMARY_REV` in
 * `agent/mod.rs`), so a higher one is strictly later state. Deliberately not
 * `updated_at`: that is a clock, and two updates in the same millisecond — or a
 * clock that steps backwards — would order the wrong way, which is the same
 * desync with a harder cause to find.
 *
 * A summary with no `rev` (an older backend, or a test fixture) is applied as
 * before: this can only ever *withhold* a summary the backend marked as older.
 *
 * Exported for tests.
 */
export function newerSummary(held: TaskSummary | undefined, incoming: TaskSummary): TaskSummary {
  if (!held || typeof incoming.rev !== "number" || typeof held.rev !== "number") return incoming;
  return incoming.rev < held.rev ? held : incoming;
}

/**
 * Fold a whole list of summaries — a boot snapshot, a resync — into what the
 * store already holds, without ever moving a chat backwards.
 *
 * Additive: a chat the snapshot does not mention is kept, because the snapshot
 * can be older than a chat the store learned about while it was in flight (a
 * chat created, duplicated or imported in that window). Removal is the backend's
 * business, and no summary carries that information.
 *
 * Exported for tests.
 */
export function mergeTaskSnapshot(held: Record<string, TaskSummary>, snapshot: TaskSummary[]): Record<string, TaskSummary> {
  const out = { ...held };
  for (const t of snapshot) out[t.id] = newerSummary(held[t.id], t);
  return out;
}

/** One reconciliation at a time: a flurry of visibility changes must not queue
 *  a chat-list read per switch. */
let resyncInflight: Promise<void> | null = null;

/**
 * Read the whole chat list back and fold it in.
 *
 * The live `task` events are what keep the store current, and nothing calls this
 * on a schedule: it is the recovery path for the states events cannot reach.
 * A page that was suspended — a phone in a pocket, a laptop that slept, a
 * webview hidden to the tray — can miss events that were published while it was
 * frozen (nothing is owed to a listener that was not running), and no later
 * event for a settled chat will ever come along to correct it. Asking the
 * backend for the list is the one thing that fixes that without guessing, and
 * `newerSummary` means it can never undo a fresher event that arrived first.
 *
 * Never rethrows: failing to reconcile is not news, and the live events still do
 * their job.
 */
export function resyncTasks(): Promise<void> {
  if (resyncInflight) return resyncInflight;
  resyncInflight = api.tasks()
    .then((list) => { if (Array.isArray(list)) set((s) => ({ tasks: mergeTaskSnapshot(s.tasks, list) })); })
    .catch(() => { /* the next visibility change tries again */ })
    .then(() => { resyncInflight = null; });
  return resyncInflight;
}

/**
 * Reconcile when this page comes back into view. The moment a hidden page is
 * shown again is when it is most likely to be holding stale chats *and* the
 * moment the user is looking, so it is the one trigger worth having. Exported
 * for tests; `boot` detaches it with the rest of its listeners.
 */
export function watchVisibility(): () => void {
  if (typeof document === "undefined") return () => {};
  const onVisible = () => { if (document.visibilityState === "visible") void resyncTasks(); };
  document.addEventListener("visibilitychange", onVisible);
  return () => document.removeEventListener("visibilitychange", onVisible);
}

/** Field-wise comparison of the rebuilt `subs`/`todos` lists. Takes `unknown`
 *  because it is handed a union of those two element shapes; it reads only the
 *  shared field names, so a field the shape lacks (`Todo` has no `meta`) reads
 *  as equal on both sides — the same thing the previous `any` version did. */
function sameAgents(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
  return a.every((x: unknown, i: number) => {
    const y: unknown = b[i];
    if (x === y) return true;
    if (typeof x !== "object" || x === null || typeof y !== "object" || y === null) return false;
    const p = x as Record<string, unknown>, q = y as Record<string, unknown>;
    return p.id === q.id && p.status === q.status && p.meta === q.meta && p.model === q.model && p.started === q.started && p.report === q.report;
  });
}

/** One item's streamed chunks, and the point in its text they belong at. */
interface PendingDelta {
  /** The length the item's text had when the first of these chunks was
   *  buffered. `appendDeltas` re-checks it before appending — see there. */
  base: number;
  text: string;
}
/** Streamed chunks waiting to be folded into their items, keyed `task\0sub\0item`. */
const pendingDeltas = new Map<string, PendingDelta>();
let deltaFrame: number | null = null;
/** The item id each chat's oldest-loaded page starts at, and which chats have a
 *  page request in flight. A `null` cursor means the history is exhausted. */
const historyCursor = new Map<string, string | null>();
const historyBusy = new Set<string>();

/**
 * Fetch one older page and splice it above what is already loaded, holding the
 * reader's scroll position while it happens.
 *
 * A cursor by item id, not an index: live events mutate the timeline while a page
 * is in flight (`item` appends, `itemgone` removes), so an index cursor drifts and
 * the older page then repeats rows the user already read or skips past others
 * entirely. An id is stable under both, and the `known` filter below is the
 * backstop for an item that arrives by both paths at once.
 */
export async function loadPreviousHistory(id: string, timeline: HTMLElement) {
  const cursor = historyCursor.get(id) ?? null;
  if (!cursor || historyBusy.has(id)) return;
  historyBusy.add(id);
  const oldHeight = timeline.scrollHeight;
  const oldTop = timeline.scrollTop;
  try {
    const page = await api.taskHistory(id, cursor, 100);
    // An empty page means this cursor is exhausted; asking again would spin.
    if (!page.items.length) { historyCursor.set(id, null); return; }
    historyCursor.set(id, page.cursor);
    set((s) => {
      const current = s.items[id] ?? [];
      const known = new Set(current.map((item) => item.id));
      const older = page.items.filter((item) => !known.has(item.id));
      return older.length ? { items: { ...s.items, [id]: [...older, ...current] } } : {};
    });
    requestAnimationFrame(() => {
      if (!timeline.isConnected) return;
      timeline.scrollTop = oldTop + Math.max(0, timeline.scrollHeight - oldHeight);
    });
  } catch {
    // The next upward scroll can retry; never block the already-loaded history.
  } finally {
    historyBusy.delete(id);
  }
}

/** The length of one item's text as the store holds it now, or null when the
 *  item is not in the transcript it names. Read from the live tail, so a long
 *  page of history behind the streaming item is never walked. */
function itemTextLen(task: string, sub: string | null, item: string): number | null {
  const list = sub ? state.subItems[task]?.[sub] : state.items[task];
  if (!list) return null;
  for (let i = list.length - 1; i >= 0; i--) {
    const it = list[i]!;
    if (it.id === item) return it.text.length;
  }
  return null;
}

/** Buffers one streamed chunk. Exported for tests: this is the coalescing. */
export function bufferDelta(task: string, sub: string | null, item: string, text: string) {
  if (!text) return;
  const key = `${task}\u0000${sub ?? ""}\u0000${item}`;
  const held = pendingDeltas.get(key);
  if (held) {
    held.text += text;
    return;
  }
  // A chunk for an item this page is not holding has nowhere to land: the
  // transcript was evicted, or its chat is not open. `appendDeltas` would drop
  // it at the flush, so there is nothing gained by carrying it there.
  const base = itemTextLen(task, sub, item);
  if (base === null) return;
  pendingDeltas.set(key, { base, text });
}

/** Fold buffered chunks into the items they belong to. A chunk is appended only
 *  while the item's text is still exactly where the last applied chunk left it.
 *
 *  The text can be replaced instead of appended: the backend re-emits a finished
 *  text item carrying the whole of it (and stamps the model on it), and a
 *  transcript read — `task_open`, a rewind, `task_get` — replaces the list. Any
 *  such copy already contains the chunks still sitting in this buffer, because
 *  they were emitted before it was built, so appending them again prints that
 *  text twice. The field report was a reply whose tail repeated verbatim from a
 *  chunk boundary on, which is what re-applying a buffered chunk looks like.
 *
 *  This is common rather than exotic on a remote page: `requestAnimationFrame`
 *  does not run while a document is hidden, so a phone in a pocket buffers a
 *  whole reply and flushes it only once the page is shown again — long after
 *  the item it belongs to was replaced by its finished copy.
 *
 *  A chunk that fails the check is dropped, not merged: a rewrite comes from the
 *  backend's own copy of the item, which holds every delta it has emitted, so
 *  the replacement is already complete and there is nothing left to splice in.
 *  Searched from the live tail, stopping as soon as the buffered chunks have
 *  landed, so history behind the streaming item is never touched. */
function appendDeltas(list: Item[], chunks: Map<string, PendingDelta>): Item[] {
  let next = list;
  for (let i = list.length - 1; i >= 0 && chunks.size; i--) {
    const item = list[i]!;
    const chunk = chunks.get(item.id);
    if (chunk === undefined) continue;
    chunks.delete(item.id);
    if (item.text.length !== chunk.base) continue;
    if (next === list) next = list.slice();
    next[i] = { ...item, text: item.text + chunk.text };
  }
  return next;
}

/** Applies every buffered chunk in one state update, then re-arms for the next frame. */
export function flushDeltas() {
  deltaFrame = null;
  if (!pendingDeltas.size) return;
  // Grouped by transcript so one pass copies each list at most once.
  const byList = new Map<string, { sub: string | null; chunks: Map<string, PendingDelta> }>();
  for (const [key, chunk] of pendingDeltas) {
    const [id, sub, item] = key.split("\u0000");
    // The key is `task\0sub\0item` and we wrote it with all three non-empty
    // (`sub ?? ""` becomes "" only when there is no sub, and the task and item
    // ids are always real), so every part is present.
    if (id === undefined || item === undefined) continue;
    const gk = `${id}\u0000${sub}`;
    let g = byList.get(gk);
    if (!g) byList.set(gk, (g = { sub: sub || null, chunks: new Map() }));
    // One key is one item and its chunks are already coalesced, so there is
    // nothing to merge into the group here.
    g.chunks.set(item, chunk);
  }
  pendingDeltas.clear();
  set((s) => {
    let items = s.items, subItems = s.subItems;
    for (const [gk, g] of byList) {
      const [id, sub] = gk.split("\u0000");
      if (id === undefined) continue;
      if (g.sub && sub !== undefined) {
        const list = s.subItems[id]?.[sub];
        if (!list) continue;
        const next = appendDeltas(list, g.chunks);
        if (next !== list) subItems = { ...subItems, [id]: { ...subItems[id], [sub]: next } };
      } else {
        const list = s.items[id];
        if (!list) continue;
        const next = appendDeltas(list, g.chunks);
        if (next !== list) items = { ...items, [id]: next };
      }
    }
    return items === s.items && subItems === s.subItems ? {} : { items, subItems };
  });
}

/** Boot supplies only pending notice items for unopened chats. Prefer any live
 * item already received while boot was in flight, including a dismissal. */
export function mergeNoticeSnapshot(items: Record<string, Item[]>, snapshot: Record<string, Item[]>): Record<string, Item[]> {
  const merged = { ...items };
  for (const [id, notices] of Object.entries(snapshot)) {
    const live = items[id] ?? [];
    const known = new Set(live.map((it) => it.id));
    merged[id] = [...live, ...notices.filter((it) => !known.has(it.id))];
  }
  return merged;
}

/** A subagent's transcript as read from disk, merged with what arrived live while
 *  that read was in flight. Same rule as `mergeNoticeSnapshot`: a live copy of an
 *  item wins, since it is at least as new, and live items the snapshot missed are
 *  kept after it. Exported for tests. */
export function mergeSubTranscript(live: Item[] = [], snapshot: Item[]): Item[] {
  const liveById = new Map(live.map((it) => [it.id, it]));
  const seen = new Set<string>();
  const merged = snapshot.map((it) => {
    seen.add(it.id);
    return liveById.get(it.id) ?? it;
  });
  for (const it of live) if (!seen.has(it.id)) merged.push(it);
  return merged;
}

/** Read one subagent's transcript into the store. `task_open` only sends the main
 *  timeline, so without this a chat opened after launch shows a subagent with no
 *  brief and no work, until something new happens inside it. */
export async function loadSubTranscript(taskId: string, subId: string): Promise<void> {
  let snapshot: Item[];
  try {
    snapshot = await api.subTranscript(taskId, subId);
  } catch (e) {
    flash(`Couldn't load that subagent's transcript · ${String(e)}`);
    return;
  }
  set((s) => {
    const task = s.subItems[taskId] ?? {};
    return { subItems: { ...s.subItems, [taskId]: { ...task, [subId]: mergeSubTranscript(task[subId], snapshot) } } };
  });
}

export function boot(): Promise<void> {
  if (bootInflight) return bootInflight;
  bootInflight = (async () => {
    // Listeners first, `api.boot()` second. It used to be the other way round,
    // which meant every event the backend emitted while boot was still in flight
    // was dropped on the floor: the agent's deltas, status changes, usage ticks.
    // Nothing re-fetched to reconcile, so the UI could paint a list that was
    // already stale and stay that way. Attaching here also means the deltas
    // buffer into `pendingDeltas` (see `bufferDelta`) instead of vanishing, so
    // the transcript is correct by the time it renders.
    if (!listenersAttached) {
      listenersAttached = true;
      void watchWindowState();
      trackDetach(watchVisibility());
      // Attach fully before booting: a partial listener set would still drop
      // events, which is the bug this ordering exists to fix.
      //
      // The generation is claimed before the first `await` so a disposeBoot()
      // racing this attach cannot interleave: if it lands mid-attach, the set
      // below is torn down on arrival instead of left installed alongside the
      // next boot's.
      const gen = ++attachGen;
      const dispose = await attachListeners();
      if (gen !== attachGen) {
        dispose();
        return;
      }
      trackDetach(dispose);
      // Subscribe before taking the snapshot; a newer event must win over a
      // slow IPC response. Updater failure must not prevent harness startup.
      const revision = appUpdateRevision;
      void api.appUpdateStatus().then((status) => {
        if (gen === attachGen && revision === appUpdateRevision) receiveAppUpdate(status);
      }).catch((error: unknown) => {
        if (gen === attachGen && revision === appUpdateRevision) set({ appUpdateActionError: String(error) });
      });
    }

    let b: Boot;
    try {
      b = await api.boot();
    } catch (e) {
      set({ bootError: String(e) });
      // Allow a retry: don't cache the failed attempt.
      bootInflight = null;
      return;
    }
    set({
      ready: true, settings: b.settings, providers: b.providers, providerPresets: b.provider_presets ?? [], models: b.models, shell: b.shell, dataDir: b.data_dir, monthSpend: b.month_spend, claudeAllowed: b.claude_allowed ?? false,
      // Folded in rather than assigned: the listeners above are already live, so
      // this snapshot can be older than a `task` event that landed while it was
      // in flight, and replacing the map wholesale would paint that event away —
      // a chat that finished during startup stuck on "running", with nothing
      // left to republish it. See `newerSummary`.
      tasks: mergeTaskSnapshot(get().tasks, b.tasks),
      accounts: b.accounts, agents: b.agents, skills: b.skills ?? [],
      items: mergeNoticeSnapshot(get().items, b.user_notices ?? {}),
      home: { plan: false, ultra: false, ultra_wt: false, ultra_x: null, perm: b.settings.perm, model: b.settings.model, effort: b.settings.effort, worktree: defaultMode(b.settings) === "worktree", docker: defaultMode(b.settings) === "docker", branch: "", subagents: true, assist: b.settings.assist ?? "default", agents: withRequiredAgents(b.settings.default_agents ?? ["explore", "general"]), route: "" },
    });
    set((st) => ({ home: { ...st.home, route: defaultRoute(st.home.model) } }));
    // A toast click that landed before this window was listening — the app was
    // closed and the click launched it. The listener is attached above, and
    // taking the click is what lets later clicks through, so this is the moment.
    api.takeToastClick()
      .then((id) => {
        const task = clickedChat(id ?? undefined);
        if (task) go("session", { task });
      })
      .catch(() => { /* no pending click, or no shell to ask: nothing to open */ });
    api.draftGet("new-chat").then((d) => d && !get().draft && set({ draft: d })).catch(() => {});
    void loadProject(b.settings.project);
  })();
  return bootInflight;
}

/** The `ol://event` payloads, discriminated on `kind`, mirroring the shapes
 *  `Harness::emit` serializes in Rust. A concrete variant per kind lets the
 *  handler narrow `payload` instead of treating it as `any`. */
type TaskEvent =
  | { task_id: string; kind: "task"; payload: TaskSummary }
  | { task_id: string; kind: "item"; payload: Item }
  | { task_id: string; kind: "sitem"; payload: { sub_id: string; item: Item } }
  | { task_id: string; kind: "delta"; payload: { item_id: string; text: string; sub_id: string | null } }
  | { task_id: string; kind: "drop"; payload: { sub_id: string | null; ids: string[] } }
  | { task_id: string; kind: "itemgone"; payload: { id: string } }
  | { task_id: string; kind: "bg"; payload: Bg[] }
  | { task_id: string; kind: "artifict"; payload: { artifict_id: string; version_id: string } };

/** The whole Tauri listener set, as one detachable function. */
async function attachListeners(): Promise<() => void> {
    const detachers: Array<() => void> = [];
    const pending: Promise<unknown>[] = [];
    const add = (registration: Promise<unknown>) => {
      pending.push(registration.then((u) => { if (typeof u === "function") detachers.push(u as () => void); }));
    };

    add(listen<TaskEvent>("ol://event", ({ payload: e }) => {
    const id = e.task_id;
    if (e.kind === "task") {
      set((s) => {
        const prev = s.tasks[id];
        // A partial summary (status/step/subagent meta) is merged into the task
        // we already hold, so a quiet chat republishing the same status doesn't
        // hand the session a brand-new object and re-render the transcript.
        //
        // The merge is then held to the one rule the whole store keeps for
        // summaries: this can replace what we hold, never walk it backwards.
        // Events and snapshots are built on two channels (a snapshot answers a
        // request that was sent before the events it races), and a stale event
        // overtaking a newer snapshot would leave the chat wrong the same way.
        const next = prev ? newerSummary(prev, { ...prev, ...e.payload }) : e.payload;
        return shallowEqualTask(prev, next) ? {} : { tasks: { ...s.tasks, [id]: next } };
      });
    } else if (e.kind === "item") {
      set((s) => {
        const list = s.items[id] ? [...s.items[id]] : [];
        const i = list.findIndex((x) => x.id === e.payload.id);
        if (i >= 0) list[i] = e.payload;
        else list.push(e.payload);
        return { items: { ...s.items, [id]: list } };
      });
    } else if (e.kind === "sitem") {
      const sub: string = e.payload.sub_id, item: Item = e.payload.item;
      set((s) => {
        const task = s.subItems[id] ?? {};
        const list = task[sub] ? [...task[sub]] : [];
        const i = list.findIndex((x) => x.id === item.id);
        if (i >= 0) list[i] = item;
        else list.push(item);
        return { subItems: { ...s.subItems, [id]: { ...task, [sub]: list } } };
      });
    } else if (e.kind === "delta") {
      // Streamed text arrives in many small chunks per token. Each one used to
      // copy the whole transcript, rebuild it and re-render the feed; instead the
      // chunk is buffered and flushed on an animation frame, so a burst of chunks
      // costs one update. `appendDeltas` re-checks the text it appends to, so a
      // chunk still in the buffer when that item is rewritten is not applied on
      // top of the rewrite.
      bufferDelta(id, e.payload.sub_id ?? null, e.payload.item_id, e.payload.text);
      if (deltaFrame) return;
      deltaFrame = requestAnimationFrame(flushDeltas);
    } else if (e.kind === "drop") {
      // A failed provider attempt streamed partial output; a retry is coming.
      const sub: string | null = e.payload.sub_id ?? null;
      const ids = new Set<string>(e.payload.ids);
      set((s) => {
        if (sub) {
          const list = s.subItems[id]?.[sub];
          return list ? { subItems: { ...s.subItems, [id]: { ...s.subItems[id], [sub]: list.filter((x) => !ids.has(x.id)) } } } : {};
        }
        const list = s.items[id];
        return list ? { items: { ...s.items, [id]: list.filter((x) => !ids.has(x.id)) } } : {};
      });
    } else if (e.kind === "itemgone") {
      // A message the user took back — a queued row they removed. It never
      // reached the agent, so it leaves the transcript rather than waiting to
      // be overwritten.
      set((s) => {
        const list = s.items[id];
        return list ? { items: { ...s.items, [id]: list.filter((x) => x.id !== e.payload.id) } } : {};
      });
    } else if (e.kind === "bg") {
      set((s) => ({ bg: { ...s.bg, [id]: e.payload } }));
    } else if (e.kind === "artifict") {
      // Latest version per artifict, so a mounted Artificts view (or an inline
      // card) reloads exactly when the agent writes one. Ids only: the content
      // still comes from `artifict_get`.
      set((s) => ({
        artificts: { ...s.artificts, [id]: { ...s.artificts[id], [e.payload.artifict_id]: e.payload.version_id } },
      }));
    }
    }));
    add(listen<{ month: number }>("ol://usage", ({ payload }) => set({ monthSpend: payload.month })));
    // The global pause (pause all) lives in settings, not on a task, so no
    // ol://event payload ever carries it: without this the pause banner on the
    // new-chat page cannot appear until some unrelated settings write happens
    // to round-trip through `settings_update`.
    add(listen<AppUpdateStatus>("ol://app-update", ({ payload }) => receiveAppUpdate(payload)));
    add(listen<Settings>("ol://settings", ({ payload }) => set({ settings: payload })));
    add(listen<AccountView[]>("ol://accounts", ({ payload }) => set({ accounts: payload })));
    add(listen<ModelInfo[]>("ol://models", ({ payload }) => set({ models: payload })));
    add(listen<Attention>("ol://attention", ({ payload }) => {
      const s = get();
      const task = s.tasks[payload.task_id];
      const viewing = s.task === payload.task_id && (s.view === "session" || s.view === "diff");
      if (!task) return;
      // Where it goes is one decision: a focused window gets the in-app notice
      // alone (a toast would cover the chat you're reading), an unfocused one
      // gets the toast *and* the notice, so the event is still there when you
      // come back to this window.
      // The window's focus is a fact we can only get asynchronously, so it is
      // tracked live (see `watchWindowState`) and read from here.
      const where = surfaces(payload, task.title, task.step, windowInFront(), viewing, s.settings?.notify !== false);
      // What this event has to say, or null when it has nothing to say. One
      // decision, shared by all the reports below: `describe()` knows that a
      // pause or a stop the user made themselves is not news — they are standing
      // right there having done it.
      const said = attentionText(payload, task.title, task.step);
      // One toast per chat and kind until you look at it again; the in-app
      // notice below still records every event.
      if (where.includes("desktop") && said && toasts.admit(said.needs === "paused" ? PAUSE_TOAST_KEY : payload.task_id, said.needs)) void notify(payload, task.title, task.step, task, raiseToast);
      // Durable notice cards come from saved items, never the expiring/superseding
      // toast list. Their attention event still raises a desktop notification.
      if (where.includes("inapp") && said && payload.kind !== "notice") {
        set((st) => {
          // One notice per chat: a second event supersedes the first, and a
          // chat that needs an answer outranks the ones that only report.
          const rest = st.notices.filter((x) => x.task_id !== payload.task_id);
          const blocking = (x: Notice) => x.needs === "question" || x.needs === "approval";
          return { notices: [...rest, { key: `${payload.task_id}:${said.needs}`, task_id: payload.task_id, needs: said.needs, title: said.title, body: said.body, at: Date.now() }]
            .sort((a, b) => Number(blocking(b)) - Number(blocking(a)))
            .slice(-4) };
        });
      }
      // The blue dot is not set here either. The backend marks the chat as it
      // publishes this event (`Harness::attention`), so this window, a second
      // window and a phone's browser all draw the same dot — including a page
      // that has only just loaded, or reloaded after a dropped connection, and
      // therefore never saw the event at all.
    }));

    // A clicked desktop toast. The backend raises the window and hands us the
    // chat the toast was raised for; all that is left is to open it, which is
    // `go` so the transcript loads and the chat's notices and unread dot clear
    // exactly as they do when you click it in the sidebar.
    add(listen<{ task_id: string }>("ol://toast-click", ({ payload }) => {
      const task = clickedChat(payload?.task_id);
      if (!task) return;
      go("session", { task });
    }));

    await Promise.all(pending);
    return () => { for (const d of detachers) { try { d(); } catch { /* already detached */ } } };
}

export const modelInfo = (id: string): ModelInfo => {
  const m = state.models.find((x) => x.id === id);
  if (m) return m;
  if (id.startsWith("route/")) {
    const r = state.settings?.routes.find((x) => "route/" + x.id === id);
    const first = r?.steps[0] ? modelInfo(r.steps[0]) : null;
    return { ...(first ?? { context: 128000, output: 32000, input_price: 0, output_price: 0, effort: false, input_types: ["text"], capabilities: [], reasoning_levels: [], reasoning_param: "none" }), id, name: r?.name ?? "Missing route", provider: "route", custom: true, enabled: true };
  }
  const [p, ...rest] = id.split("/");
  return { id, name: rest.join("/") || id, provider: p ?? "", context: 128000, output: 32000, input_price: 0, output_price: 0, effort: false, input_types: ["text"], capabilities: [], reasoning_levels: [], reasoning_param: "none", custom: true, enabled: true };
};

/**
 * A model's accent (working spinner, picker dot): your pick, else its provider's
 * brand colour, else the app violet. Routes use their first step.
 *
 * The family colour used to be a literal here — every id matching `claude`,
 * `opus`, `sonnet` or `haiku` painted the same orange, from any provider, which
 * is why a Claude model reached through OpenRouter looked identical to one on an
 * Anthropic key and why an added model could never be anything but orange.
 * `ProviderView.color` already carries the brand palette as data, so the default
 * follows the provider that will actually serve the request and no second copy
 * of the palette lives in the frontend to fall out of date.
 */
export const modelColor = (id: string): string => {
  const set = state.settings?.model_colors?.[id];
  if (set) return set;
  if (id.startsWith("route/")) {
    // `routes` is `[]` on a real settings object, but `modelColor` also runs off
    // the settings a test or a half-built boot left behind, and it runs from
    // render paths where a throw white-screens the app.
    const r = state.settings?.routes?.find((x) => "route/" + x.id === id);
    if (r?.steps[0]) return modelColor(r.steps[0]);
  }
  return state.providers.find((p) => p.id === id.split("/")[0])?.color || "var(--violet)";
};

/**
 * The model a chat is on: a swap queued for its next turn outranks the live one.
 *
 * `pending` is optional here because it is only ever read for a queued swap —
 * summaries from before it existed have none, and a missing one is the same thing
 * as an empty one.
 */
export const chatModel = (t: Pick<TaskSummary, "model" | "pending">): string => t.pending?.model || t.model;

/**
 * The model one subagent is on, mirroring the backend's `resolve_model` exactly:
 * a model chosen for this agent, else the X layer for its depth, else the chat's.
 *
 * `sub.model` is empty unless somebody picked a model for that agent, so a panel
 * that read it directly showed a blank name and a generic colour for every agent
 * actually running on the chat's model — and a queued swap never showed up on any
 * of them. Exported for tests: the two sides have to agree.
 */
export function subModel(t: Pick<TaskSummary, "model" | "pending" | "ultra_x">, sub: Pick<SubInfo, "model" | "depth">): string {
  if (sub.model) return sub.model;
  // `depth` is optional on tasks saved before it existed; those agents are all
  // the main agent's own, so depth 1 is the right layer for them.
  const layer = xOn(t.ultra_x) ? t.ultra_x!.layers[(sub.depth ?? 1) - 1] : undefined;
  return layer?.model || chatModel(t);
}

export const ZOOMS = [60, 70, 80, 90, 100, 110, 125, 150, 175, 200];

/** The zoom a fresh install starts on, and what Ctrl 0 / Reset goes back to. */
export const DEFAULT_ZOOM = 110;

/**
 * Current zoom as a CSS scale factor. Every overlay that positions itself in
 * unzoomed pixels has to undo the app root's zoom, so they all read it here
 * rather than each picking their own fallback.
 */
export function zoom() {
  return zoomFactor(get().settings?.ui_zoom);
}

/**
 * The app root's zoom for a saved `ui_zoom`. A phone driving this computer
 * remotely ignores the saved value: it is the desktop's setting, tuned for the
 * desktop's monitor, and 150% there would leave a phone about 260px of layout.
 * The phone renders at 100% (its own pinch-zoom covers the rest) with the
 * larger touch targets App.css gives coarse pointers. `App` and `zoom()` both
 * go through here so the root and the overlays always agree.
 */
export function zoomFactor(uiZoom: number | undefined) {
  return isPhoneClient() ? 1 : (uiZoom ?? DEFAULT_ZOOM) / 100;
}

/** A remote-control browser on a touch-first device (a phone or tablet). */
export function isPhoneClient() {
  return isRemoteClient() && typeof window !== "undefined" && !!window.matchMedia?.("(pointer: coarse)").matches;
}

/** Whether the window is phone-width: the sidebar is a drawer over the content
 *  and the settings nav a strip above it. Keep the width in step with the
 *  `max-width: 640px` media queries in App.css. A function declaration, not a
 *  const, because the initial state above reads it while the module loads. */
export function isNarrow() {
  return typeof window !== "undefined" && !!window.matchMedia?.("(max-width: 640px)").matches;
}

/** Step the interface zoom like an IDE: Ctrl + / Ctrl - / Ctrl 0. */
export function stepZoom(dir: 1 | -1 | 0) {
  const cur = get().settings?.ui_zoom ?? DEFAULT_ZOOM;
  const next = dir === 0 ? DEFAULT_ZOOM : dir > 0 ? ZOOMS.find((z) => z > cur) ?? cur : [...ZOOMS].reverse().find((z) => z < cur) ?? cur;
  if (next !== cur) void saveSettings({ ui_zoom: next });
}


/** Connected right now: a key, a custom endpoint, or an account-backed provider with an account. */
export const isConnected = (p: ProviderView) => p.connected || (!!p.account && state.accounts.some((a) => a.kind === p.id));

/**
 * Subscription logins that are off unless the install opts in. The backend has
 * the provider compiled in and can still import and route it; this only decides
 * whether it is offered on screen.
 *
 * The opt-in is a file named `ALLOW_CLAUDE` next to the executable, read once
 * into `claudeAllowed` at boot (and enforced again in `add_account`, which is
 * the real gate — hiding a control is not). Without it, no part of the app hints
 * that connecting one was ever possible.
 *
 * Filter what is *shown*, not what is *ready*: `isConnected` and the composer's
 * "is this model usable" check still see the real providers, so a chat already
 * on one of these models keeps working, and accounts imported before the opt-in
 * was removed are hidden rather than dropped.
 */
const OPT_IN_SUBSCRIPTIONS = new Set(["claude"]);
const subscriptionHidden = (kind: string) => OPT_IN_SUBSCRIPTIONS.has(kind) && !state.claudeAllowed;
export const shownProviders = (list: ProviderView[]) => list.filter((p) => !(p.account && subscriptionHidden(p.id)));
export const shownAccounts = (list: AccountView[]) => list.filter((a) => !subscriptionHidden(a.kind));

/** Default route for a model: a route naming it as a head beats an "all models" route; else none. */
export function defaultRoute(model: string): string {
  const routes = state.settings?.routes ?? [];
  return (routes.find((r) => r.heads?.includes(model)) ?? routes.find((r) => r.all))?.id ?? "";
}

const draftTimers: Record<string, ReturnType<typeof setTimeout>> = {};
/** Save a composer draft to disk (debounced; empty = delete now). key = task id or "new-chat". */
export function persistDraft(key: string, text: string) {
  clearTimeout(draftTimers[key]);
  if (!text.trim()) { void api.draftSet(key, "").catch(() => {}); return; }
  draftTimers[key] = setTimeout(() => void api.draftSet(key, text).catch(() => {}), 400);
}

// ───────────────────────── saved prompts ─────────────────────────

// Stable identity for a fresh entry, so re-renders never churn ids.
let savedSeq = 0;
const savedId = () => `s${Date.now().toString(36)}${(savedSeq++).toString(36)}`;

/** Park what's in the new-task composer — text, images and every option — for later. */
export async function savePrompt() {
  const s = get();
  const text = s.draft.trim();
  if (!text) return flash("Write a prompt to save first");
  const h = s.home;
  const p: SavedPrompt = {
    id: savedId(), text, project: s.settings?.project ?? "",
    model: h.model, route: h.route, assist: h.assist, perm: h.perm, effort: h.effort,
    plan: h.plan, ultra: h.ultra, ultra_wt: h.ultra_wt, worktree: h.worktree, branch: h.branch,
    agents: h.agents, images: s.attach["new-chat"] ?? [], files: s.attachFiles["new-chat"] ?? [], created_at: new Date().toISOString(),
  };
  // Leave the composer clean: the prompt now lives in the saved list.
  set({ draft: "", attach: { ...s.attach, "new-chat": [] }, attachFiles: { ...s.attachFiles, "new-chat": [] } });
  persistDraft("new-chat", "");
  const saved = [...(s.settings?.saved_prompts ?? []), p];
  set((st) => ({ settings: { ...st.settings!, saved_prompts: saved } }));
  // `saveSettings` reports its own failure, so this only adds the good news.
  if (await saveSettings({ saved_prompts: saved })) flash("Saved · Ctrl N to start something else, it's in Saved prompts");
}

/**
 * Open a project folder, the way every entry point in the app does it: make it
 * the current project, remember it in the project list, and load its git /
 * memory / agent catalogs. `false` means the folder isn't there, or the settings
 * write refused it — the prompt is kept in the composer either way.
 *
 * The existence check is the one thing `loadProject` doesn't do: it reports a
 * missing folder as a plain project with no git and no instructions, which
 * looks like it worked until the task is created in a directory that isn't
 * there. Auto-opening a saved prompt is the one path where the folder can have
 * gone away since the prompt was parked, so that has to be said out loud.
 */
export async function openProject(path: string): Promise<boolean> {
  if (!path) return false;
  const s = get().settings;
  // Nothing here rejects: the settings write reports itself, `api.project` is
  // the only call that can throw, and a call site with no `.catch` of its own
  // would turn that into an unhandled rejection instead of a missing folder.
  // A refusal (an SSH host that can't be reached, say) is reported as itself:
  // "isn't there any more" would send the user looking for a folder that is fine.
  let failed = "";
  const info = await api.project(path).catch((e) => { failed = String(e); return null; });
  if (!info?.exists) {
    flash(failed ? `Couldn't open ${baseName(path)}: ${failed}` : `${baseName(path)} isn't there any more · pick another folder to run this in`);
    return false;
  }
  // Remember it, or a folder arrived at only from a saved prompt stays invisible
  // in the project menu afterwards. Newest first, same cap as the folder picker.
  const projects = [path, ...(s?.projects ?? []).filter((p) => normPath(p) !== normPath(path))].slice(0, 12);
  if (!(await saveSettings({ project: path, projects }))) return false;
  // Ask only when opening a folder, never during boot or metadata refresh.
  // Both trust and refusal are persisted by the dialog.
  set({ trustPrompt: !info.decided });
  await loadProject(path);
  return true;
}

/** Load a saved prompt into the new-task composer, options and all, and take it off the list.
 *  Returns false only when it couldn't be loaded at all — a prompt parked for a
 *  different folder opens that folder first rather than being refused. */
export async function loadPrompt(p: SavedPrompt) {
  const s = get();
  const cur = s.settings?.project ?? "";
  // A prompt with no folder of its own belongs wherever you are. One written for
  // another folder drags the app there, so pressing Enter on it can't quietly
  // run in the wrong checkout.
  if (p.project && cur && normPath(p.project) !== normPath(cur) && !(await openProject(p.project))) return false;
  set((st) => ({
    home: {
      ...st.home, plan: p.plan, ultra: p.ultra, ultra_wt: p.ultra_wt, perm: p.perm,
      model: p.model || st.home.model, effort: p.effort ?? st.home.effort, assist: p.assist,
      worktree: p.worktree, docker: p.worktree ? false : st.home.docker, branch: p.branch || st.home.branch,
      agents: withRequiredAgents(p.agents.length ? p.agents : st.home.agents), route: p.route,
    },
    draft: p.text,
    attach: { ...st.attach, "new-chat": p.images },
    attachFiles: { ...st.attachFiles, "new-chat": p.files ?? [] },
  }));
  persistDraft("new-chat", p.text);
  const rest = (s.settings?.saved_prompts ?? []).filter((x) => x.id !== p.id);
  set((st) => ({ settings: { ...st.settings!, saved_prompts: rest } }));
  await saveSettings({ saved_prompts: rest });
  go("home");
  return true;
}

/** Drop a saved prompt without loading it. */
export async function dropPrompt(p: SavedPrompt) {
  const rest = (get().settings?.saved_prompts ?? []).filter((x) => x.id !== p.id);
  set((s) => ({ settings: { ...s.settings!, saved_prompts: rest } }));
  await saveSettings({ saved_prompts: rest });
}
