// Best-of-N race: the frontend's own small store and the pure helpers around it.
//
// A race is one prompt run N times, each attempt its own chat in its own git
// worktree, then checked and ranked by a judge (see `raceApi` in api.ts). The
// races themselves are backend state, mirrored here from `race_list` and the
// `ol://race` event. The composer's race options are transient UI state: they
// are not settings, and are reset once a race has started.
//
// Kept out of `store.ts` on purpose: that store is shared by every screen and
// edited on many branches, and nothing outside the race UI reads these fields.
import { useSyncExternalStore } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { MAX_RACE_ATTEMPTS, MIN_RACE_ATTEMPTS, raceApi, type NewRace, type Race, type RaceAttemptSpec, type RaceEvent, type TaskSummary } from "./api";
import { back, flash, get as getApp, go, set as setApp } from "./store";

/** The composer's race options. `attempts`/`judge` null means "follow the
 *  composer's own model and effort", so a model switched after turning race on
 *  is the one the attempts run on unless the user picked them by hand. */
export interface RaceConfig { on: boolean; n: number; attempts: RaceAttemptSpec[] | null; judge: RaceAttemptSpec | null;
  /** Where each attempt works: a git worktree (default) or a docker workspace. */
  workspace?: "worktree" | "docker" }

export interface RaceStore {
  races: Record<string, Race>;
  /** Race whose comparison view is open. */
  open: string | null;
  /** The composer's race setup dialog. */
  setup: boolean;
  config: RaceConfig;
  /** Sidebar race groups the user folded away. */
  collapsed: Record<string, true>;
}

export const DEFAULT_RACE_N = 3;
const freshConfig = (): RaceConfig => ({ on: false, n: DEFAULT_RACE_N, attempts: null, judge: null, workspace: "worktree" });

let state: RaceStore = { races: {}, open: null, setup: false, config: freshConfig(), collapsed: {} };
const subs = new Set<() => void>();

export const getRaces = () => state;
export function setRaces(patch: Partial<RaceStore> | ((s: RaceStore) => Partial<RaceStore>)) {
  const p = typeof patch === "function" ? patch(state) : patch;
  if (!(Object.keys(p) as (keyof RaceStore)[]).some((k) => !Object.is(state[k], p[k]))) return;
  state = { ...state, ...p };
  subs.forEach((f) => f());
}
function subscribe(f: () => void) {
  subs.add(f);
  return () => { subs.delete(f); };
}
export function useRaces<T>(sel: (s: RaceStore) => T): T {
  return useSyncExternalStore(subscribe, () => sel(state), () => sel(state));
}
/** Tests only: back to an empty store. */
export function resetRaces() {
  state = { races: {}, open: null, setup: false, config: freshConfig(), collapsed: {} };
  subs.forEach((f) => f());
}

// ── pure helpers ───────────────────────────────────────────────────────────

/** N is 2..6: one attempt is just a chat, and the backend refuses more than six. */
export function clampRaceN(n: number): number {
  const r = Math.round(Number.isFinite(n) ? n : MIN_RACE_ATTEMPTS);
  return Math.min(MAX_RACE_ATTEMPTS, Math.max(MIN_RACE_ATTEMPTS, r));
}

/** The attempt list a send would use: what the user picked, padded with the
 *  composer's model/effort and cut to N. */
export function resolveAttempts(cfg: RaceConfig, model: string, effort: number): RaceAttemptSpec[] {
  const picked = cfg.attempts ?? [];
  return Array.from({ length: clampRaceN(cfg.n) }, (_, i) => picked[i] ?? { model, effort });
}

export function resolveJudge(cfg: RaceConfig, model: string, effort: number): RaceAttemptSpec {
  return cfg.judge ?? { model, effort };
}

/** Fold one `ol://race` payload into the map. */
export function applyRaceEvent(races: Record<string, Race>, e: RaceEvent): Record<string, Race> {
  if ("removed" in e && e.removed) {
    if (!(e.id in races)) return races;
    const next = { ...races };
    delete next[e.id];
    return next;
  }
  const race = e as Race;
  return races[race.id] === race ? races : { ...races, [race.id]: race };
}

/** Fold a `race_list` snapshot in. The event listener is live before the
 *  snapshot is asked for, so a race an event already delivered is newer than
 *  the snapshot's copy and is kept. */
export function mergeRaceSnapshot(races: Record<string, Race>, list: Race[]): Record<string, Race> {
  const next = { ...races };
  for (const r of list) if (!(r.id in next)) next[r.id] = r;
  return next;
}

/** task id → race id, for every attempt of every race. */
export function raceIndex(races: Record<string, Race>): Map<string, string> {
  const m = new Map<string, string>();
  for (const r of Object.values(races)) for (const a of r.attempts) m.set(a.task_id, r.id);
  return m;
}

export type RaceRow<T> = { kind: "task"; t: T } | { kind: "race"; race: Race; tasks: T[] };

/** Group a sidebar list: a race's attempts collapse into one entry, placed
 *  where the first of them would have been, holding them in attempt order.
 *  Chats in no race pass through untouched and in their own order. */
export function groupRaceRows<T extends { id: string }>(list: T[], allRaces: Record<string, Race>): RaceRow<T>[] {
  // An archived race has no group: a chat restored on its own reads as a plain chat.
  const races = Object.fromEntries(Object.entries(allRaces).filter(([, r]) => !r.archived));
  const idx = raceIndex(races);
  if (!idx.size) return list.map((t) => ({ kind: "task", t }));
  const out: RaceRow<T>[] = [];
  const groups = new Map<string, { kind: "race"; race: Race; tasks: T[] }>();
  for (const t of list) {
    const rid = idx.get(t.id);
    const race = rid ? races[rid] : undefined;
    if (!race) { out.push({ kind: "task", t }); continue; }
    let g = groups.get(race.id);
    if (!g) { g = { kind: "race", race, tasks: [] }; groups.set(race.id, g); out.push(g); }
    g.tasks.push(t);
  }
  for (const g of groups.values()) {
    const order = new Map(g.race.attempts.map((a, i) => [a.task_id, i]));
    g.tasks.sort((a, b) => (order.get(a.id) ?? 0) - (order.get(b.id) ?? 0));
  }
  return out;
}

/** The backend refuses a judge run past the monthly cap with "budget:…". */
export const isBudgetError = (e: unknown) => String(e).replace(/^Error:\s*/, "").startsWith("budget:");
/** The readable part of a budget refusal. */
export const budgetMessage = (e: unknown) => String(e).replace(/^Error:\s*/, "").replace(/^budget:\s*/, "");

export const RACE_STATE: Record<Race["state"], { label: string; color: string }> = {
  running: { label: "Running", color: "var(--st-run)" },
  checking: { label: "Checking", color: "var(--ultra)" },
  judging: { label: "Judging", color: "var(--ultra)" },
  judged: { label: "Judged", color: "var(--green)" },
  failed: { label: "Failed", color: "var(--red)" },
};

/** How long an attempt has run: until its last update once it has settled,
 *  until now while it is still going. Milliseconds, never negative. */
export function attemptElapsed(t: Pick<TaskSummary, "status" | "created_at" | "updated_at">, now = Date.now()): number {
  const start = new Date(t.created_at).getTime();
  const live = t.status === "running" || t.status === "waiting";
  const end = live ? now : new Date(t.updated_at).getTime();
  return Number.isFinite(start) && Number.isFinite(end) ? Math.max(0, end - start) : 0;
}

export function fmtDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** Attempts whose chat has settled (not running or waiting on you). A deleted chat counts: it is no longer going. */
export function attemptsFinished(race: Race, tasks: Record<string, Pick<TaskSummary, "status">>): number {
  return race.attempts.filter((a) => { const t = tasks[a.task_id]; return !t || (t.status !== "running" && t.status !== "waiting"); }).length;
}

/** Everything the race has cost so far: the attempts still in the store plus the judge. */
export function raceCost(race: Race, tasks: Record<string, Pick<TaskSummary, "usage">>): number {
  return race.attempts.reduce((n, a) => n + (tasks[a.task_id]?.usage.cost ?? 0), 0) + (race.judge?.cost ?? 0);
}

// ── actions ────────────────────────────────────────────────────────────────

export function upsertRace(race: Race) {
  setRaces((s) => ({ races: applyRaceEvent(s.races, race) }));
}

export const openRace = (id: string) => { setRaces({ open: id }); go("race"); };
export const closeRace = () => {
  setRaces({ open: null });
  const h = getApp().hist;
  if (h.length && h[h.length - 1]?.view !== "race") back(); else go("home");
};

/** Race is for new tasks in a git project only, like ultrathread worktrees. */
export function raceAvailable(): string | null {
  const s = getApp();
  if (s.view !== "home") return "Race starts new tasks · open New task first";
  if (!s.projectInfo?.git) return "Race needs a git repo";
  return null;
}

/** `/race` and the + menu: turning it on opens the setup. */
export function toggleRace() {
  if (state.config.on) {
    setRaces((s) => ({ config: { ...s.config, on: false }, setup: false }));
    flash("Race off");
    return;
  }
  const why = raceAvailable();
  if (why) { flash(why); return; }
  setRaces((s) => ({ config: { ...s.config, on: true }, setup: true }));
}

/** True when a home-composer send should start a race instead of one chat. */
export const raceArmed = () => state.config.on;

/** Start the race the composer is configured for. Everything but the attempts
 *  and the judge comes from the caller, which builds it exactly as a normal
 *  new task would be built. Adds the new chats to the store and, unless
 *  `stay`, opens the comparison view. Throws whatever the backend refused with. */
export async function startRace(req: Omit<NewRace, "attempts" | "judge_model" | "judge_effort">, model: string, effort: number, stay = false) {
  if (!getApp().projectInfo?.git) throw new Error("Race needs a git repo");
  const cfg = state.config;
  const judge = resolveJudge(cfg, model, effort);
  const r = await raceApi.create({ ...req, workspace: cfg.workspace ?? "worktree", attempts: resolveAttempts(cfg, model, effort), judge_model: judge.model, judge_effort: judge.effort });
  setApp((st) => ({ tasks: { ...st.tasks, ...Object.fromEntries(r.tasks.map((t) => [t.id, t])) } }));
  // One-shot, like plan mode: the next new task is a plain one again. The
  // picked models are kept, so a second race starts from the same lineup.
  setRaces((s) => ({ races: applyRaceEvent(s.races, r.race), config: { ...s.config, on: false }, setup: false, open: stay ? s.open : r.race.id }));
  return r;
}

// ── backend events ─────────────────────────────────────────────────────────

/** Attach the `ol://race` listener, then fold in a `race_list` snapshot. Called
 *  once from the store's listener set, so it shares its detach and its guard
 *  against attaching twice. */
export function listenRaces(): Promise<UnlistenFn> {
  return Promise.resolve(listen<RaceEvent>("ol://race", ({ payload }) => {
    if (!payload || typeof payload !== "object" || typeof payload.id !== "string") return;
    setRaces((s) => {
      const races = applyRaceEvent(s.races, payload);
      const gone = "removed" in payload && payload.removed;
      return { races, ...(gone && s.open === payload.id ? { open: null } : {}) };
    });
  })).then((off) => {
    void refreshRaces();
    return off;
  });
}

export async function refreshRaces() {
  try {
    const list = await raceApi.list();
    if (Array.isArray(list)) setRaces((s) => ({ races: mergeRaceSnapshot(s.races, list) }));
  } catch {
    // An older backend has no races; the sidebar simply shows none.
  }
}

/** Archive a race with its attempt chats, or restore it. Archiving offers Undo, like archiving a chat. */
export async function archiveRace(id: string, archived = true) {
  try {
    const race = await raceApi.archive(id, archived);
    setRaces((s) => ({ races: applyRaceEvent(s.races, race), ...(archived && s.open === id ? { open: null } : {}) }));
    if (archived) flash("Race archived", { label: "Undo", run: () => { void archiveRace(id, false); } });
    else flash("Race restored");
    return true;
  } catch (e) {
    flash(String(e));
    return false;
  }
}

/** Open an attempt's worktree folder in the system file manager. */
export function openWorktree(cwd: string) {
  raceApi.openFolder(cwd).catch((e) => flash(String(e)));
}
