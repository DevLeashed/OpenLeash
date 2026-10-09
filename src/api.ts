// Typed IPC surface for the Rust harness (src-tauri/src/lib.rs).
import { invoke } from "@tauri-apps/api/core";

export type BrowserPanelAction = { kind: "snapshot" | "back" | "refresh" } | { kind: "navigate"; url: string } | { kind: "click"; x: number; y: number } | { kind: "type"; text: string } | { kind: "key"; key: "Enter" | "Tab" } | { kind: "scroll"; delta_y: number };
export interface BrowserPanelFrame { url: string; title: string; png: string; width: number; height: number; viewport_width: number; viewport_height: number }
export function browserPanel(taskId: string, action: BrowserPanelAction) {
  return invoke<BrowserPanelFrame>("browser_panel", { taskId, action });
}


export interface Todo { content: string; status: "pending" | "in_progress" | "completed"; activeForm: string }
export interface Goal { text: string; status: "active" | "achieved" | "blocked" | "gave_up"; summary: string; nudges: number }
export interface SubInfo { id: string; role: string; task: string; status: string; meta: string; model: string; serving: string; started: string | null; report: string; background: boolean; parent?: string; depth?: number; cwd?: string; branch?: string; effort?: number | null }
export interface Pause { reason: string; kind: "manual" | "exhausted" | "error" | "closed"; since: string }
export type Assist = "guide" | "default" | "necessary";
/** One entry of an agent's `groups`: a bare group name where the group is
 *  unrestricted, or `[name, {fileRegex, description}]` where it is not (Roo
 *  Code's shape). Rust serializes the tuple form as an object. */
export type ToolGroup = string | { name: string; file_regex?: string; description?: string };
export interface AgentDef { id: string; name: string; description: string; prompt: string; model: string; tools: "all" | "read_only" | "no_shell"; color: string; builtin: boolean; source: string; inject_instructions?: boolean; effort?: number | null; groups?: ToolGroup[]; steps?: number }
export interface SkillDef { name: string; description: string; source: string; path: string; enabled: boolean; files: number; disable_model_invocation?: boolean; user_invocable?: boolean }
export interface Route { id: string; name: string; heads: string[]; all: boolean; steps: string[]; on_exhausted: "pause" | "fail" }
export interface Window { label: string; used: number; resets_at: number }
export interface AcctUsage { windows: Window[]; limited: boolean; plan: string; fetched: number; error: string }
export interface AccountView {
  id: string; kind: string; label: string; email: string; priority: number; enabled: boolean; source: string; disabled_reason: string;
  usage: AcctUsage | null; cooldown_until: number; cooldown_reason: string; available: boolean; active: boolean; expires_at: number;
  /** Masked OpenCode workspace-key suffix; absent on older backends, empty for OAuth logins. */
  key_hint?: string;
}
export interface Usage { input: number; output: number; cache_read: number; cache_write: number; cost: number; last_context: number }

export interface TaskSummary {
  id: string; title: string;
  /** The user named this chat themselves, so the agent's `set_title` is
   *  refused on it. Cleared from the sidebar's context menu. */
  titled: boolean;
  status: "idle" | "running" | "waiting" | "done" | "failed" | "stopped";
  waiting_kind: "approval" | "question" | "wake" | null;
  step: string; project: string; cwd: string; branch: string; base_branch: string; worktree: boolean;
  model: string; effort: number; perm: Perm; plan: boolean; ultra: boolean; ultra_wt: boolean; ultra_x: UltraX | null; subagents: boolean; goal: Goal | null;
  assist: Assist; agents: string[]; pending: { model?: string; assist?: Assist; agents?: string[] }; paused: Pause | null; serving: string; route: string; unpaused: boolean; busy: number; archived: boolean; hidden: boolean; forked_from: string | null; pinned: boolean; order: number;
  todos: Todo[]; subs: SubInfo[]; usage: Usage; context_window: number;
  created_at: string; updated_at: string;
  /** When the user last did something here (messaged, answered, paused,
   *  stopped, resumed). `updated_at` moves on every agent step, so the chat
   *  list sorts on this one instead. */
  touched_at: string;
}

export interface Item {
  id: string;
  kind: "user" | "text" | "thinking" | "tool" | "approval" | "question" | "asklater" | "sub" | "notice" | "user_notice" | "artifact";
  text: string;
  data: any;
  ts: string;
}

export interface Bg { id: string; cmd: string; started: string; last_line: string; running: boolean; exit: string | null }

export type Perm = "disabled" | "allowlist" | "auto" | "turbo";
/** Permission mode used when no choice has been recorded. */
export const DEFAULT_PERM: Perm = "turbo";

export interface ModelInfo {
  id: string; name: string; provider: string; context: number; output: number; input_price: number; output_price: number; effort: boolean;
  input_types: string[]; capabilities: string[]; reasoning_levels: string[]; reasoning_param: string; custom: boolean; enabled: boolean;
}
/** Blocking notice the user has to acknowledge before an account of this
 *  provider can be added. Null on providers whose terms say nothing about it. */
export interface AccountTermsGate { title: string; lede: string; points: string[]; terms_url: string; accept: string }
export interface AccountProviderInfo { display_name: string; short_name: string; login_file: string; login_command: string; paste_hint: string; setup_command: string; token_prefix: string; warning: string; terms_gate: AccountTermsGate | null; key_login: boolean }
export interface ProviderPreset { name: string; url: string; kind: "openai" | "anthropic"; icon: string }
export interface ProviderView { id: string; name: string; icon: string; mono: string; color: string; base_url: string; env: string; chip: string; kind: "openai" | "anthropic" | "codex"; custom: boolean; insist: boolean; connected: boolean; has_key: boolean; key_hint: string; key_hints: string[]; key_pool: boolean; base_url_override: string; enabled: boolean; local: boolean; account: AccountProviderInfo | null }
export interface RemoteModel { id: string; name: string; context: number | null; output: number | null; input_price: number | null; output_price: number | null; modalities: string[] | null; reasoning_levels?: string[] | null; reasoning_param?: string | null; reasoning_parameter?: string | null; supported_reasoning_levels?: (string | { effort?: string })[] | null; supported_parameters?: string[] | null }
export interface AllowRule { pattern: string; project: string }
/** `transport` is absent on servers saved before the HTTP transport existed —
 *  those are stdio, and `command` is empty on a URL server. */
export interface McpServerCfg { name: string; command: string; args: string[]; env: Record<string, string>; enabled: boolean; transport?: string; url?: string; headers?: Record<string, string>; oauth?: boolean;
  /** Tool names on this server that skip the approval prompt — pre-approves
   *  `mcp__<server>__<tool>` and nothing else. The tool's own name, never a
   *  wildcard; a full `mcp__…` name is refused by the backend. Absent on
   *  servers saved before it existed. */
  auto_approve?: string[];
  /** Always declare this server's tools instead of deferring them to `tool_search`. */
  defer?: boolean;
}
export interface McpStatus { name: string; status: string; tools: number; error: string | null; desc?: string; tool_names?: string[] }

/** A prompt parked for later, with the composer options it was written under. */
export interface SavedPrompt {
  id: string; text: string; project: string;
  model: string; route: string; assist: Assist; perm: Perm; effort: number | null;
  plan: boolean; ultra: boolean; ultra_wt: boolean; worktree: boolean; branch: string;
  agents: string[]; images: string[]; files?: FileRef[];
  created_at: string;
}

export interface CustomCommand { name: string; description: string; prompt: string }

export interface Settings {
  model: string; effort: number; perm: Perm; worktree: boolean; subagents: boolean;
  ui_size: string; ui_zoom: number; send_with: "enter" | "ctrl-enter"; theme?: "raycast" | "lavender" | "nord" | "dracula" | "gruvbox" | "tokyo" | "rosepine" | "catppuccin" | "solarized" | "everforest" | "monokai" | "ayu" | "ocean";
  /** Light or dark, or follow the OS. Absent on settings saved before light mode. */
  mode?: "light" | "dark" | "system";
  routes: Route[]; agents: AgentDef[]; default_agents: string[]; assist: Assist; paused_all: boolean; paused_reason: string; close_to_tray: boolean; notify?: boolean; home_usage: string[]; model_colors: Record<string, string>;
  /** Cross-chat inbox for pending approvals and questions. Hidden by default. */
  needs_you?: boolean;
  /** Sidebar lists chats from every project folder, not just the current one. */
  all_projects: boolean;
  show_thinking: boolean; inject_global_claude: boolean;
  /** Let the agent rename this chat as the work moves on. On by default. */
  agent_titles?: boolean;
  /** Keep chats ordered automatically by attention and recent activity, even after a manual drag. On by default. */
  automatic_chat_reorder?: boolean;
  /** Let the agent keep per-project notes across chats. Off by default. */
  memory?: boolean;
  /** Idle window for "archive the chats I have not used in a while" (Settings →
   *  Chats), in days. Nothing prunes on its own: this is only how long the
   *  button's threshold sits before the user presses it. */
  archive_after_days?: number;
  /** State of the built-in `.env` read deny shown in Settings. */
  deny_env_files?: boolean;
  /** Hold back the long tail of MCP tool schemas in favor of `tool_search`. */
  tool_search?: boolean;
  /** Also defer plugin tools (left off until prompt guidance is connected). */
  defer_plugins?: boolean;
  /** Snapshot working-folder changes for file-level rewind. */
  checkpoints?: boolean;
  plugins: Plugins;
  diagnostics: boolean; verify_nudge: boolean; hooks: Hook[]; saved_prompts?: SavedPrompt[]; custom_commands?: CustomCommand[];
  projects: string[]; project: string;
  recent_models: string[]; favorite_models: string[]; custom_models: string[]; disabled_models: string[]; removed_models: string[];
  allow: AllowRule[]; mcp: McpServerCfg[];
  budget: number; spend: Record<string, number>; tokens_month: number;
  /** Folder trust decisions. No entry for a folder = untrusted until decided. */
  trust?: TrustDecision[];
  /** How an agent-made commit is attributed in git log. */
  git_attribution?: Attribution;
  /** Run the repository's own git hooks when the agent commits. Off by default. */
  git_commit_verify?: boolean;
  /** Keep the provider's cached prefix warm during long idle periods. */
  cache_keepalive?: boolean;
  /** Maximum number of keepalive pings per task. */
  cache_keepalive_pings?: number;
}

export type HookEvent =
  | "session_start" | "user_prompt_submit" | "pre_tool" | "post_tool"
  | "post_setup_worktree" | "subagent_stop" | "error" | "stop" | "session_end";

export interface Hook {
  event: HookEvent;
  matcher: string;
  command: string;
  enabled: boolean;
  /** Stable identity assigned by the backend. Absent on a hook you just built in the composer. */
  id?: string;
  /** "user" (or absent/"" — a settings.json written by an older build) or "project". */
  source?: "" | "user" | "project";
  /** Project root, for a `source: "project"` hook. */
  origin?: string;
  /** pre_tool only: this hook may rewrite the tool call's arguments (Codex's `updatedInput`). Off unless ticked. */
  updated_input?: boolean;
}

/** A hook plus the part of its state that is not in settings.json: whether the
 *  user has reviewed the command, and whether it still matches the one they
 *  approved. An untrusted hook is listed but never runs. */
export interface HookView extends Hook {
  /** Reviewed and trusted: it will run. */
  trusted: boolean;
  /** Was trusted, but the command or matcher changed since. Shows `changed_from`. */
  stale: boolean;
  /** The command as it was when the user approved it (only when `stale`). */
  changed_from: string | null;
  /** false for `source: "project"` hooks — those live in the repo, edit them there. */
  can_edit: boolean;
}
export interface HooksStatus { project: string; hooks: HookView[] }
export type Attribution = "none" | "co-authored-by" | "assisted-by" | "generated-with";
export const ATTRIBUTIONS: { id: Attribution; label: string; hint: string }[] = [
  { id: "co-authored-by", label: "Co-authored-by", hint: "Adds “Co-authored-by: OpenLeash <agent@openleash.dev>” — rendered as credit on every git host" },
  { id: "assisted-by", label: "Assisted-by", hint: "Adds “Assisted-by: OpenLeash <agent@openleash.dev>”" },
  { id: "generated-with", label: "Generated with", hint: "Adds a “Generated with OpenLeash” line, as some CLI agents do" },
  { id: "none", label: "None", hint: "No attribution trailer on agent-made commits" },
];
export interface Plugins { github: { enabled: boolean; client_id?: string }; computer: { enabled: boolean; settle_ms: number }; browser: { enabled: boolean; width: number; height: number } }

/** Folder trust. A folder with no decision is untrusted until the user says otherwise. */
export type TrustKind = "folder" | "parent";
export type TrustState = "trusted" | "untrusted";
export interface TrustDecision { path: string; kind: TrustKind; decision: TrustState; decided_at: string }
export interface TrustMatched { kind: TrustKind; path: string; decision: TrustState }
/** One file that would be injected into the system prompt. */
export interface ManifestFile { name: string; path: string; bytes: number; chars: number; truncated: boolean; pointer_only: boolean; global: boolean; preview: string }
export interface ManifestSkill { name: string; description: string; path: string; files: number }
export interface ManifestAgent { id: string; name: string; description: string; tools: string; path: string }
export interface ManifestHook { event: string; matcher: string; command: string; source: string }
export interface ManifestMcp { name: string; command: string; transport: string; source: string }
export interface ManifestOverride { file: string; key: string; value: string }
export interface TrustWarning { level: "danger" | "warn" | "info"; text: string }
/** Everything a folder would add, computed even when it is untrusted so the user
 *  can see exactly what they are deciding about. */
export interface TrustManifest {
  path: string; trusted: boolean; decided: boolean; matched: TrustMatched | null; parent_dir: string; injecting: boolean;
  instructions: ManifestFile[]; skills: ManifestSkill[]; agents: ManifestAgent[];
  hooks: ManifestHook[]; mcp: ManifestMcp[]; overrides: ManifestOverride[]; warnings: TrustWarning[];
}
export interface PluginsStatus {
  has_token: boolean; can_login: boolean;
  github: { ok: boolean; source: string | null; login?: string; error?: string };
  computer: { ok: boolean; error?: string; monitors: { name: string; w: number; h: number; primary: boolean }[] };
  browser: { ok: boolean; error?: string; browser?: string };
}

export interface Boot { settings: Settings; providers: ProviderView[]; provider_presets: ProviderPreset[]; models: ModelInfo[]; tasks: TaskSummary[]; accounts: AccountView[]; agents: AgentDef[]; skills: SkillDef[]; shell: string; data_dir: string; month_spend: number; user_notices?: Record<string, Item[]> }

/** What the monthly cap is actually counting: the per-day spend ledger the
 *  backend keeps, plus the running token total. Survives a stats reset, so it
 *  is not derivable from `stats_get`. */
export interface UsageView { month: number; spend: Record<string, number>; tokens: number; budget: number }
export interface FileDiff { path: string; status: string; add: number; del: number; lines: { k: "a" | "d" | "c" | "h"; t: string }[] }

/** One file a checkpoint changed, measured from the shadow-git snapshot. */
export interface CheckpointFile { path: string; status: string; add: number; del: number }

/** One rewindable step: a submitted prompt and what it changed on disk. */
export interface CheckpointInfo {
  /** The user item this checkpoint belongs to, stable across reloads. */
  item_id: string;
  text: string;
  ts: string;
  /** Self-describing, Gemini-style: when it happened and what it was about. */
  label: string;
  files: CheckpointFile[];
  /** Whether a file snapshot exists at all — the file options are hidden when
   *  it is false, since there would be nothing to restore. */
  has_files: boolean;
  /** The newest step: restoring its files is a no-op. */
  current: boolean;
}

/** Everything the rewind picker needs, in one round trip. */
export interface RewindInfo { checkpoints: CheckpointInfo[]; in_worktree: boolean }

/** What a file restore did. `skipped` is the user's own work, left alone. */
export interface RewindResult { restored: number; skipped: string[]; text: string }

export interface NewTask { prompt: string; project: string; model: string; effort: number; perm: Perm; plan: boolean; worktree: boolean; base_branch: string; subagents: boolean; assist: Assist; agents: string[]; route: string; images?: string[]; ultra?: boolean; ultra_wt?: boolean; ultra_x?: UltraX | null }
/** One layer of an ULTRATHREAD X ladder. */
export interface UltraXLayer { model: string; effort: number | null; fanout: number }
/** The ULTRATHREAD X ladder: a model / effort / fanout per layer below the orchestrator. */
export interface UltraX { layers: UltraXLayer[]; max_running: number; max_total: number; wt: boolean }
export const X_MAX_LAYERS = 6;
export const X_MAX_FANOUT = 8;
export const X_DEFAULT_RUNNING = 32;
export const X_DEFAULT_TOTAL = 400;
export const emptyXLayer = (): UltraXLayer => ({ model: "", effort: null, fanout: 0 });
export const defaultX = (): UltraX => ({ layers: Array.from({ length: X_MAX_LAYERS }, emptyXLayer), max_running: X_DEFAULT_RUNNING, max_total: X_DEFAULT_TOTAL, wt: false });
/** X mode is a real mode only from two layers up; one layer is ultrathread with a worker model. */
export const xOn = (x: UltraX | null | undefined): x is UltraX => !!x && x.layers.length > 1;
export const xDepth = (x: UltraX) => Math.min(x.layers.length, X_MAX_LAYERS);
export type TaskPatch = Partial<Pick<TaskSummary, "model" | "effort" | "perm" | "plan" | "ultra" | "ultra_wt" | "ultra_x" | "subagents" | "title" | "assist" | "agents" | "archived" | "pinned" | "order" | "route">> & { now?: boolean; apply_pending?: boolean; /** Hand the name back to the agent: clears the manual-rename lock. */ untitled?: boolean };

/** A file the user attached for the agent to read. The model gets the path,
 *  except for images, which ride along as real image blocks so it can see
 *  them. Produced by the `files_attach` command, which is the only way this
 *  app can learn a real path: the browser `File` object deliberately has none. */
export interface FileRef { path: string; name: string; size: number; isDir: boolean; image: boolean }

export const api = {
  boot: () => invoke<Boot>("app_boot"),
  tasks: () => invoke<TaskSummary[]>("tasks_list"),
  task: (id: string) => invoke<{ summary: TaskSummary; items: Item[]; sub_items: Record<string, Item[]>; bg: Bg[] }>("task_get", { id }),
  /** A subagent's report, fetched when its panel opens: the live `task` event
   *  leaves reports out, because they are large and rarely change. */
  subReport: (id: string, sub: string) => invoke<string>("sub_report", { id, sub }),
  create: (req: NewTask) => invoke<TaskSummary>("task_create", { req }),
  send: (id: string, text: string, later = false, images?: string[]) => invoke<void>("task_send", { id, text, later, images }),
  interrupt: (id: string) => invoke<void>("task_interrupt", { id }),
  respond: (id: string, itemId: string, response: unknown) => invoke<void>("task_respond", { id, itemId, response }),
  /** Answer a question the agent raised without waiting (`ask_nonblocking`). The
   *  run is not blocked on it, so the answer goes to the agent as a note it reads
   *  on its next request rather than through the pending-answer channel. */
  answerNonBlocking: (id: string, itemId: string, response: unknown) => invoke<void>("task_answer_nonblocking", { id, itemId, response }),
  /** Dismiss only the persisted notice; this never sends an answer or resumes a run. */
  dismissAgentNotice: (id: string, itemId: string) => invoke<void>("task_dismiss_notice", { id, itemId }),
  update: (id: string, patch: TaskPatch) => invoke<TaskSummary>("task_update", { id, patch }),
  stats: () => invoke<import("./ui/Stats").StatsData>("stats_get"),
  statsReset: () => invoke<void>("stats_reset"),
  /** One chat's stats, by id — the panel behind "Stats…" in a chat's context
   *  menu. Its own command because `stats_get` returns the whole ledger and
   *  opening one chat's numbers does not need the rest of it.
   *
   *  `null` when the chat has no row: it never ran a request, or it has aged
   *  out of the cap the backend keeps. An empty title means the chat itself is
   *  gone — its numbers are kept after deletion on purpose. */
  statsChat: (id: string) => invoke<[string, import("./ui/Stats").ChatStat] | null>("stats_chat", { id }),
  draftGet: (key: string) => invoke<string>("draft_get", { key }),
  draftSet: (key: string, text: string) => invoke<void>("draft_set", { key, text }),
  /** Resolve picked/dropped absolute paths into attachments. */
  filesAttach: (paths: string[]) => invoke<FileRef[]>("files_attach", { paths }),
  /** Read an attached image into a data URL so the model can see it. */
  fileDataUrl: (path: string) => invoke<string>("file_data_url", { path }),
  subSend: (id: string, subId: string, text: string) => invoke<void>("sub_send", { id, subId, text }),
  subModel: (id: string, subId: string, model: string) => invoke<void>("sub_set_model", { id, subId, model }),
  subEffort: (id: string, subId: string, effort: number | null) => invoke<void>("sub_set_effort", { id, subId, effort }),
  dismissPause: (id: string) => invoke<void>("task_dismiss_pause", { id }),
  pause: (id: string) => invoke<void>("task_pause", { id }),
  statusSummary: (id: string) => invoke<{ text: string; model: string; via?: string }>("task_status_summary", { id }),
  forcePause: (id: string) => invoke<void>("task_force_pause", { id }),
  forcePauseAll: () => invoke<void>("force_pause_all"),
  resume: (id: string, message?: string, mode?: "continue" | "wrap") => invoke<void>("task_resume", { id, message: message || null, mode: mode ?? null }),
  broadcast: (id: string, text: string) => invoke<string>("task_broadcast", { id, text }),
  pauseAll: () => invoke<void>("pause_all"),
  resumeAll: (message?: string) => invoke<void>("resume_all", { message: message || null }),
  /** Lift the pauses on a chosen set of chats, leaving the rest frozen. Under
   *  "Pause all" this is the only way to thaw one chat without the rest. */
  tasksResume: (ids: string[], message?: string) => invoke<number>("tasks_resume", { ids, message: message || null }),
  /** Turn each listed chat's pause into a stop: the frozen work is cancelled and
   *  nothing is resumed. The bulk half of `dismissPause`. */
  tasksDismissPause: (ids: string[]) => invoke<number>("tasks_dismiss_pause", { ids }),
  tasksMessage: (ids: string[], text: string, later = false) => invoke<number>("tasks_message", { ids, text, later }),
  accounts: () => invoke<AccountView[]>("accounts_list"),
  accountImport: (kind: string, text: string) => invoke<AccountView[]>("account_import", { kind, text }),
  accountImportLocal: (kind: string, path?: string) => invoke<AccountView[]>("account_import_local", { kind, path: path || null }),
  accountUpdate: (id: string, patch: { label?: string; priority?: number; enabled?: boolean; move?: number }) => invoke<AccountView[]>("account_update", { id, patch }),
  accountRemove: (id: string) => invoke<AccountView[]>("account_remove", { id }),
  accountsRefresh: () => invoke<AccountView[]>("accounts_refresh"),
  exportTask: (id: string, path: string) => invoke<void>("task_export", { id, path }),
  /** The ticked chats into one file. Its own command rather than a loop of
   * `exportTask`: N round-trips is N writes of work to end up in one file, and
   * falling back to `exportAll` would quietly drag in chats nobody ticked.
   * Returns how many went in. */
  exportTasks: (ids: string[], path: string) => invoke<number>("tasks_export", { ids, path }),
  exportAll: (path: string, includeArchived: boolean) => invoke<number>("tasks_export_all", { path, includeArchived }),
  importTasks: (path: string) => invoke<TaskSummary[]>("tasks_import", { path }),
  deleteArchived: () => invoke<number>("tasks_delete_archived"),
  /** Put the ticked chats away at once. The bulk half of `update(id,
   * {archived: true})`; returns how many were actually archived, so a tick that
   * named an already-archived chat reports honestly instead of inflating the
   * toast. */
  archiveTasks: (ids: string[]) => invoke<number>("tasks_archive", { ids }),
  duplicate: (id: string) => invoke<TaskSummary>("task_duplicate", { id }),
  fork: (id: string, mode: "full" | "compact") => invoke<TaskSummary>("task_fork", { id, mode }),
  poolModels: () => invoke<ModelInfo[]>("pool_models_refresh"),
  agents: (project?: string) => invoke<AgentDef[]>("agents_list", { project: project || null }),
  skills: (project?: string) => invoke<SkillDef[]>("skills_list", { project: project || null }),
  skillImport: (path: string) => invoke<SkillDef>("skill_import", { path }),
  skillRemove: (name: string) => invoke<SkillDef[]>("skill_remove", { name }),
  skillSetEnabled: (name: string, enabled: boolean) => invoke<SkillDef[]>("skill_set_enabled", { name, enabled }),
  skillRead: (name: string, project?: string) => invoke<{ skill: SkillDef; content: string; files: string[] }>("skill_read", { name, project: project || null }),
  providerInsist: (id: string, insist: boolean) => invoke<ProviderView[]>("provider_insist", { id, insist }),
  remove: (id: string, removeWorktree: boolean) => invoke<void>("task_delete", { id, removeWorktree }),
  rewind: (id: string, itemId: string) => invoke<string>("task_rewind", { id, itemId }),
  /** Every rewindable step of a chat, with what each one changed on disk. */
  rewindInfo: (id: string) => invoke<RewindInfo>("task_rewind_info", { id }),
  /** The three-way rewind: `files` | `conversation` | `both`. Returns whatever
   *  the files side had to leave alone (the user's own edits since). */
  rewindTo: (id: string, itemId: string, mode: "files" | "conversation" | "both") => invoke<RewindResult>("task_rewind_to", { id, itemId, mode }),
  /** Branch this chat at one of its steps into a fresh worktree, leaving this
   *  chat exactly as it is. Distinct from a rewind. */
  forkAt: (id: string, itemId: string) => invoke<TaskSummary>("task_fork_at", { id, itemId }),
  /** The message list above the composer: edit, drop, or move one to a new place. */
  queueRemove: (id: string, itemId: string) => invoke<void>("task_queue_remove", { id, itemId }),
  queueEdit: (id: string, itemId: string, text: string) => invoke<void>("task_queue_edit", { id, itemId, text }),
  /** `before` is the row to sit in front of, or null to move it to the end. */
  queueMove: (id: string, itemId: string, before: string | null) => invoke<void>("task_queue_move", { id, itemId, before }),
  /** Take one waiting message now, leaving the rest of the queue as it is. */
  queueSendNow: (id: string, itemId: string) => invoke<void>("task_queue_send_now", { id, itemId }),
  queueSendAll: (id: string) => invoke<void>("task_queue_send_all", { id }),
  review: (id: string) => invoke<{ git: boolean; files: FileDiff[] }>("task_review", { id }),
  revertFile: (id: string, path: string) => invoke<void>("task_revert_file", { id, path }),
  commit: (id: string, message: string) => invoke<string>("task_commit", { id, message }),
  /** Draft a Conventional Commits message for the current change.
   *  `source` is "model" when a model wrote it and "fallback" when there was no
   *  usable answer (or the provider call is not wired yet) — the caller says so
   *  rather than presenting a local guess as the model's work. */
  commitMessage: (id: string) => invoke<{ message: string; source: "model" | "fallback" }>("task_commit_message", { id }),
  bgKill: (taskId: string, bgId: string) => invoke<void>("bg_kill", { taskId, bgId }),
  settings: (patch: Partial<Settings>) => invoke<Settings>("settings_update", { patch }),
  // Hooks are shell commands, so they get their own command rather than riding
  // the generic settings patch.
  hooks: (hooks: Hook[]) => invoke<Settings>("hooks_save", { hooks }),
  hooksStatus: () => invoke<HooksStatus>("hooks_status"),
  /** approve = review & trust this hook's current body; approve:false revokes it. Origin is part of the identity (empty for user hooks). */
  hooksTrust: (id: string, origin: string, approve: boolean) => invoke<Settings>("hooks_trust", { id, origin, approve }),
  provider: (id: string, p: { apiKey?: string; baseUrl?: string; enabled?: boolean; keyPool?: boolean }) => invoke<ProviderView[]>("provider_set", { id, apiKey: p.apiKey, baseUrl: p.baseUrl, enabled: p.enabled, keyPool: p.keyPool }),
  providerKey: (id: string, p: { op: "add" | "remove" | "replace"; key?: string; index?: number }) => invoke<ProviderView[]>("provider_key", { id, op: p.op, key: p.key ?? null, index: p.index ?? null }),
  models: () => invoke<ModelInfo[]>("models_list"),
  providerAdd: (p: { name: string; base_url: string; kind: string; api_key: string; insist?: boolean; builtin?: string }) => invoke<ProviderView[]>("provider_add", { p }),
  providerRemove: (id: string) => invoke<ProviderView[]>("provider_remove", { id }),
  modelSave: (model: ModelInfo, replace?: string) => invoke<ModelInfo[]>("model_save", { model, replace }),
  modelRemove: (id: string) => invoke<ModelInfo[]>("model_remove", { id }),
  modelSetEnabled: (id: string, enabled: boolean) => invoke<ModelInfo[]>("model_set_enabled", { id, enabled }),
  providerModels: (id: string) => invoke<RemoteModel[]>("provider_models", { id }),
  mcpStatus: () => invoke<McpStatus[]>("mcp_status"),
  mcpReconnect: () => invoke<void>("mcp_reconnect"),
  mcpOauthStart: (name: string, clientId?: string, scopes?: string[], redirectPort?: number) => invoke<{ flow_id: string; authorization_url: string }>("mcp_oauth_start", { name, clientId: clientId ?? null, scopes: scopes ?? null, redirectPort: redirectPort ?? null }),
  mcpOauthWait: (flowId: string) => invoke<void>("mcp_oauth_wait", { flowId }),
  mcpOauthCancel: (flowId: string) => invoke<void>("mcp_oauth_cancel", { flowId }),
  mcpOauthDisconnect: (name: string) => invoke<void>("mcp_oauth_disconnect", { name }),
  /** What the swap dialog lists. `rows` is one entry per model actually in use.
   *  `agents` names the subagent types a `spread` would reach — only the global
   *  dialog gets them, and only so the opt-in can say what it is about to touch. */
  modelsInUse: (id?: string) => invoke<{ rows: { model: string; effort: number; uses: string[] }[]; agents: { id: string; name: string }[] }>("models_in_use", { id: id ?? null }),
  /** `effort` is keyed by the model being swapped *away from*, like `map`: the
   *  reasoning level picked for the destination travels with that row. `spread`
   *  is the separate "and the subagent types too" opt-in: without it nothing a
   *  future spawn of an agent type would read is touched. */
  modelsSwap: (map: Record<string, string>, id?: string, effort?: Record<string, number>, spread?: boolean) => invoke<Settings>("models_swap", { id: id ?? null, map, effort: effort ?? null, spread: spread ?? false }),
  pluginsStatus: () => invoke<PluginsStatus>("plugins_status"),
  githubLoginStart: () => invoke<{ user_code: string; verification_uri: string; device_code: string; interval: number; expires_in: number }>("github_login_start"),
  githubLoginWait: (deviceCode: string, interval: number, expiresIn: number) => invoke<string>("github_login_wait", { deviceCode, interval, expiresIn }),
  githubLoginCancel: () => invoke<void>("github_login_cancel"),
  githubToken: (token: string) => invoke<void>("plugin_github_token", { token }),
  project: (path: string) => invoke<{ exists: boolean; git: boolean; branch: string; branches: string[]; memory: string[]; trusted: boolean; decided: boolean }>("project_info", { path }),
  /** What a folder would inject, trusted or not — the trust dialog's evidence
   *  and the manifest panel's data. */
  trustManifest: (path: string) => invoke<TrustManifest>("trust_manifest", { path }),
  /** Record a trust decision. `parent` trusts the folder and everything under
   *  it; `folder` is this directory alone. */
  trustSet: (path: string, kind: TrustKind, decision: TrustState) => invoke<Settings>("trust_set", { path, kind, decision }),
  /** Forget a folder's decisions, so it reads as never-decided (untrusted). */
  trustClear: (path: string) => invoke<Settings>("trust_clear", { path }),
  usage: () => invoke<UsageView>("usage_get"),
  /** Raise a desktop toast that opens `taskId` when clicked. The backend builds
   *  the toast document itself so it can carry the chat id as the `launch`
   *  argument — the plugin's own toast path has nowhere to put one, which is why
   *  its toasts could be delivered but never clicked. */
  toast: (taskId: string, title: string, body: string) => invoke<void>("toast_show", { taskId, title, body }),
};

export const EFFORTS = ["Max", "Extra High", "High", "Medium", "Low"];

/** The reasoning levels a model actually accepts, lowest first. Empty when it
 *  has no reasoning control, so the effort pickers offer nothing for it. */
export function effortLevels(m: { reasoning_levels: string[]; reasoning_param: string }): string[] {
  return m.reasoning_param === "none" ? [] : m.reasoning_levels.filter((l) => l.trim() !== "");
}

/** The effort positions that reach the wire as *distinct* values. The stored
 *  effort is 0 (max) … 4 (low) and Rust spreads it over the model's own levels
 *  in `pick_level`; steps of a model with fewer levels than 5 collapse onto the
 *  same level, so offering them is offering a choice that does nothing.
 *  Mirrors that spread, then keeps only the positions that change the answer:
 *  a 5-level model gets 5, a 3-level model 3, an off/on model 2, a Max-only
 *  model 1. Positions stay in stored order (max → low). */
export function effortSteps(m: { reasoning_levels: string[]; reasoning_param: string }): number[] {
  const levels = effortLevels(m);
  if (!levels.length) return [];
  const at = (e: number) => levels[Math.round(((4 - e) / 4) * (levels.length - 1))];
  const out: number[] = [];
  for (let e = 0; e <= 4; e++) if (out.every((k) => at(k) !== at(e))) out.push(e);
  return out;
}

/** Snap a stored effort onto a position the model really has, so a setting
 *  left over from another model shows a real level instead of nothing. Ties go
 *  to the lower rung: an "Extra High" left on a three-level model is nearer its
 *  Medium than its Max, and overspending effort is the worse mistake. A model
 *  with no levels has no rungs to snap to, so the value passes through. */
export function clampEffort(steps: readonly number[], effort: number): number {
  if (!steps.length) return effort;
  if (steps.includes(effort)) return effort;
  // `steps` is never empty here, but a reader (and the type-checker, since
  // `noUncheckedIndexedAccess` is on) has to see that before taking [0] as the
  // seed: without it the reduce would compare against `undefined` and pick NaN.
  const seed = steps[0]!;
  return steps.reduce((best, s) => (Math.abs(s - effort) <= Math.abs(best - effort) ? s : best), seed);
}

/** The level an effort position sends — the same spread Rust's `pick_level`
 *  applies, so what a label claims is what the request carries. Takes the
 *  stored effort as-is (bounded to the slider's range) rather than snapping to
 *  a rung first: the wire value depends on the raw number. */
export function effortLevel(m: { reasoning_levels: string[]; reasoning_param: string }, effort: number): string | null {
  const levels = effortLevels(m);
  if (!levels.length) return null;
  const e = Math.max(0, Math.min(4, effort));
  // Same index as `effortSteps`' spread, on a list we just proved non-empty. The
  // rounding is what bounds it: `(4 - e) / 4` is within [0, 1] and the product
  // with `length - 1` is within [0, length - 1].
  const at = Math.round(((4 - e) / 4) * (levels.length - 1));
  return levels[at] ?? null;
}

/** What to call the current effort: the model's own level where it has one
 *  (so a Max-only model reads "max" and a thinking budget reads "16000"),
 *  else the generic ladder name. Null when the model has no reasoning control. */
export function effortLabel(m: { reasoning_levels: string[]; reasoning_param: string }, effort: number): string | null {
  const steps = effortSteps(m);
  if (!steps.length) return null;
  return effortLevel(m, effort) ?? EFFORTS[clampEffort(steps, Math.round(effort))] ?? null;
}
/** The permission ladder, ascending autonomy. It includes the unset default,
 *  and unknown non-empty ids fail closed through permRung. */
export const PERMS: { id: Perm; name: string; color: string; desc: string }[] = [
  { id: "disabled", name: "Disabled", color: "#8b8f98", desc: "Read-only commands only. Edits and commands are refused, not asked about." },
  { id: "allowlist", name: "Allowlist only", color: "#33d6ff", desc: "Reads freely, runs allow-listed commands. Asks before anything else." },
  { id: "auto", name: "Auto-edit", color: "#a78bfa", desc: "Edits files in its working directory. Asks before commands with side effects." },
  { id: "turbo", name: "Full Access", color: "#ff6363", desc: "Runs anything without asking. Best inside a worktree." },
];

/** Resolve a stored permission id or an unset value. Unknown non-empty ids
 *  fail closed onto `allowlist`, never a more permissive rung. */
export function permRung(id: string | undefined): Perm {
  if (!id) return DEFAULT_PERM;
  switch (id) {
    case "full": return "turbo";
    case "ask": return "allowlist";
    case "auto": return "auto";
    case "turbo": return "turbo";
    case "disabled": return "disabled";
    case "allowlist": return "allowlist";
    default: return "allowlist";
  }
}

/** The display name of a rung, tolerant of legacy ids. */
export function permLabel(id: string | undefined): string {
  return PERMS.find((p) => p.id === permRung(id))?.name ?? "";
}

/** A deny rule lives in the same `allow` array as an allow rule: it is just a
 *  pattern prefixed with `!`. Deny always beats every allow rule and every rung. */
export const DENY_PREFIX = "!";
export function isDenyRule(pattern: string): boolean {
  return pattern.startsWith(DENY_PREFIX);
}

export const ASSISTS: { id: Assist; name: string; color: string; desc: string }[] = [
  { id: "guide", name: "Guide", color: "#7fe4ff", desc: "Asks a lot, even for smaller decisions" },
  { id: "default", name: "Default", color: "#d4d4d8", desc: "Asks when something's unclear or yours to decide" },
  { id: "necessary", name: "Necessary", color: "#fbbf24", desc: "Asks only when truly blocked" },
];

export function until(unix: number) {
  const s = unix - Date.now() / 1000;
  if (s <= 0) return "now";
  if (s < 3600) return Math.ceil(s / 60) + "m";
  if (s < 86400) return Math.floor(s / 3600) + "h " + Math.round((s % 3600) / 60) + "m";
  return Math.round(s / 86400) + "d";
}

/** Token counts, short: 0, 32k, 1.20M, 4.33B, 1.05T. Scaled to two decimals, trailing zeros dropped. */
const SUFFIX: [number, string][] = [[1e12, "T"], [1e9, "B"], [1e6, "M"], [1e3, "k"]];
export const fmtK = (n: number) => {
  const hit = SUFFIX.find(([size]) => n >= size);
  if (!hit) return String(n);
  const [size, suffix] = hit;
  return (n / size).toFixed(2).replace(/\.?0+$/, "") + suffix;
};
export const fmt$ = (n: number) => "$" + (n < 10 ? n.toFixed(2) : n.toFixed(1));
export const baseName = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
export const normPath = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
/** One line for a list row: runs of whitespace collapsed, trimmed to length. */
export const oneLine = (s: string, n = 160) => (s.replace(/\s+/g, " ").trim().length > n ? s.replace(/\s+/g, " ").trim().slice(0, n).trimEnd() + "…" : s.replace(/\s+/g, " ").trim());

export function ago(iso: string) {
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 45) return "now";
  if (s < 3600) return Math.round(s / 60) + "m";
  if (s < 86400) return Math.round(s / 3600) + "h";
  return Math.round(s / 86400) + "d";
}

/** Built-in sub-agents that are always on: can't be deleted or toggled off. */
export const REQUIRED_AGENTS: string[] = ["explore", "general"];
export const isRequiredAgent = (id: string) => REQUIRED_AGENTS.includes(id);
/** Count selected custom agents that still have definitions. */
export function countOptionalAgents(ids: string[], agents: { id: string }[]): number {
  const available = new Set(agents.map((agent) => agent.id));
  return ids.filter((id) => !isRequiredAgent(id) && available.has(id)).length;
}
/** Add any missing required agent id, preserving existing order. */
export function withRequiredAgents(ids: string[]): string[] {
  const out = [...ids];
  for (const r of REQUIRED_AGENTS) if (!out.includes(r)) out.push(r);
  return out;
}
