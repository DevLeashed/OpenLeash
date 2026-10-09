import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import type { DragDropEvent } from "@tauri-apps/api/window";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { api, ASSISTS, Assist, clampEffort, countOptionalAgents, defaultX, DEFAULT_PERM, EFFORTS, effortLabel, effortSteps, FileRef, fmt$, fmtK, Item, normPath, Perm, PERMS, TaskPatch, TaskSummary, UltraX, withRequiredAgents, xDepth, xOn } from "../api";
import { defaultRoute, flash, get, go, isConnected, Menu, modelInfo, persistDraft, savePrompt, saveSettings, set, shownProviders, type State, useStore, zoom } from "../store";
import { I } from "./icons";
import { annotationBlock } from "./SelectionNote";
import { ProviderIcon } from "./ProviderIcon";
import { Tooltip, IconButton, MenuButton, MenuRow, MenuScrim, Pressable, RoundButton, TextArea, Thumb, cycle } from "./primitives";

/** Ultrathread X is still being built, so it stays out of the menus and the slash
 *  list. Flip this back to true to show it again. The command still runs if typed
 *  by hand — hiding it is about discoverability, not about disabling it. */
export const ULTRA_X_SHOWN = false;

export const INIT_PROMPT =
  "Prepare concise project instructions for a future coding agent. First inspect the repository, manifests, docs, and existing instruction files to verify build/test/lint commands (including how to run a single test), high-level architecture, and project conventions. Do not ask me about anything you can discover in the code or existing docs. Before editing, interview me with one short `ask_user` form containing only material project-specific questions the repository cannot answer, with no more than five questions; skip the interview if no such questions remain. Check `OPENLEASH.md`, `AGENTS.md`, `CLAUDE.md`, and `.openleash/instructions.md`: if any exists, update the most relevant existing file in place, preserve its useful content, and do not replace it wholesale or create a duplicate. If multiple existing files conflict or the right target is genuinely unclear, ask me which to maintain. Create root `OPENLEASH.md` only if none exists. Keep the result concise and include only verified facts and my answers.";

const EMPTY_COMMANDS: { name: string; description: string; prompt: string }[] = [];

export function expandCustomCommand(text: string, commands: { name: string; prompt: string }[]): string {
  const match = text.match(/^\/([a-z0-9-]+)(?:\s+([\s\S]*))?$/i);
  if (!match) return text;
  const builtin = ["/goal", "/plan", "/ultra", "/ultrawt", "/ultrax", "/init", "/compact", "/btw", "/model", "/effort", "/assist", "/perm", "/pause", "/resume", "/cost", "/stop", "/models", "/details", "/save", "/saved", "/paused", "/skills", "/accounts", "/mcp", "/settings", "/clear"].includes(`/${match[1]?.toLowerCase()}`);
  const command = builtin ? undefined : commands.find((entry) => entry.name === match[1]?.toLowerCase());
  if (!command) return text;
  return command.prompt.replace(/{{args}}/g, match[2] ?? "").trim();
}

export interface Slash { cmd: string; args?: string; desc: string; where?: "home" | "session" }
export const SLASH: Slash[] = [
  { cmd: "/goal", args: "<what done looks like>", desc: "Keep working until the goal is achieved and verified" },
  { cmd: "/plan", desc: "Toggle plan mode: propose before editing" },
  { cmd: "/ultra", desc: "Toggle ultrathread: many nested subagents, works until everything's done" },
  { cmd: "/ultrawt", desc: "Toggle ultrathread worktrees: each worker gets its own git worktree, a fuze agent merges them back" },
  ...(ULTRA_X_SHOWN ? [{ cmd: "/ultrax", desc: "Configure ultrathread X: a per-layer ladder of subagent models up to 6 deep" }] : []),
  { cmd: "/init", desc: "Interview you before writing project instructions" },
  { cmd: "/compact", args: "[focus]", desc: "Summarise history to free context", where: "session" },
  { cmd: "/btw", args: "<question>", desc: "Ask a quick side question · answered without interrupting the agent, and not added to the chat", where: "session" },
  { cmd: "/model", args: "[name]", desc: "Switch model" },
  { cmd: "/effort", args: "<level>", desc: "Set thinking effort · the levels this model has" },
  { cmd: "/assist", args: "<guide|default|necessary>", desc: "How much the agent asks you" },
  { cmd: "/perm", args: "<ask|auto|full>", desc: "Set permission mode" },
  { cmd: "/pause", desc: "Freeze this task and all its subagents", where: "session" },
  { cmd: "/resume", args: "[message to all agents]", desc: "Unpause", where: "session" },
  { cmd: "/all", args: "<message to every working agent>", desc: "Broadcast · each agent ignores it unless it's about their own work", where: "session" },
  { cmd: "/review", desc: "Review and commit changes", where: "session" },
  { cmd: "/cost", desc: "Cost and context for this task", where: "session" },
  { cmd: "/stop", desc: "Interrupt the agent", where: "session" },
  { cmd: "/models", desc: "Swap the models in use (this chat, or everywhere from home)" },
  { cmd: "/details", desc: "Toggle the details panel" },
  { cmd: "/save", desc: "Save this prompt for later" },
  { cmd: "/saved", desc: "Prompts you saved to start later" },
  { cmd: "/paused", desc: "Every chat that is paused" },
  { cmd: "/skills", desc: "Manage agent skills" },
  { cmd: "/accounts", desc: "Subscription accounts and usage" },
  { cmd: "/mcp", desc: "MCP servers" },
  { cmd: "/settings", desc: "Open settings" },
  { cmd: "/clear", desc: "Start a new task" },
];

const EFFORT_ALIASES: Record<string, number> = { max: 0, xhigh: 1, extra: 1, "extra-high": 1, high: 2, medium: 3, med: 3, low: 4 };

export interface Opts { plan: boolean; ultra: boolean; ultra_wt: boolean; ultra_x: UltraX | null; perm: Perm; model: string; effort: number; assist: Assist; agents: string[] }

export function currentTask() {
  const s = get();
  return s.view !== "home" && s.task && s.tasks[s.task] ? s.tasks[s.task] : null;
}

export function currentOpts(): Opts {
  const t = currentTask();
  if (t) return { plan: t.plan, ultra: !!t.ultra, ultra_wt: !!t.ultra_wt, ultra_x: t.ultra_x ?? null, perm: t.perm, model: t.model, effort: t.effort, assist: t.assist, agents: t.agents };
  return get().home;
}

/** Result of an options change: did it stick? `task` is the chat's new
 *  summary, or null for the new-chat defaults. */
export type OptsResult = { ok: boolean; task: TaskSummary | null };

/** Update the task (or the new-chat defaults). Mid-run, model/assist/agents
 *  changes wait for the next message unless `now` is set. Effort is not one of
 *  them: it lands on the agent's next request whatever `now` says.
 *
 *  Never rejects. A change to a running chat is a `task_update` round-trip, and
 *  when the backend turns it down (a malformed `ultra_x` ladder, a model the
 *  provider doesn't have) the composer would otherwise keep showing a state
 *  nothing accepted. So the failure is reported and undone here, in the one
 *  place all the composer toggles go through, and reported back as `ok: false`
 *  so a caller that wants to announce success can wait for it. */
export async function setOpts(patch: TaskPatch): Promise<OptsResult> {
  // `explore` + `general` are always on: re-add them to any agents change.
  if (patch.agents) patch = { ...patch, agents: withRequiredAgents(patch.agents) };
  if (patch.subagents === false) {
    flash("Explore and General are always on");
    return { ok: false, task: null };
  }
  const t = currentTask();
  if (t) {
    let next: TaskSummary;
    try {
      next = await api.update(t.id, patch);
    } catch (e) {
      // Put the last known server state back, so the toggle springs back to
      // what the agent is really running rather than to an untruth.
      set((st) => (st.tasks[t.id] ? { tasks: { ...st.tasks, [t.id]: t } } : {}));
      flash(String(e));
      return { ok: false, task: null };
    }
    set((st) => ({ tasks: { ...st.tasks, [next.id]: next } }));
    return { ok: true, task: next };
  }
  // New-chat permission choices are defaults, not just a transient toggle:
  // boot restores them from settings. Leave the UI unchanged if saving fails.
  if (patch.perm !== undefined && !await saveSettings({ perm: patch.perm })) {
    return { ok: false, task: null };
  }
  const { now: _n, apply_pending: _a, ...rest } = patch;
  if (rest.agents) rest.subagents = rest.agents.length > 0;
  set((st) => ({ home: { ...st.home, ...(rest as object) } }));
  return { ok: true, task: null };
}

/** Tell the user whether a change landed now or waits for their next message. */
export function afterChange(label: string, now: boolean) {
  const t = currentTask();
  const live = t && (t.status === "running" || t.status === "waiting");
  if (live && !now) flash(`${label} · applies with your next message (Shift-click to apply now)`);
}

export function togglePlan() {
  const on = !currentOpts().plan;
  void setOpts({ plan: on });
}

/** Ultrathread: long-haul orchestration with nested subagents. */
export function toggleUltra() {
  const o = currentOpts();
  // Turning plain ultrathread on clears any X ladder, and vice versa: one mode at a time.
  const on = !o.ultra || xOn(o.ultra_x);
  const done = on ? "Ultrathread on · the agent fans out subagents and keeps going until the list is done" : "Ultrathread off";
  // Announce after the write lands: a chat that won't accept it flashes why instead.
  void setOpts(on ? { ultra: true, ultra_x: null } : { ultra: false, ultra_wt: false, ultra_x: null }).then((r) => { if (r.ok) flash(done); });
}

/** Ultrathread worktrees: every worker in its own git worktree, merged back by the fuze agent. */
export function toggleUltraWt() {
  const s = get();
  if (!currentTask() && !s.projectInfo?.git) return flash("Ultrathread worktrees need a git repo");
  const on = !currentOpts().ultra_wt;
  const done = on ? "Ultrathread worktrees on · each worker gets its own branch, the fuze agent merges them" : "Ultrathread worktrees off · workers share the checkout again";
  void setOpts(on ? { ultra: true, ultra_wt: true } : { ultra_wt: false }).then((r) => { if (r.ok) flash(done); });
}

/** Ultrathread X: a ladder of subagent layers, each with its own model, effort and fanout.
 *  Turning it on keeps any worktree choice, since X + worktrees is a real combination. */
export function toggleUltraX() {
  const o = currentOpts();
  const on = !xOn(o.ultra_x);
  if (!on) {
    void setOpts({ ultra_x: null, ultra: !!o.ultra, ultra_wt: !!o.ultra_wt }).then((r) => { if (r.ok) flash("Ultrathread X off"); });
    return;
  }
  // Start from a sane ladder rather than six empty rows: the orchestrator's own
  // model plans, a cheaper model does the work, and the leaf layer is uncapped.
  const base = o.ultra_x ?? defaultX();
  const x: UltraX = { ...base, wt: !!o.ultra_wt, layers: base.layers.map((l, i) => (i === 0 ? { ...l, model: o.model } : l)) };
  void setOpts({ ultra: true, ultra_x: x }).then((r) => { if (r.ok) flash(`Ultrathread X on · ${xDepth(x)} layers, each with its own model, effort and fanout`); });
}

/** Open the ladder editor for the current chat (or the new-chat defaults). */
export function openUltraXEditor() {
  const o = currentOpts();
  // Turning X on from the menu with no ladder yet starts on the default shape.
  if (!xOn(o.ultra_x) && !o.ultra) {
    const base = defaultX();
    void setOpts({ ultra: true, ultra_x: { ...base, layers: base.layers.map((l, i) => (i === 0 ? { ...l, model: o.model } : l)) } });
  }
  set({ ultraXOpen: true, menu: null, pal: false });
}
/** After a stop: the agent needs to know whether to pick the work back up or wrap it up. */
function ResumeChoice({ id }: { id: string }) {
  const [open, setOpen] = useState(false);
  const go = (mode: "continue" | "wrap") => { setOpen(false); api.resume(id, undefined, mode).catch((e) => flash(String(e))); };
  return (
    <span style={{ position: "relative", display: "inline-flex" }}>
      <RoundButton label="Resume · continue or wrap up" className="cont" onClick={() => setOpen(!open)}>{I.play(10)}</RoundButton>
      {open && (
        <>
          <MenuScrim onClick={() => setOpen(false)} />
          <div className="pop umenu" style={{ width: 280 }}>
            <div className="mhead">This chat was stopped</div>
            <MenuRow onClick={() => go("continue")}>
              <div style={{ display: "flex", flexDirection: "column", gap: 1 }}>
                <span>Continue</span>
                <span style={{ fontSize: 11, color: "var(--mut3)" }}>Retry what got cut off and carry on (subagents too)</span>
              </div>
            </MenuRow>
            <MenuRow onClick={() => go("wrap")}>
              <div style={{ display: "flex", flexDirection: "column", gap: 1 }}>
                <span>Wrap up</span>
                <span style={{ fontSize: 11, color: "var(--mut3)" }}>Don't continue · summarise what got done and what's left</span>
              </div>
            </MenuRow>
          </div>
        </>
      )}
    </span>
  );
}

export function openMenu(menu: Menu, e: React.MouseEvent | { currentTarget: EventTarget | null }) {
  set({ menu, pickFor: null, menuAnchor: (e.currentTarget as HTMLElement).getBoundingClientRect() });
}

/** Commands the UI handles itself. Returns true if handled. */
function localSlash(text: string, mode: "home" | "session"): boolean {
  const [cmd, ...restParts] = text.split(/\s+/);
  const rest = restParts.join(" ").trim();
  const s = get();
  const task = mode === "session" && s.task ? s.tasks[s.task] : null;
  switch (cmd) {
    case "/plan": togglePlan(); return true;
    case "/ultra": toggleUltra(); return true;
    case "/ultrawt": toggleUltraWt(); return true;
    case "/ultrax": case "/ultrax/": openUltraXEditor(); return true;
    case "/details": set({ details: !s.details }); return true;
    case "/agents": go("saved"); return true;
    case "/saved": go("saved"); return true;
    case "/paused": go("paused"); return true;
    case "/save":
      if (mode === "session") { flash("Only new-task prompts can be saved"); return true; }
      void savePrompt();
      return true;
    case "/skills": go("settings", { settingsTab: "skills" }); return true;
    case "/settings": go("settings"); return true;
    case "/accounts": go("settings", { settingsTab: "accounts" }); return true;
    case "/mcp": go("settings", { settingsTab: "mcp" }); return true;
    case "/clear": case "/new": go("home"); return true;
    case "/review": if (task) go("diff"); return true;
    case "/stop": if (task) api.interrupt(task.id).catch((e) => flash(String(e))); return true;
    case "/models": set({ swap: true }); return true;
    case "/cost":
      if (task) flash(`${fmt$(task.usage.cost)} · ${fmtK(task.usage.input + task.usage.output + task.usage.cache_read)} tokens · context ${fmtK(task.usage.last_context)}/${fmtK(task.context_window)}`);
      return true;
    case "/all": case "/broadcast": {
      if (!task) { flash("Open a chat with agents working first"); return true; }
      if (!rest) { flash("Say what to send, e.g. /all the lighting is bad, undo it"); return true; }
      api.broadcast(task.id, rest).then((r) => flash(r)).catch((e) => flash(String(e)));
      return true;
    }
    case "/effort": {
      const r = rest.toLowerCase();
      // Only the levels this model really has are valid, so a model that tops
      // out at Max lists that one level instead of five rungs.
      const mi = modelInfo(currentOpts().model);
      const steps = effortSteps(mi);
      // `steps` only ever holds positions from EFFORTS, so there is always a
      // generic name for a level this model has none of its own for.
      const name = (s: number) => effortLabel(mi, s) ?? EFFORTS[s]!;
      const i = steps.find((s) => name(s).toLowerCase() === r) ?? EFFORT_ALIASES[r];
      if (i === undefined || !steps.includes(i)) {
        flash(steps.length ? `Effort for ${mi.name}: ${steps.map(name).join(", ")}` : `${mi.name} has no effort control`);
        return true;
      }
      void setOpts({ effort: clampEffort(steps, i) }); return true;
    }
    case "/assist": {
      const a = ASSISTS.find((x) => x.id.startsWith(rest.toLowerCase()) && rest);
      if (!a) { flash("Assist: guide, default or necessary"); return true; }
      void setOpts({ assist: a.id, now: true }); return true;
    }
    case "/perm": case "/permissions": {
      const r = rest.toLowerCase();
      const p = r ? PERMS.find((x) => x.id === r || x.name.toLowerCase().startsWith(r)) : undefined;
      if (!p) { flash("Permission: ask, auto or full"); return true; }
      void setOpts({ perm: p.id }); return true;
    }
    case "/model": {
      if (!rest) { set({ menu: "model", pickFor: null, menuAnchor: document.querySelector(".composer .mbtn")?.getBoundingClientRect() ?? null }); return true; }
      const q = rest.toLowerCase();
      // Prefer your own models and routes, then providers you can actually call.
      const usable = (p: string) => s.providers.find((x) => x.id === p && isConnected(x) && x.enabled) ? 1 : 0;
      const routes = (s.settings?.routes ?? []).map((r) => modelInfo("route/" + r.id));
      const all = [...routes, ...s.models.filter((m) => m.enabled !== false)];
      const hits = all
        .filter((x) => (x.name + " " + x.id).toLowerCase().includes(q))
        .sort((a, b) => Number(b.custom) - Number(a.custom) || usable(b.provider) - usable(a.provider));
      const m = all.find((x) => x.id.toLowerCase() === q) ?? hits[0];
      const setModel = (id: string) => void setOpts({ model: id, route: defaultRoute(id) });
      if (m) setModel(m.id);
      else if (q.includes("/")) setModel(q);
      else flash(`No model matches "${rest}"`);
      return true;
    }
  }
  return false;
}

const NO_IMAGES: string[] = [];
const NO_FILES: State["attachFiles"][string] = [];

/** Short size for an attachment chip: "1.2 MB". */
export const humanBytes = (n: number) => (n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${Math.round(n / 1024)} KB` : n < 1024 * 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)} MB` : `${(n / 1024 / 1024 / 1024).toFixed(1)} GB`);
/** Enough files to be useful without turning the composer into a file list. */
export const MAX_FILES = 12;
/** The cap on one pasted image, shared with the message editor. */
export const MAX_IMAGE = 5 * 1024 * 1024;
export const MAX_IMAGES = 8;

/**
 * Did this window-level drag land on the composer? The window reports one
 * position for the whole window, so hit-test it against the box.
 *
 * Two conversions, and both are load-bearing:
 *
 *  - `/dpr` because the window reports physical pixels and `getBoundingClientRect`
 *    reports CSS pixels.
 *  - `/zoom()` because the app root is `zoom`ed, and `getBoundingClientRect`
 *    reports *pre-zoom* CSS pixels for its coordinates. Without this the hit
 *    test misses the box at any zoom other than 100% — the composer sits at the
 *    bottom of the pane, so the error grows with the zoom and the drop lands
 *    nothing at all.
 *
 * `overComposer` is the only path that can attach a file: the browser's
 * `DataTransfer` never carries the path behind a dropped file, and Tauri's
 * handler only reports a position for the whole window, so the hit test is how
 * a dropped file knows which composer asked for it.
 */
export function overComposer(e: { payload: DragDropEvent }) {
  if (e.payload.type !== "enter" && e.payload.type !== "over" && e.payload.type !== "drop") return false;
  const el = document.querySelector(".composer")?.getBoundingClientRect();
  const p = e.payload.position;
  if (!el) return false;
  const dpr = window.devicePixelRatio || 1;
  const x = p.x / dpr / zoom(), y = p.y / dpr / zoom();
  return x >= el.left && x <= el.right && y >= el.top && y <= el.bottom;
}

/** Read image files (paste or drop) into the pending attachments for this composer. */
export function addImages(key: string, files: File[]) {
  const imgs = files.filter((f) => f.type.startsWith("image/"));
  for (const f of imgs) {
    if (f.size > MAX_IMAGE) { flash(`${f.name || "Image"} is over 5 MB`); continue; }
    const r = new FileReader();
    r.onload = () => {
      if (typeof r.result !== "string") return;
      if ((get().attach[key] ?? []).length >= MAX_IMAGES) return flash(`Only ${MAX_IMAGES} images at a time`);
      set((s) => ({ attach: { ...s.attach, [key]: [...(s.attach[key] ?? []), r.result as string] } }));
    };
    r.onerror = () => flash(`Couldn't read ${f.name || "image"}`);
    r.readAsDataURL(f);
  }
  return imgs.length > 0;
}

/** Add files the agent should read, by path.
 *
 *  The model can only *see* an image, so those are also inlined as image
 *  blocks; every other file is just a path, and the agent opens it with its
 *  own tools. This is the only way an attachment keeps its real path: a
 *  browser `File` never has one, so everything picked or dropped comes back
 *  through the `files_attach` command first. */
export async function addFiles(key: string, paths: string[]) {
  if (!paths.length) return;
  let refs: FileRef[];
  try {
    refs = await api.filesAttach(paths);
  } catch (e) {
    flash(String(e));
    return;
  }
  // The backend drops paths that aren't readable files, so say something when
  // that leaves nothing rather than appearing to ignore the drop.
  const skipped = paths.length - refs.length;
  if (skipped) flash(skipped === 1 ? "That isn't a file" : `${skipped} of those aren't files`);
  const duplicate = (p: string) => (get().attachFiles[key] ?? []).some((f) => normPath(f.path) === normPath(p));
  let dupes = 0;
  for (const ref of refs) {
    if (duplicate(ref.path)) { dupes++; continue; }
    if ((get().attachFiles[key] ?? []).length >= MAX_FILES) { flash(`Only ${MAX_FILES} files at a time`); break; }
    let imageDataUrl: string | undefined;
    if (ref.image) {
      if (ref.size > MAX_IMAGE) { flash(`${ref.name} is over 5 MB`); continue; }
      if ((get().attach[key] ?? []).length >= MAX_IMAGES) { flash(`Only ${MAX_IMAGES} images at a time`); continue; }
      try {
        imageDataUrl = await api.fileDataUrl(ref.path);
      } catch (e) {
        flash(String(e));
        continue; // No inert image ref: submit only sends successfully inlined images.
      }
    }
    // Paste, another drop, or a removal may have landed during conversion.
    // Commit the path and image together, rechecking both limits and dedupe.
    if (duplicate(ref.path)) { dupes++; continue; }
    if ((get().attachFiles[key] ?? []).length >= MAX_FILES) { flash(`Only ${MAX_FILES} files at a time`); break; }
    if (imageDataUrl !== undefined && (get().attach[key] ?? []).length >= MAX_IMAGES) { flash(`Only ${MAX_IMAGES} images at a time`); continue; }
    set((s) => ({
      ...(imageDataUrl !== undefined ? { attach: { ...s.attach, [key]: [...(s.attach[key] ?? []), imageDataUrl] } } : {}),
      attachFiles: { ...s.attachFiles, [key]: [...(s.attachFiles[key] ?? []), {
        ...ref, ...(imageDataUrl !== undefined ? { imageDataUrl, imageIndex: (s.attach[key] ?? []).length } : {}),
      }] },
    }));
  }
  if (dupes) flash(`${dupes} already attached`);
}

/** Saved prompts pass through Rust's FileRef, which intentionally has no UI
 *  metadata. Reconnect their paths by bytes, never by file/image array order.
 *  If a file changed or vanished, retain the saved image but discard its path. */
export async function resolveImageRefs(key: string) {
  for (const ref of get().attachFiles[key] ?? []) {
    if (!ref.image || ref.imageIndex !== undefined) continue;
    let src = ref.imageDataUrl;
    try { src ??= await api.fileDataUrl(ref.path); } catch (e) { flash(String(e)); }
    set((s) => {
      const files = s.attachFiles[key] ?? [];
      if (!files.includes(ref)) return {};
      const images = s.attach[key] ?? [];
      const index = images.findIndex((image, i) => image === src && !files.some((f) => f.imageIndex === i));
      return { attachFiles: { ...s.attachFiles, [key]: index < 0
        ? files.filter((f) => f !== ref)
        : files.map((f) => f === ref ? { ...f, imageDataUrl: src, imageIndex: index } : f) } };
    });
  }
}

/** Remove a thumbnail and its path together. Equal-byte pasted images are
 *  independent, so the association uses an explicit image index. */
export function removeImage(key: string, index: number) {
  set((s) => ({
    attach: { ...s.attach, [key]: (s.attach[key] ?? []).filter((_, i) => i !== index) },
    attachFiles: { ...s.attachFiles, [key]: (s.attachFiles[key] ?? [])
      .filter((f) => f.imageIndex !== index)
      .map((f) => f.imageIndex !== undefined && f.imageIndex > index ? { ...f, imageIndex: f.imageIndex - 1 } : f) },
  }));
}

export async function submit(mode: "home" | "session", later = false, stayOnHome = false) {
  const s = get();
  let text = (mode === "home" ? s.draft : s.sessionDrafts[s.task ?? ""] ?? "").trim();
  const originalDraft = mode === "home" ? s.draft : s.sessionDrafts[s.task ?? ""] ?? "";
  const key = mode === "home" ? "new-chat" : s.task ?? "";
  const images = s.attach[key] ?? [];
  const files = s.attachFiles[key] ?? [];
  const notes = mode === "home" ? [] : (s.notes[key] ?? []);
  if (!text && !images.length && !files.length && !notes.length) return;  // Warn against a model that can't take images, but still send: the *pending*
  // model is the one this message will actually be routed to mid-run.
  if (images.length) {
    const id = mode === "home" ? s.home.model : s.tasks[key]?.pending?.model ?? s.tasks[key]?.model ?? "";
    if (!modelInfo(id).input_types.includes("image")) flash("This model may not accept images");
  }
  const clear = () => {
    set((st) => ({ attach: { ...st.attach, [key]: [] }, attachFiles: { ...st.attachFiles, [key]: [] } }));
    persistDraft(key, "");
    return mode === "home" ? set({ draft: "" }) : set((st) => ({ sessionDrafts: { ...st.sessionDrafts, [key]: "" } }));
  };
  // Annotations travel with the message, not inside the box: they are notes the
  // user wrote beside the transcript, and are prepended here rather than typed
  // into the draft so a note can't be edited by accident or
  // left behind in the draft after it has been answered. They are cleared with
  // the rest of what the send consumed — a note that had been sent and stayed on
  // would go out again with the next message.
  const annotated = annotationBlock(notes);
  const takeNotes = () => set((st) => (st.notes[key]?.length ? { notes: { ...st.notes, [key]: [] } } : {}));
  const [, commandName] = text.match(/^\/([a-z0-9-]+)/i) ?? [];
  if (text === "/init") text = INIT_PROMPT;
  else if (commandName && (s.settings?.custom_commands ?? EMPTY_COMMANDS).some((command) => command.name === commandName.toLowerCase())) text = expandCustomCommand(text, s.settings?.custom_commands ?? EMPTY_COMMANDS);
  // What the user actually typed, before the attachment list goes on. A failed
  // send puts this back, not the decorated version.
  const typed = text;
  // Attached files ride along as paths in the text: the agent has read_file,
  // glob and friends, so a path is all it needs to get at the bytes. Images
  // are already inlined above, so they're left out of this list.
  if (files.length) {
    const paths = files.filter((f) => !f.image).map((f) => f.path);
    if (paths.length) text = (text ? text + "\n\n" : "") + "Attached files (read these with your tools, they are not inlined):\n" + paths.map((p) => `- ${p}`).join("\n");
  }
  if (text.startsWith("/") && localSlash(text, mode)) return clear();
  if (text === "/goal" && mode === "home") return flash("Say what done looks like, e.g. /goal all tests pass on Windows");
  if (mode === "home") {
    const project = s.settings?.project ?? "";
    if (!project) return flash("Pick a project folder first");
    clear();
    try {
      const h = s.home;
      const t = await api.create({ prompt: text, project, model: h.model, effort: h.effort, perm: h.perm, plan: h.plan, worktree: h.worktree && !!s.projectInfo?.git, base_branch: h.branch, subagents: h.agents.length > 0, assist: h.assist, agents: h.agents, route: h.route, images, ultra: h.ultra, ultra_wt: h.ultra_wt, ultra_x: h.ultra_x });
      set((st) => ({ tasks: { ...st.tasks, [t.id]: t }, home: { ...st.home, plan: false, ultra: false, ultra_wt: false, ultra_x: null } }));
      if (!stayOnHome) go("session", { task: t.id });
    } catch (e) {
      set((st) => ({ draft: typed, attach: { ...st.attach, [key]: images }, attachFiles: { ...st.attachFiles, [key]: files } }));
      persistDraft("new-chat", typed);
      flash(String(e));
    }
  } else if (s.task) {
    const body = [annotated, text].filter(Boolean).join("\n\n");
    // Consume the composer before any IPC awaits: text and screenshots should
    // disappear together, and anything added during the request is a new draft.
    clear();
    takeNotes();
    const optimistic: Item = {
      id: `pending-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      kind: "user",
      text: body,
      data: images.length ? { images } : {},
      ts: new Date().toISOString(),
    };
    set((st) => ({ pendingMessages: { ...st.pendingMessages, [s.task!]: [...(st.pendingMessages[s.task!] ?? []), optimistic] } }));
    const removeOptimistic = () => set((st) => {
      const remaining = (st.pendingMessages[s.task!] ?? []).filter((item) => item.id !== optimistic.id);
      const pendingMessages = { ...st.pendingMessages };
      if (remaining.length) pendingMessages[s.task!] = remaining;
      else delete pendingMessages[s.task!];
      return { pendingMessages };
    });
    try {
      await api.send(s.task, body, later, images.length ? images : undefined);
      removeOptimistic();
    } catch (e) {
      removeOptimistic();
      // Restore what failed without overwriting a newer draft or attachments.
      if (!(get().sessionDrafts[key] ?? "")) {
        set((st) => ({ sessionDrafts: { ...st.sessionDrafts, [key]: originalDraft } }));
        persistDraft(key, originalDraft);
      }
      if (images.length || files.length) set((st) => ({
        attach: { ...st.attach, [key]: [...images, ...(st.attach[key] ?? [])] },
        attachFiles: { ...st.attachFiles, [key]: [...files, ...(st.attachFiles[key] ?? []).map((f) =>
          f.imageIndex !== undefined ? { ...f, imageIndex: f.imageIndex + images.length } : f)] },
      }));
      // The notes go back on failure: the send did not happen, so they were not
      // sent, and a failed send already restores everything else it consumed.
      if (notes.length) set((st) => ({ notes: { ...st.notes, [s.task!]: [...notes, ...(st.notes[s.task!] ?? [])] } }));
      flash(String(e));
    }
  }
}

/** Which fallback route the chat uses (next to the model). */
function RouteChip({ route }: { route: string }) {
  const routes = useStore((s) => s.settings?.routes);
  const r = routes?.find((x) => x.id === route);
  if (!routes?.length) return null;
  return (
    <Tooltip content={r ? `Fallback route: ${r.name} · ${r.steps.length} fallback${r.steps.length === 1 ? "" : "s"}, then ${r.on_exhausted}` : "No fallback route · pauses when the model is out"}><MenuButton className={"routechip" + (r ? " on" : "")} onClick={(e) => openMenu("route", e)}>
      <span className="routeglyph" style={{ display: "flex" }}>{I.route(13)}</span>
      <span key={route} className="mname">{r ? r.name : "No routing"}</span>
    </MenuButton></Tooltip>
  );
}

/** The `*` after a pending change: click it twice to apply the change right now. */
export function PendingStar({ title = "Changed mid-turn · applies with your next message. Click twice to apply now." }: { title?: string }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => { if (!armed) return; const t = setTimeout(() => setArmed(false), 1400); return () => clearTimeout(t); }, [armed]);
  return (
    <Tooltip content={armed ? "Click again to apply now" : title}><span className={"pstar" + (armed ? " armed" : "")}
      onClick={async (e) => {
        e.stopPropagation();
        if (!armed) return setArmed(true);
        setArmed(false);
        await setOpts({ apply_pending: true });
      }}>*</span></Tooltip>
  );
}

export function Composer({ mode }: { mode: "home" | "session" }) {
  const task = useStore((s) => (mode === "session" && s.task ? s.tasks[s.task] : null));
  const home = useStore((s) => s.home);
  const draft = useStore((s) => (mode === "home" ? s.draft : s.sessionDrafts[s.task ?? ""] ?? ""));
  const sendWith = useStore((s) => s.settings?.send_with ?? "enter");
  const project = useStore((s) => s.settings?.project ?? "");
  const pausedAll = useStore((s) => s.settings?.paused_all ?? false);
  const models = useStore((s) => s.models);
  const providers = useStore((s) => s.providers);
  useStore((s) => s.accounts);
  useStore((s) => s.settings?.routes);
  const ref = useRef<HTMLTextAreaElement>(null);
  const [hi, setHi] = useState(0);
  const [slashOff, setSlashOff] = useState(false);
  const o = task ? { plan: task.plan, ultra: !!task.ultra, ultra_wt: !!task.ultra_wt, ultra_x: task.ultra_x ?? null, perm: task.perm, model: task.model, effort: task.effort, assist: task.assist, agents: task.agents } : home;
  const pend = task?.pending ?? {};
  // No mode is recorded yet, so use Full Access. Malformed explicit ids still
  // fail closed to the allowlist rung.
  const perm = o.perm
    ? PERMS.find((p) => p.id === o.perm) ?? PERMS[1]!
    : PERMS.find((p) => p.id === DEFAULT_PERM)!;
  const mi = modelInfo(pend.model ?? o.model);
  const modelReady = mi.provider === "route" || (models.some((m) => m.id === mi.id) && providers.some((p) => p.id === mi.provider && p.enabled && isConnected(p)));
  const assist = ASSISTS.find((a) => a.id === (pend.assist ?? o.assist)) ?? ASSISTS[1]!;
  const agentCount = countOptionalAgents(pend.agents ?? o.agents, useStore((s) => s.agents));
  const running = task && (task.status === "running" || task.status === "waiting");
  const paused = !!task?.paused || (!!task && pausedAll && !task.unpaused && !!running);
  const akey = mode === "home" ? "new-chat" : task?.id ?? "";
  const attached = useStore((s) => s.attach[akey] ?? NO_IMAGES);
  const files = useStore((s) => s.attachFiles[akey] ?? NO_FILES);
  const pathFiles = files.filter((f) => !f.image);
  const resolvingImages = files.some((f) => f.image && f.imageIndex === undefined);
  useEffect(() => { if (resolvingImages) void resolveImageRefs(akey); }, [akey, files, resolvingImages]);
  const noteCount = useStore((s) => mode === "session" ? s.notes[akey]?.length ?? 0 : 0);
  const has = draft.trim().length > 0 || attached.length > 0 || files.length > 0 || noteCount > 0;
  const [dropAt, setDropAt] = useState(false);

  // The window's own drag events are what carry real paths. A drop landing on
  // this composer is taken from here rather than from the browser's
  // DataTransfer, which never exposes the path behind a dropped file.
  //
  // It is also the only drop path that exists: with the window's native handler
  // installed, Windows' OLE drop target takes every drag before the page sees
  // it, so an HTML5 `onDrop` here would never fire. See `ui/drag.ts`.
  useEffect(() => {
    let off: UnlistenFn | undefined;
    let live = true;
    void getCurrentWindow()
      .onDragDropEvent((e) => {
        const p = e.payload;
        if (p.type === "enter") setDropAt(overComposer(e));
        else if (p.type === "leave") setDropAt(false);
        else if (p.type === "drop") {
          if (overComposer(e)) void addFiles(akey, p.paths);
          setDropAt(false);
        }
      })
      .then((f) => { if (live) off = f; else f(); });
    return () => { live = false; off?.(); };
  }, [akey]);

  const customCommands = useStore((s) => s.settings?.custom_commands ?? EMPTY_COMMANDS);
  const word = draft.startsWith("/") && !/\s/.test(draft) ? draft.toLowerCase() : null;
  const matches = word && !slashOff ? [
    ...SLASH.filter((c) => (!c.where || c.where === mode) && c.cmd.startsWith(word)),
    ...customCommands.filter((c) => /^[a-z0-9-]{1,32}$/.test(c.name) && !SLASH.some((b) => b.cmd.slice(1) === c.name)).map((c) => ({ cmd: `/${c.name}`, args: "[args]", desc: c.description || "Custom prompt" })).filter((c) => c.cmd.startsWith(word)),
  ] : [];
  const selectedCommands = matches.map((m) => SLASH.find((c) => c.cmd === m.cmd) ?? m);

  const sel = Math.min(hi, Math.max(0, matches.length - 1));

  useEffect(() => {
    ref.current?.focus();
  }, [mode, task?.id]);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, window.innerHeight * 0.4) + "px";
  }, [draft]);

  const onDraft = (v: string) => {
    setSlashOff(false);
    if (mode === "home") set({ draft: v });
    else set((s) => ({ sessionDrafts: { ...s.sessionDrafts, [s.task ?? ""]: v } }));
    persistDraft(mode === "home" ? "new-chat" : get().task ?? "", v);
  };
  const complete = (c: { cmd: string; args?: string }) => {
    onDraft(c.cmd + (c.args ? " " : ""));
    setHi(0);
    ref.current?.focus();
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (matches.length) {
      if (e.key === "ArrowDown") { e.preventDefault(); setHi(cycle(sel, 1, matches.length)); return; }
      if (e.key === "ArrowUp") { e.preventDefault(); setHi(cycle(sel, -1, matches.length)); return; }
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setSlashOff(true); return; }
      // In range by construction: `sel` is clamped to the last match above, and
      // this whole block is behind `matches.length`, so both arrow keys can only
      // ever move it to another index that exists.
      const c = matches[sel]!;
      // Tab always completes; Enter completes unless the command is already typed and needs no args.
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && (c.cmd !== draft.trim() || c.args?.startsWith("<")))) {
        e.preventDefault();
        complete(c);
        return;
      }
    }
    if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
    const mod = e.ctrlKey || e.metaKey;
    const wants = sendWith === "enter" ? !e.shiftKey && !mod : mod;
    if (wants || (e.altKey && !e.shiftKey)) {
      e.preventDefault();
      void submit(mode, e.altKey, mode === "home" && e.altKey);
    }
  };

  const ph = mode === "home"
    ? project ? "Describe a task, or / for commands like /goal and /plan" : "Open a project folder to start"
    : paused ? "Paused · type a message and hit Resume, or just resume" : task && (task.status === "stopped" || task.status === "failed") ? "Stopped · Resume to continue or wrap up, or say what to do instead" : running ? "Steer the agent now · Alt Enter to send after this turn" : "Follow up, or / for commands";

  return (
    <div className={"composer" + (mode === "session" ? " float" : "") + (paused ? " paused" : "") + (dropAt ? " dropping" : "")} style={{ position: "relative" }}>
      {matches.length > 0 && (
        <div className="slash">
          {selectedCommands.map((c, i) => (

            <div key={c.cmd} className={"mrow" + (i === sel ? " hi" : "")} style={{ animationDelay: i * 6 + "ms" }} onMouseMove={() => setHi(i)} onMouseDown={(e) => { e.preventDefault(); complete(c); }}>
              <span className="mono" style={{ fontWeight: 500, color: "var(--fg)", minWidth: 84 }}>{c.cmd}</span>
              {c.args && <span className="mono" style={{ fontSize: 11, color: "var(--mut2)" }}>{c.args}</span>}
              <span className="secondary-text" style={{ flex: 1, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{c.desc}</span>
            </div>
          ))}
        </div>
      )}
      {attached.length > 0 && (
        <div className="attachrow">
          {attached.map((src, i) => (
            <div key={i} className="attach">
              <Thumb src={src} items={attached.map((s: string, j: number) => ({ src: s, alt: `Attached image ${j + 1}` }))} index={i} alt={`Attached image ${i + 1}`} />
              <IconButton label="Remove image" className="attach-x" disabled={resolvingImages} onClick={() => removeImage(akey, i)}>{I.close(10)}</IconButton>
            </div>
          ))}
        </div>
      )}
      {pathFiles.length > 0 && (
        <div className="filerow">
          {pathFiles.map((f) => (
            <Tooltip key={f.path} content={f.path}>
              <div className="filechip">
                <span className="filename">{f.name}</span>
                <span className="filesize">{humanBytes(f.size)}</span>
                <IconButton label={`Remove ${f.name}`} className="attach-x" onClick={() => set((s) => ({ attachFiles: { ...s.attachFiles, [akey]: (s.attachFiles[akey] ?? []).filter((file) => file !== f) } }))}>{I.close(10)}</IconButton>
              </div>
            </Tooltip>
          ))}
        </div>
      )}
      <TextArea ref={ref} className={dropAt ? "dropping" : ""} value={draft} onChange={(e) => onDraft(e.currentTarget.value)} onKeyDown={onKey} placeholder={ph} rows={mode === "home" ? 3 : 2}
        onPaste={(e) => { if (addImages(akey, Array.from(e.clipboardData.files))) e.preventDefault(); }} />
      <div className="bar">
        <div className="bl">
          <IconButton label="Plan mode, compact, MCP" style={{ color: "var(--mut)" }} onClick={(e) => openMenu("plus", e)}>{I.plus()}</IconButton>
          {/* Attach: the dialog, or dropping onto the box. The dialog is the OS
              one rather than a hidden <input type=file>, because the agent needs
              the real path and the browser's File object never has one. */}
          <Tooltip content="Attach files for the agent to read · or drop them onto the box">
            <span className="attachbtn">
              <IconButton label="Attach files" onClick={async () => {
                const picked = await open({ multiple: true, title: "Attach files" });
                const paths = (Array.isArray(picked) ? picked : picked ? [picked] : []).filter((p): p is string => typeof p === "string");
                if (paths.length) void addFiles(akey, paths);
              }}>{I.paperclip()}</IconButton>
            </span>
          </Tooltip>
          {o.ultra && (
            <Tooltip content={xOn(o.ultra_x) ? `Ultrathread X · ${xDepth(o.ultra_x)} layers, each with its own model. Click to configure.` : "Ultrathread · nested subagents, keeps going until the list is done. Click to turn off."}>
              <Pressable className={"planpill ultrapill" + (xOn(o.ultra_x) ? " ultrapillx" : "")} onClick={() => (ULTRA_X_SHOWN && xOn(o.ultra_x) ? openUltraXEditor() : toggleUltra())}>
                <span className="ultraspark" aria-hidden="true" />
                {xOn(o.ultra_x) ? `Ultrathread X${o.ultra_x!.wt ? " · wt" : ""} · ${xDepth(o.ultra_x)}` : o.ultra_wt ? "Ultrathread · wt" : "Ultrathread"}
                <span style={{ fontSize: 10, opacity: 0.7 }}>{I.close(9)}</span>
              </Pressable>
            </Tooltip>
          )}
          {o.plan && <Tooltip content="Turn plan mode off · Shift Tab"><Pressable className="planpill" onClick={togglePlan}>Plan<span style={{ fontSize: 10, opacity: 0.7 }}>{I.close(9)}</span></Pressable></Tooltip>}
          {task?.goal?.status === "active" && <Tooltip content={task.goal.text}><div className="planpill" style={{ background: "rgba(51,214,255,0.12)", color: "#7fe4ff" }}>Goal</div></Tooltip>}
          <MenuButton style={{ color: perm.color }} onClick={(e) => openMenu("perm", e)}><span className="dot" style={{ background: perm.color, boxShadow: `0 0 6px ${perm.color}99` }} />{perm.name}</MenuButton>
        </div>
        <div className="bc">
          {modelReady ? <>
            <Tooltip content={pend.model ? `Switching from ${modelInfo(o.model).name} after this turn` : "Model · Shift-click a model to switch mid-turn"}><MenuButton className="mbtn" onClick={(e) => openMenu("model", e)}>
              <ProviderIcon provider={mi.provider} size={14} />
              <span key={mi.id} className="mname" style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis" }}>{mi.name}</span>
              {pend.model && <PendingStar />}
              {effortLabel(mi, o.effort) && <span key={o.effort} className="effl">{effortLabel(mi, o.effort)}</span>}
              {I.chev()}
            </MenuButton></Tooltip>
            <RouteChip route={task ? task.route : home.route} />
          </> : shownProviders(providers).some((p) => p.enabled && isConnected(p)) ? (
            <Tooltip content="Pick a model from a connected provider"><MenuButton className="mbtn" onClick={(e) => openMenu("model", e)}>
              <span style={{ color: "var(--dim)" }}>Pick a model</span>{I.chev()}
            </MenuButton></Tooltip>
          ) : <Tooltip content="Connect a model provider to continue"><MenuButton className="mbtn connect-provider" onClick={() => go("settings", { settingsTab: "models", modelSetupOpen: true })}>
            {I.plus(13)}<span>Connect Provider</span>
          </MenuButton></Tooltip>}
        </div>
        <div className="br">
          {mode === "home" && has && (
            <Tooltip content="Save this prompt and its options for later · clears the box"><IconButton label="Save for later" onClick={() => void savePrompt()}>{I.bookmark(13)}</IconButton></Tooltip>
          )}
          <Tooltip content={`Assist: ${assist.desc}`}><MenuButton style={{ color: assist.color, fontWeight: 500 }} onClick={(e) => openMenu("assist", e)}>
            {assist.name}{pend.assist && <PendingStar />}
          </MenuButton></Tooltip>
          <Tooltip content="Subagents this chat may use"><MenuButton style={{ color: agentCount ? "var(--mut)" : "var(--dim)" }} onClick={(e) => openMenu("agents", e)}>
            {I.agents()}{agentCount}{pend.agents && <PendingStar />}
          </MenuButton></Tooltip>
          {paused && !has ? (
            // `task_resume`, never `resume_all`: this button is about *this*
            // chat, and going through the global resume woke every chat the user
            // had deliberately left frozen.
            <RoundButton label={pausedAll ? "Resume this chat" : "Resume"} className="resume" onClick={() => task && api.resume(task.id).catch((e) => flash(String(e)))}>{I.play(11)}</RoundButton>
          ) : paused && has ? (
            <RoundButton label="Resume with this message for every agent" className="ready" onClick={() => { if (!task) return; // Photos ride the normal send path, which also lifts the pause. Plain text goes to every agent.
              if (attached.length || files.length || noteCount) void submit("session"); else { const m = draft; onDraft(""); api.resume(task.id, m).catch((e) => flash(String(e))); } }}>{I.up()}</RoundButton>
          ) : !has && task && (task.status === "stopped" || task.status === "failed") ? (
            <ResumeChoice id={task.id} />
          ) : running && !has ? (
            <>
              <RoundButton label="Pause · Esc · lets the current command finish, then freezes. Resume carries on as if nothing happened." className="pausebtn" onClick={() => task && api.pause(task.id).catch((e) => flash(String(e)))}>{I.pause(10)}</RoundButton>
              <RoundButton label="Stop · Esc Esc · cancels everything in flight, including background commands (dev servers, watchers). The agent is told what got cut off." className="stop" onClick={() => task && api.interrupt(task.id).catch((e) => flash(String(e)))}>{I.close(12)}</RoundButton>
            </>
          ) : (
            <RoundButton label={running ? "Steer now · Enter · Alt-click to send after this turn" : "Send · Enter"} className={has ? "ready" : ""} onClick={(e) => submit(mode, e.altKey)}>{I.up()}</RoundButton>
          )}
        </div>
      </div>
    </div>
  );
}
