// Typed IPC surface for the Rust harness (src-tauri/src/lib.rs).
import { invoke } from "@tauri-apps/api/core";
import type { ContainerState, DockerEnv, WorkspaceMode } from "./workspaces";

/** Keys the Browser view forwards on their own. Mirrors Rust `browser_panel::PanelKey`; printable text goes through `type`. */
export type BrowserKey = "Enter" | "Tab" | "Backspace" | "Delete" | "Escape" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "Home" | "End" | "PageUp" | "PageDown" | "Space";
export interface BrowserModifiers { ctrl?: boolean; shift?: boolean; alt?: boolean; meta?: boolean }
export interface BrowserStyleChange { property: string; value: string }
/** Mirrors Rust `browser_panel::PanelAction` (closed: unknown kinds and fields are rejected). */
export type BrowserPanelAction =
  | { kind: "snapshot" | "back" | "forward" | "refresh" | "clear_preview" }
  | { kind: "navigate"; url: string }
  | { kind: "click"; x: number; y: number; button?: "left" | "middle" | "right"; click_count?: number }
  | { kind: "type"; text: string }
  | { kind: "key"; key: BrowserKey; modifiers?: BrowserModifiers }
  | { kind: "scroll"; delta_y: number; delta_x?: number; x?: number; y?: number }
  | { kind: "tab_new"; url?: string }
  | { kind: "tab_switch" | "tab_close"; id: string }
  | { kind: "dialog"; accept: boolean; text?: string }
  | { kind: "inspect"; x: number; y: number }
  | { kind: "preview_style"; selector: string; styles: BrowserStyleChange[] };
export interface BrowserTab { id: string; url: string; title: string; active: boolean }
export interface BrowserRect { x: number; y: number; width: number; height: number }
export interface BrowserElement { ref: string; selector: string; tag: string; role: string; name: string; bbox: BrowserRect; styles: Record<string, string> }
export interface BrowserDialog { kind: string; message: string; default_prompt: string }
/** Mirrors Rust `browser_panel::PanelFrame`. `png` is empty while a page dialog blocks the renderer: keep the last picture. */
export interface BrowserPanelFrame {
  url: string; title: string; png: string; width: number; height: number; viewport_width: number; viewport_height: number;
  tabs?: BrowserTab[]; can_back?: boolean; can_forward?: boolean; history?: string[]; dialog?: BrowserDialog | null; element?: BrowserElement | null;
}
export function browserPanel(taskId: string, action: BrowserPanelAction) {
  return invoke<BrowserPanelFrame>("browser_panel", { taskId, action });
}
/** Mirrors Rust `browser_annotate::Annotation`. `element` is absent for a dragged region. */
export interface BrowserAnnotation {
  comment: string; bbox: BrowserRect;
  element?: { ref: string; selector: string; tag: string; role: string; name: string };
  styles?: { property: string; before: string; after: string }[];
}
/** Send annotations made in the Browser view to the agent as one chat turn (screenshot crops attached). */
export function browserAnnotate(taskId: string, annotations: BrowserAnnotation[]) {
  return invoke<void>("browser_annotate", { taskId, annotations });
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
export interface AgentDef { id: string; name: string; description: string; prompt: string; model: string; tools: "all" | "read_only" | "no_shell"; color: string; builtin: boolean; source: string; inject_instructions?: boolean; effort?: number | null; groups?: ToolGroup[] }
export interface SkillDef { name: string; description: string; source: string; path: string; enabled: boolean; files: number; disable_model_invocation?: boolean; user_invocable?: boolean }
/** "Save as skill…": a SKILL.md the model drafted from one chat, for the user
 *  to edit before anything is written (`skill_draft_from_chat`). */
export interface SkillDraft {
  /** Suggested folder-safe name; "" when the model gave nothing usable. */
  name: string; description: string;
  /** Markdown body without frontmatter. */
  body: string;
  /** Notes to show prominently, e.g. what was redacted. */
  warnings: string[]; model: string;
  /** The chat's project folder; "" when it has none or is remote. */
  project: string;
  /** Project skills only load in trusted folders. */
  project_trusted: boolean;
  user_dir: string; project_dir: string;
}
export type SkillScope = "user" | "project";
export interface SkillSaveReq { id: string; name: string; description: string; body: string; scope: SkillScope; overwrite: boolean; allow_secrets: boolean }
/** `saved` false means nothing was written: `exists` and `secrets` say what
 *  needs the user's confirmation before resending. */
export interface SkillSaveResult { saved: boolean; exists: boolean; secrets: string[]; path: string; note: string }
export interface Route { id: string; name: string; heads: string[]; all: boolean; steps: string[]; on_exhausted: "pause" | "fail";
  description?: string;
  /** Models ordered weakest → strongest, for escalate / de-escalate. */
  ladder?: string[];
  rules?: RouteRule[] }
/** One ordered rule of a router; the first enabled match acts. */
export interface RouteRule { id: string; name: string; enabled: boolean; match: "all" | "any"; when: RouteCondition[]; then: RouteAction; note?: string }
export type RouteConditionKind = "stuck" | "errors_in_row" | "no_progress" | "agent" | "context" | "images" | "plan" | "keyword" | "headroom" | "budget" | "time" | "model" | "turns" | "always";
/** Which fields are read depends on `kind` (a string, so a kind written by a
 *  newer build survives a round trip); the backend ignores the rest. */
export interface RouteCondition { kind: string; not?: boolean;
  /** stuck: same failing call N× (default 2); errors_in_row: N failing calls in a row (3); no_progress: N rounds without a successful edit (5); turns: at least N turns (1). */
  count?: number;
  /** stuck / errors_in_row: only these tools count (empty = any). */
  tools?: string[];
  /** agent: "main", "subagent", subagent type ids, or system kinds (`ROUTER_SYSTEM_AGENTS`). */
  agents?: string[];
  /** context: estimated prompt tokens, inclusive bounds (0 = unbounded); min_pct = percent of the base model's window. */
  min_tokens?: number; max_tokens?: number; min_pct?: number;
  /** keyword: searched in the latest user message. */
  pattern?: string; regex?: boolean; case_sensitive?: boolean;
  /** headroom: pool kind (empty = the base model's provider) has less than below_pct % left. */
  provider?: string; below_pct?: number;
  /** budget: at least above_pct % of the monthly budget spent. */
  above_pct?: number;
  /** time: local "HH:MM" window (from > to wraps midnight); days 0 = Mon … 6 = Sun, empty = every day. */
  from?: string; to?: string; days?: number[];
  /** model: the base model is one of these ("prefix*" = prefix match). */
  models?: string[];
}
export interface RouteAction { kind: "use" | "escalate" | "deescalate" | "effort" | "stay" | (string & {});
  model?: string;
  /** escalate / deescalate: ladder steps (default 1). */
  steps?: number;
  /** 0 = Max … 4 = Low (`EFFORTS`); null = unchanged. */
  effort?: number | null;
  /** 0 = this request only; K = hold for K turns, then revert. */
  turns?: number }
export interface RouteHold { model: string; effort: number | null; turns_left: number; rule_id: string; rule_name: string; reason: string; from_model: string; action: string }
/** Everything a rule can look at, for one request. */
export interface RouteSituation { agent: string; sub: boolean; turn: boolean; base_model: string; base_effort: number; base_context: number; plan: boolean; images: boolean; context_tokens: number; user_text: string; repeat_failures: number; failing_tool: string; errors_in_row: number; last_error_tool: string; no_progress_turns: number; turns: number; headroom: Record<string, number>; budget_used_pct: number | null; minute_of_day: number; weekday: number; hold: RouteHold | null }
export interface RouteCondTrace { kind: string; matched: boolean; detail: string }
export interface RouteRuleTrace { rule_id: string; rule_name: string; enabled: boolean; matched: boolean; skipped: string; conditions: RouteCondTrace[] }
export interface RouteDecision { model: string; effort: number; action: "base" | "use" | "escalate" | "deescalate" | "effort" | "stay" | "hold" | "revert" | (string & {}); rule_id: string; rule_name: string; reason: string; hold: RouteHold | null; consume_stuck: boolean; trace: RouteRuleTrace[] }
/** One switch the router made in a chat, for the audit view. `at` is unix seconds. */
export interface RouteEvent { at: number; agent: string; agent_kind: string; from: string; to: string; effort_from: number; effort_to: number; action: string; rule_id: string; rule_name: string; reason: string }
export interface Window { label: string; used: number; resets_at: number }
/** A ceiling the user put on one usage window: the harness stops using that
 *  account once the window reaches `used` percent.
 *
 *  `until` 0 means the cap stands until it is lifted by hand; anything else is
 *  the unix time a temporary cap lifts by itself. `holding` is the backend's own
 *  answer to "is this cap what is keeping the account out of the pool right
 *  now", computed from the predicate the router uses, so a row cannot draw a cap
 *  as reached while the harness is still spending the account. */
export interface UsageCap { window: string; used: number; until: number; holding: boolean }
export interface AcctUsage { windows: Window[]; limited: boolean; plan: string; fetched: number; error: string }
export interface AccountView {
  id: string; kind: string; label: string; email: string; priority: number; enabled: boolean; source: string; disabled_reason: string;
  usage: AcctUsage | null; cooldown_until: number; cooldown_reason: string; available: boolean; active: boolean; expires_at: number;
  /** Masked OpenCode workspace-key suffix; absent on older backends, empty for OAuth logins. */
  key_hint?: string;
  /** Caps standing on this account, expired ones already left out by the backend. */
  caps?: UsageCap[];
}
/** How the most recent prompt split by category, in tokens.
 *
 *  An estimate, and shown as one: a provider reports only a prompt *total*, so
 *  the backend measures the request it built and rescales the parts onto that
 *  total. Absent on a task that has not run a turn yet, and on any task saved
 *  before this existed. */
export interface ContextParts { system: number; instructions: number; skills: number; tools: number; attachments: number; history: number; summary: number }
export interface Usage { input: number; output: number; cache_read: number; cache_write: number; cost: number; last_context: number; parts?: ContextParts }

export interface TaskSummary {
  id: string; title: string;
  /** The user named this chat themselves, so the agent's `set_title` is
   *  refused on it. Cleared from the sidebar's context menu. */
  titled: boolean;
  status: "idle" | "running" | "waiting" | "done" | "failed" | "stopped";
  waiting_kind: "approval" | "question" | "wake" | null;
  step: string; project: string; cwd: string; branch: string; base_branch: string; worktree: boolean;
  /** Present when this chat runs in a docker workspace (src/workspaces.ts). */
  docker?: DockerEnv | null; container?: ContainerState | null;
  /** The sandbox (an agent-owned desktop, src/sandboxApi.ts) this chat is attached to. */
  sandbox?: string | null;
  model: string; effort: number; perm: Perm; plan: boolean; ultra: boolean; ultra_wt: boolean; ultra_x: UltraX | null; subagents: boolean; goal: Goal | null;
  assist: Assist; agents: string[]; pending: { model?: string; assist?: Assist; agents?: string[] }; paused: Pause | null; serving: string; route: string; unpaused: boolean; busy: number;
  /** The background commands alive in this chat *now*, so the UI can name what a
   *  pause does not stop — see `Task::bg_now` in `src-tauri/src/agent/mod.rs`.
   *
   *  Read from the backend's live registry on every recount, never from
   *  `bg_live`, which is the record saved as the app quits and therefore names
   *  jobs the OS has already killed: a window that reloaded would list dead
   *  servers as running. */
  bg_now: Bg[];
  archived: boolean; hidden: boolean; forked_from: string | null; pinned: boolean; artifict_open: boolean; order: number;
  /** There is something in this chat the user has not looked at yet: the
   *  sidebar's blue dot.
   *
   *  The backend keeps this, not the window that draws it. A remote browser is a
   *  second client of the same app, and `remote/shim.js` reloads its page on
   *  every reconnect — a page that only had the live `ol://attention` event to go
   *  on painted every finished chat as if it had already been read. The backend
   *  sets it where it publishes attention (`Harness::attention`, the same rule
   *  `describe()` in `notify.ts` words into a toast) and clears it when a chat is
   *  opened, so every client draws the same dot. A question or an approval that
   *  is still pending stays marked until the backend ends the wait. */
  unread: boolean;
  todos: Todo[]; subs: SubInfo[]; usage: Usage; context_window: number;
  created_at: string; updated_at: string;
  /** When the user last did something here (messaged, answered, paused,
   *  stopped, resumed). `updated_at` moves on every agent step, so the chat
   *  list sorts on this one instead. */
  touched_at: string;
  /** Which build of this summary the backend made; higher is later state.
   *
   *  Every summary carries one — a live `task` event or a snapshot (`app_boot`'s
   *  list, `task_open`, `tasks_list`). The store keeps the highest it has seen
   *  per chat and refuses an older one, because a snapshot is built a moment
   *  before the events it races: applying it anyway used to put a chat back to
   *  "running" after it had finished, or back to "waiting" after the answer had
   *  been given, with nothing left to publish the truth afterwards. See
   *  `newerSummary` in store.ts. */
  rev: number;
}

/** One entry of a `computer` tool call's `actions` list. */
export interface ToolAction {
  [key: string]: unknown;
  type?: string; text?: string; keys?: string[]; x?: number; y?: number;
  monitor?: number; width?: number; height?: number; button?: string;
  scroll_x?: number; scroll_y?: number; ms?: number;
}

/** A tool call's parsed arguments. Free-form across every tool (path, pattern,
 *  ids, actions…), so only the fields the transcript reads in a typed position
 *  are named; the index signature keeps the rest reachable as `unknown` for
 *  narrowing. */
export interface ToolInput {
  [key: string]: unknown;
  path?: string; pattern?: string; glob?: string; to?: string; message?: string;
  ids?: string[]; mode?: string; edits?: unknown[]; command?: string; id?: string;
  url?: string; query?: string; queries?: string[]; urls?: string[]; action?: string;
  /** `http_request`: url/headers/body arrive already redacted by the backend. */
  method?: string; headers?: Record<string, string>; body?: string; json?: unknown;
  name?: string; kind?: string; summary?: string; file?: string; monitor?: number | string;
  selector?: string; text?: string; js?: string; number?: number; title?: string;
  repo?: string; actions?: ToolAction[]; run_in_background?: boolean; content?: string;
}

/** The free-form per-item payload the backend puts on `Item.data`. Different
 *  item kinds fill different fields — an approval card, a question form, a tool
 *  result — so the index signature stays `unknown` and a caller narrows what it
 *  reads. The named fields are the ones read in a typed position (rendered as
 *  text, compared to a literal, indexed). */
export interface ItemData {
  [key: string]: unknown;
  name?: string; input?: ToolInput; output?: string; status?: string; meta?: string;
  diff?: { k: "a" | "d" | "c" | "h"; t: string }[];
  images?: string[]; images_dropped?: number; exec?: boolean; bg_id?: string;
  /** Staged temp copies of what the user attached: path, name and size. Lets an
   *  edited message be resent with its attachments without re-reading files. */
  attachments?: { path: string; name: string; size: number }[];
  resolved?: string; kind?: string; detail?: string; title?: string; feedback?: string;
  rec_wt?: boolean | null; suggest_wt?: boolean; reason?: string; git?: boolean; rec_why?: string;
  sub?: string; repeat?: boolean; rule?: string;
  sub_id?: string; role?: string; background?: boolean; secs?: number;
  level?: string; summary?: string; quiet?: boolean; model?: string; via?: string;
  from?: string; question?: string; answer?: string; working?: boolean;
  path?: string; branch?: string; goal?: boolean; global?: boolean; replied?: string[];
  queued?: boolean; brief?: boolean; dismissed?: boolean; note?: string; sum?: string;
  intro?: string; label?: string; artifict_card?: Record<string, unknown>; preview_draft?: string;
  /** A plan-mode approval names the record the plan was filed as in the
   *  project's artificts, so the card can send the user to what they will come
   *  back to. Absent when the plan could not be filed (no project folder). */
  plan_artifict?: { id?: string; version_id?: string; title?: string };
  questions?: import("./ui/Question").Q[]; notes?: string[]; answers?: unknown[];
}

export interface Item {
  id: string;
  kind: "user" | "text" | "thinking" | "tool" | "approval" | "question" | "asklater" | "sub" | "notice" | "user_notice" | "artifict";
  text: string;
  data: ItemData;
  ts: string;
}

export interface Bg { id: string; cmd: string; started: string; last_line: string; running: boolean; exit: string | null }

export type Perm = "disabled" | "allowlist" | "auto" | "turbo" | "reviewer";
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
export interface RemoteModel { id: string; name: string; context: number | null; output: number | null; input_price: number | null; output_price: number | null; modalities: string[] | null; reasoning_levels?: string[] | null; reasoning_param?: string | null; reasoning_parameter?: string | null; supported_reasoning_levels?: (string | { effort?: string })[] | null; supported_parameters?: string[] | null; guessed?: boolean }
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

/** One low-priority prompt waiting for spare subscription quota. `model` empty =
 *  any subscription provider; `perm` empty = the default mode, and never
 *  stronger than it (the backend clamps). `error` = why the last start failed. */
export interface BacklogItem { id: string; project: string; prompt: string; model: string; perm: string; created_at: number; error: string }
export interface BurnerCfg { enabled: boolean; reset_within_min: number; min_headroom: number; spend_cap: number; queue: BacklogItem[] }
export const BURNER_DEFAULTS: BurnerCfg = { enabled: false, reset_within_min: 30, min_headroom: 20, spend_cap: 90, queue: [] };
/** A chat the quota burner started. `ended_at` 0 = still running. */
export interface BurnRun { task_id: string; item_id: string; project: string; prompt: string; model: string; perm: string; account_id: string; account_label: string; kind: string; window: string; resets_at: number; started_at: number; ended_at: number; end_reason: string }
export interface BurnerAccount { account_id: string; label: string; kind: string; head: boolean; window: Window | null; reason: string }
export interface BurnerStatus { runs: BurnRun[]; accounts: BurnerAccount[] }

/** One swap-dialog row: agents on `from` at level `from_effort` move to `to`, and onto `to_effort` when set. */
export interface SwapRule { from: string; from_effort: number; to: string; to_effort: number | null }
export interface Settings {
  model: string; effort: number; perm: Perm; worktree: boolean; subagents: boolean;
  /** Automatically rebuild Docker images when config changes; defaults on for old settings. */
  docker_auto_rebuild?: boolean;
  /** Workspace for new tasks; empty on older settings, which follow `worktree`. */
  workspace?: WorkspaceMode | ""; docker_idle_minutes?: number;
  ui_size: string; ui_zoom: number; send_with: "enter" | "ctrl-enter"; theme?: "raycast" | "lavender" | "nord" | "dracula" | "gruvbox" | "tokyo" | "rosepine" | "catppuccin" | "solarized" | "everforest" | "monokai" | "ayu" | "ocean";
  /** Let Enter send in a phone browser, with Shift Enter the newline. Off by
   *  default: a phone's on-screen Return key cannot be trusted to report Shift,
   *  so there Enter starts a new line and the send button sends. The desktop
   *  window is unaffected and follows `send_with`. */
  phone_enter_sends?: boolean;
  /** Light or dark, or follow the OS. Absent on settings saved before light mode. */
  mode?: "light" | "dark" | "system";
  routes: Route[]; agents: AgentDef[]; default_agents: string[]; assist: Assist; paused_all: boolean; paused_reason: string; close_to_tray: boolean; notify?: boolean; home_usage: string[]; model_colors: Record<string, string>;
  /** Settings page the sidebar footer's shortcut button — the one beside the
   *  usage bar — opens. A settings-tab id; Accounts on settings saved before
   *  this existed, and an id this build has no page for falls back to it. */
  sidebar_button_tab?: string;
  /** Cross-chat inbox for pending approvals and questions. Hidden by default. */
  needs_you?: boolean;
  /** Sidebar lists chats from every project folder, not just the current one. */
  all_projects: boolean;
  /** Show the optional GitHub pull-request screen beside Saved prompts. */
  show_schedules?: boolean;
  /** Wizard for the first-run welcome; absent before the wizard existed. */
  welcome_seen?: boolean;
  show_pull_requests?: boolean;
  /** Order of the Saved prompts list: "newest", "folder" or "az". */
  saved_sort?: string;
  show_thinking: boolean;
  /** Draw the estimated per-category split of the context window under the
   *  meter. Off by default: the ring's used/limit pair is measured, the split
   *  is an estimate rescaled to the provider's count. */
  ctx_breakdown?: boolean;
  inject_global_claude: boolean;
  /** Let the agent rename this chat as the work moves on. On by default. */
  agent_titles?: boolean;
  /** OTLP/HTTP endpoint to send traces to. Empty (the default) means nothing is
   *  exported at all — no client, no thread, no allocation. This is not
   *  telemetry on by another name: the user names a collector *they* run, and
   *  nothing is sent anywhere else. */
  trace_endpoint?: string;
  /** "Spend quota before it resets" (Settings → Accounts). Absent on settings
   *  saved before it existed, which reads as off with an empty queue. */
  burner?: BurnerCfg;
  /** Keep chats ordered automatically by attention and recent activity, even after a manual drag. On by default. */
  automatic_chat_reorder?: boolean;
  /** Resume chats paused when provider accounts/keys were exhausted after one becomes available. On by default. */
  automatic_resume?: boolean;
  /** Long-term memory: global notes about the user plus per-project notes,
   *  loaded into every new chat. On by default; absent reads as on. */
  memory?: boolean;
  /** Projects (folder paths) whose own notes are kept in
   *  `<project>/.openleash/memory/` so they can be committed, instead of under
   *  `~/.openleash/memory/`. Absent reads as none. */
  memory_in_project?: string[];
  /** Let an agent search the user's other chats, see what each is doing, and
   *  send one of them a message. On by default. */
  cross_chat?: boolean;
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
  /** Per-project opt-out of individual project instruction files. */
  project_docs?: ProjectDocRule[];
  /** How an agent-made commit is attributed in git log. */
  git_attribution?: Attribution;
  /** Run the repository's own git hooks when the agent commits. Off by default. */
  git_commit_verify?: boolean;
  /** Keep the provider's cached prefix warm during long idle periods. */
  cache_keepalive?: boolean;
  /** Maximum number of keepalive pings per task. */
  cache_keepalive_pings?: number;
  /** Keep a chat's row in Stats after the chat is deleted. On unless set false. */
  keep_chat_stats?: boolean;
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
export interface ServicePluginCfg { enabled: boolean; base_url?: string; email?: string }
export type ServicePluginId = "linear" | "jira" | "slack" | "notion" | "gitlab";
export interface ServiceTestResult { ok: boolean; identity?: string; source?: string; error?: string }
export interface Plugins {
  github: { enabled: boolean; client_id?: string }; computer: { enabled: boolean; settle_ms: number }; browser: BrowserPluginCfg;
  /** Absent in settings written before these plugins existed; treat as off. */
  linear?: ServicePluginCfg; jira?: ServicePluginCfg; slack?: ServicePluginCfg; notion?: ServicePluginCfg; gitlab?: ServicePluginCfg;
}
/** Mirrors Rust `plugins::BrowserCfg`. Per-site exceptions are permission rules (`browser:site <host>`, `browser:ask <host>`, `!browser:site <host>`, `browser:lan <host>`), not fields here. */
export interface BrowserPluginCfg { enabled: boolean; width: number; height: number; persistent_profile: boolean; headed: boolean; developer_mode: boolean; interactions: "ask" | "allow" | "block"; download_dir: string }

/** Folder trust. A folder with no decision is untrusted until the user says otherwise. */
export type TrustKind = "folder" | "parent";
export type TrustState = "trusted" | "untrusted";
export interface TrustDecision { path: string; kind: TrustKind; decision: TrustState; decided_at: string }
export interface TrustMatched { kind: TrustKind; path: string; decision: TrustState }
/** Per-project opt-out of individual project instruction files (`MEMORY_FILES`).
 *  No rule for a folder = every file it has is loaded. This is a selection, not
 *  a trust decision: it never weakens folder trust. */
export interface ProjectDocRule { path: string; disabled: string[] }
/** One file that would be injected into the system prompt. */
export interface ManifestFile { name: string; path: string; bytes: number; chars: number; truncated: boolean; pointer_only: boolean; disabled: boolean; global: boolean; preview: string }
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
  computer: { ok: boolean; error?: string; monitors: { name: string; w: number; h: number; primary: boolean; /** Whether the agent may move the cursor on this display: the primary one, unrotated. The others can be looked at but not clicked. */ driveable: boolean; rotation: number; x: number; y: number }[] };
  browser: { ok: boolean; error?: string; browser?: string };
  /** Whether each service-plugin credential is saved in settings, e.g. `secrets.slack.user_token`. */
  secrets?: Partial<Record<ServicePluginId, Record<string, boolean>>>;
}

export interface Boot { settings: Settings; providers: ProviderView[]; provider_presets: ProviderPreset[]; models: ModelInfo[]; tasks: TaskSummary[]; accounts: AccountView[]; agents: AgentDef[]; skills: SkillDef[]; shell: string; data_dir: string; month_spend: number; user_notices?: Record<string, Item[]>; /** Whether a file named `ALLOW_CLAUDE` sits beside the executable, opting this install into the Claude subscription integration. Absent from an older backend, which reads as "not opted in". */ claude_allowed?: boolean }

/** What the monthly cap is actually counting: the per-day spend ledger the
 *  backend keeps, plus the running token total. Survives a stats reset, so it
 *  is not derivable from `stats_get`. */
export interface UsageView { month: number; spend: Record<string, number>; tokens: number; budget: number }
/** One lane of a chat's run timeline (`task_run_timeline`): the main agent
 *  (`id: "main"`), one per subagent (`id` = its sub id, nested by `parent`/
 *  `depth`), and `"bg"` for background commands. `item` is the transcript row
 *  that stands for the lane (a subagent's pill in the main transcript). */
export interface RunLane { id: string; label: string; role: string; parent: string; depth: number; status: string; item: string; start_ms: number | null; end_ms: number | null; rows: number }
/** One block on a lane. `kind`: `model` (one request attempt), `tool`, `wait`
 *  (`name`: approval | question | pause | park | retry), or `bg`. `row` stacks
 *  blocks that overlap on one lane (parallel tool calls). `estimated` = derived
 *  from transcript timestamps because the chat predates recorded timings; `open`
 *  = still running, and `end_ms` is then the backend's `now_ms`. `item` is the
 *  transcript row to jump to (empty when none is known); a block on a subagent
 *  lane names a row in that subagent's own transcript. */
export interface RunBlock {
  lane: string; row: number; kind: "model" | "tool" | "wait" | "bg"; name: string;
  start_ms: number; end_ms: number; open: boolean; status: string; item: string; estimated: boolean;
  account?: string; role?: string; ttft_ms?: number | null; input?: number; output?: number;
  cache_read?: number; cache_write?: number; cost?: number; attempt?: number; fallback?: boolean; detail?: string;
}
export interface RunModelTotal { model: string; requests: number; failed: number; cost: number; ms: number; input: number; output: number; cache_read: number; cache_write: number }
export interface RunSlowTool { name: string; ms: number; lane: string; item: string; start_ms: number }
/** Exclusive wall-clock split: every instant of `wall_ms` is counted once, in
 *  the first of you → paused → model → retry → tool → idle that was active on
 *  any lane, so the parts add up to the wall time. */
export interface RunTotals {
  wall_ms: number; you_ms: number; paused_ms: number; model_ms: number; retry_ms: number; tool_ms: number; idle_ms: number; bg_ms: number;
  requests: number; failed_requests: number; retries: number; fallbacks: number; tool_calls: number; tool_errors: number;
  cost: number; task_cost: number; by_model: RunModelTotal[]; slowest_tools: RunSlowTool[];
}
export interface RunTimeline { task_id: string; start_ms: number | null; end_ms: number | null; now_ms: number; recorded: boolean; partial: boolean; lanes: RunLane[]; blocks: RunBlock[]; totals: RunTotals }
export interface FileDiff { path: string; status: string; add: number; del: number; lines: { k: "a" | "d" | "c" | "h"; t: string }[] }
/** One before/after screenshot pair of a URL the agent asked to compare.
 *  `before`/`after` are absolute local PNG paths; `*_at` are RFC3339. */
export interface VisualEntry { url: string; before: string; after: string | null; before_at: string; after_at: string | null }
export interface VisualDiff { entries: VisualEntry[] }
/** One numbered step of a chat's edit history, as checkpoint bisect walks it.
 *  `index` is 1-based. Most steps are agent file edits; `tool === "external"`
 *  is the files that changed between two agent edits by something else (a shell
 *  command, the user): `item_id` is then empty and `path` a short summary. */
export interface BisectEdit { index: number; item_id: string; sub_id: string | null; msg_index: number; tool: string; path: string; add: number; del: number; lines: { k: "a" | "d" | "c" | "h"; t: string }[] }
export type BisectVerdict = "good" | "bad" | "timeout" | "flaky" | "error";
/** One run of the test command against the file state after step `state`
 *  (`from - 1` is the state before the range). */
export interface BisectProbe { state: number; verdict: BisectVerdict; code: number | null; ms: number; output: string }
export type BisectOutcome = "found" | "all_good" | "already_bad" | "inconclusive" | "cancelled";
export interface BisectReport { outcome: BisectOutcome; culprit: BisectEdit | null; candidates: BisectEdit[]; probes: BisectProbe[]; from: number; to: number; total: number; message: string }
/** The steps a bisect could walk. Their `lines` are empty here (no diffs);
 *  `available: false` comes with the `reason` a bisect cannot run. */
export interface BisectEditsInfo { edits: BisectEdit[]; available: boolean; reason: string }
/** Live progress of a bisect, on `ol://event` with `kind: "bisect"`. */
export type BisectEvent =
  | { phase: "start"; from: number; to: number; total: number }
  | { phase: "probe"; state: number; step: number }
  | { phase: "probed"; probe: BisectProbe }
  | { phase: "done"; report: BisectReport };

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

/** A WSL distro (`agent::projects::WslDistro`). `root` is `\\wsl.localhost\<name>`. */
export interface WslDistro { name: string; root: string }
/** What a clone would do (`agent::projects::ClonePlan`). `existing`: `dest` is already a checkout of this repo. */
export interface ClonePlan { url: string; name: string; dest: string; existing: boolean }

export interface NewTask { prompt: string; project: string; model: string; effort: number; perm: Perm; plan: boolean; worktree: boolean; workspace?: WorkspaceMode; base_branch: string; subagents: boolean; assist: Assist; agents: string[]; route: string; images?: string[]; image_names?: string[]; files?: string[]; ultra?: boolean; ultra_wt?: boolean; ultra_x?: UltraX | null }
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
export type TaskPatch = Partial<Pick<TaskSummary, "model" | "effort" | "perm" | "plan" | "ultra" | "ultra_wt" | "ultra_x" | "subagents" | "title" | "assist" | "agents" | "archived" | "pinned" | "artifict_open" | "order" | "route">> & { now?: boolean; apply_pending?: boolean; /** Hand the name back to the agent: clears the manual-rename lock. */ untitled?: boolean };

/** What the user picked when resuming a stopped chat. `continue` retries what was
 *  cut off; `summarise` reports where things stand and then carries on; `wrap`
 *  reports and stops there. Mirrors the modes `task_resume` accepts in Rust. */
export type ResumeMode = "continue" | "summarise" | "wrap";

/** A file the user attached for the agent to read. The model gets the path,
 *  except for images, which ride along as real image blocks so it can see
 *  them. Produced by the `files_attach` command, which is the only way this
 *  app can learn a real path: the browser `File` object deliberately has none. */
export interface FileRef { path: string; name: string; size: number; isDir: boolean; image: boolean }

/** Scheduled / recurring agent runs (`~/.openleash/schedules.json`). Each fire
 *  starts a normal chat titled "⏰ <name>". */
export type Weekday = "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun";
export type ScheduleTiming =
  | { kind: "interval"; every: number; unit: "minutes" | "hours" }
  | { kind: "daily"; time: string }
  | { kind: "weekly"; days: Weekday[]; time: string };
export interface ScheduleRun { at: string; status: "started" | "skipped" | "failed"; message: string; task_id: string | null }
export interface Schedule {
  id: string; name: string; enabled: boolean; prompt: string; project: string;
  model: string; effort: number; perm: Perm; plan: boolean;
  workspace: "local" | "worktree"; base_branch: string;
  timing: ScheduleTiming; catch_up: boolean; created_at: string;
  next_run_at: string | null; last_run_at: string | null;
  last_status: "" | "started" | "skipped" | "failed"; last_message: string;
  last_task_id: string | null; history: ScheduleRun[];
}
export interface SchedulesState { revision: number; paused: boolean; schedules: Schedule[] }
export type ScheduleDraft = Omit<Schedule, "id" | "created_at" | "next_run_at" | "last_run_at" | "last_status" | "last_message" | "last_task_id" | "history"> & { id?: string };

export interface AppUpdateStatus {
  status: "idle" | "checking" | "available" | "downloading" | "ready" | "error" | "current";
  version: string | null;
  error: string | null;
  can_install: boolean;
}

/** One page of a chat's timeline, worst-newest first. `cursor` is the id of the
 *  oldest item in `items`, the anchor for the next older page. */
export interface TaskOpenPage { summary: TaskSummary; items: Item[]; bg: Bg[]; cursor: string | null; total: number }
/** One older page. `cursor: null` means the history is exhausted. */
export interface TaskHistoryPage { items: Item[]; cursor: string | null; total: number }

export const api = {
  appUpdateStatus: () => invoke<AppUpdateStatus>("app_update_status"),
  appUpdateCheck: () => invoke<AppUpdateStatus>("app_update_check"),
  appUpdateDownload: () => invoke<AppUpdateStatus>("app_update_download"),
  appUpdateInstall: () => invoke<void>("app_update_install"),
  boot: () => invoke<Boot>("app_boot"),
  tasks: () => invoke<TaskSummary[]>("tasks_list"),
  task: (id: string) => invoke<{ summary: TaskSummary; items: Item[]; sub_items: Record<string, Item[]>; bg: Bg[] }>("task_get", { id }),
  taskOpen: (id: string) => invoke<TaskOpenPage>("task_open", { id }),
  /** The chat is on screen and got attention, so it is already read: clears the backend's unread mark. */
  taskSeen: (id: string) => invoke<void>("task_seen", { id }),
  taskHistory: (id: string, cursor: string | null, limit = 100) => invoke<TaskHistoryPage>("task_history", { id, cursor, limit }),
  /** A subagent's report, fetched when its panel opens: the live `task` event
   *  leaves reports out, because they are large and rarely change. */
  subReport: (id: string, sub: string) => invoke<string>("sub_report", { id, sub }),
  /** One subagent's own transcript, read when its panel opens: `task_open` sends only the main timeline. */
  subTranscript: (id: string, sub: string) => invoke<Item[]>("sub_transcript", { id, sub }),
  create: (req: NewTask) => invoke<TaskSummary>("task_create", { req }),
  send: (id: string, text: string, later = false, images?: string[], files?: string[], imageNames?: string[]) => invoke<void>("task_send", { id, text, later, images, files, imageNames }),
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
  /** Drop the per-chat stats rows of deleted and archived chats; returns how many. */
  statsPruneChats: () => invoke<number>("stats_prune_chats"),
  /** One chat's stats, by id — the panel behind "Stats…" in a chat's context
   *  menu. Its own command because `stats_get` returns the whole ledger and
   *  opening one chat's numbers does not need the rest of it.
   *
   *  `null` when the chat has no row: it never ran a request, or it has aged
   *  out of the cap the backend keeps. An empty title means the chat itself is
   *  gone — its numbers are kept after deletion on purpose. */
  statsChat: (id: string) => invoke<[string, import("./ui/Stats").ChatStat] | null>("stats_chat", { id }),
  /** Where one chat's run spent its time and money, assembled from the timings
   *  recorded with the task (`agent/spans.rs`). A chat from before those were
   *  recorded comes back `partial`, with blocks estimated from its transcript. */
  runTimeline: (id: string) => invoke<RunTimeline>("task_run_timeline", { id }),
  draftGet: (key: string) => invoke<string>("draft_get", { key }),
  draftSet: (key: string, text: string) => invoke<void>("draft_set", { key, text }),
  /** Resolve picked/dropped absolute paths into attachments. `cwd` is the
   *  chat's folder: for a remote (SSH) one, non-image files are refused. */
  filesAttach: (paths: string[], cwd?: string) => invoke<FileRef[]>("files_attach", { paths, cwd: cwd ?? null }),
  /** Read an attached image into a data URL so the model can see it. */
  fileDataUrl: (path: string) => invoke<string>("file_data_url", { path }),
  remoteUpload: (id: string, filename: string, bytes: ArrayBuffer) => invoke<FileRef>("remote_stage_upload", { id, filename, bytes: Array.from(new Uint8Array(bytes)) }),
  /** A picture an agent's reply points at, confined to the chat's folder (`reply_image`). */
  replyImage: (cwd: string, path: string) => invoke<string>("reply_image", { cwd, path }),
  subSend: (id: string, subId: string, text: string, images?: string[], imageNames?: string[]) => invoke<void>("sub_send", { id, subId, text, images, imageNames }),
  subModel: (id: string, subId: string, model: string) => invoke<void>("sub_set_model", { id, subId, model }),
  subStop: (id: string, subId: string) => invoke<void>("sub_stop", { id, subId }),
  subEffort: (id: string, subId: string, effort: number | null) => invoke<void>("sub_set_effort", { id, subId, effort }),
  dismissPause: (id: string) => invoke<void>("task_dismiss_pause", { id }),
  pause: (id: string) => invoke<void>("task_pause", { id }),
  statusSummary: (id: string) => invoke<{ text: string; model: string; via?: string }>("task_status_summary", { id }),
  forcePause: (id: string) => invoke<void>("task_force_pause", { id }),
  forcePauseAll: () => invoke<void>("force_pause_all"),
  /** Resume a stopped chat. `mode` is what the user picked after a stop: keep going
   *  (`continue`), report where things stand and then keep going (`summarise`), or
   *  report and stop there (`wrap`). Omitting it is a plain resume. */
  resume: (id: string, message?: string, mode?: ResumeMode) => invoke<void>("task_resume", { id, message: message || null, mode: mode ?? null }),
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
  burnerStatus: () => invoke<BurnerStatus>("burner_status"),
  accountImport: (kind: string, text: string) => invoke<AccountView[]>("account_import", { kind, text }),
  accountImportLocal: (kind: string, path?: string) => invoke<AccountView[]>("account_import_local", { kind, path: path || null }),
  accountUpdate: (id: string, patch: { label?: string; priority?: number; enabled?: boolean; move?: number }) => invoke<AccountView[]>("account_update", { id, patch }),
  accountRemove: (id: string) => invoke<AccountView[]>("account_remove", { id }),
  /** Cap a usage window across accounts: each stops being used once that window
   *  reaches `used` percent. `secs` 0 = until lifted by hand; the backend stamps
   *  the deadline against its own clock, since a phone driving this backend is
   *  not a clock this page can trust. */
  accountCapSet: (ids: string[], window: string, used: number, secs: number) => invoke<AccountView[]>("account_cap_set", { ids, window, used, secs }),
  accountCapClear: (ids: string[], window: string) => invoke<AccountView[]>("account_cap_clear", { ids, window }),
  accountsRefresh: () => invoke<AccountView[]>("accounts_refresh"),
  exportTask: (id: string, path: string) => invoke<void>("task_export", { id, path }),
  /** The ticked chats into one file. Its own command rather than a loop of
   * `exportTask`: N round-trips is N writes of work to end up in one file, and
   * falling back to `exportAll` would quietly drag in chats nobody ticked.
   * Returns how many went in. */
  exportTasks: (ids: string[], path: string) => invoke<number>("tasks_export", { ids, path }),
  exportAll: (path: string, includeArchived: boolean) => invoke<number>("tasks_export_all", { path, includeArchived }),
  /** Open an exported chat file as new chats. The chats are sanitised like a transfer from
   * another machine; `project` forces a folder, else each lands in the matching project here
   * (or the home folder, with an entry in `warnings`). */
  importTasks: (path: string, project: string | null = null) => invoke<{ chats: TaskSummary[]; warnings: string[] }>("tasks_import", { path, project }),
  /** Point a stopped chat at another existing folder on this machine. */
  changeFolder: (id: string, folder: string) => invoke<void>("task_change_folder", { id, folder }),
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
  skillDraftFromChat: (id: string, model?: string) => invoke<SkillDraft>("skill_draft_from_chat", { id, model: model || null }),
  skillDraftCancel: (id: string) => invoke<void>("skill_draft_cancel", { id }),
  skillSaveFromChat: (req: SkillSaveReq) => invoke<SkillSaveResult>("skill_save_from_chat", { req }),
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
  /** Just the totals of `review` (files changed, lines added/removed), without the diff lines. */

  visualDiff: (id: string) => invoke<VisualDiff>("task_visual_diff", { id }),
  /** Re-capture every entry's "after" shot in a headless browser. Rejects with
   *  a readable reason (e.g. the browser plugin is off). */
  visualRecapture: (id: string) => invoke<VisualDiff>("task_visual_recapture", { id }),
  /** Discard this chat's visual diff and its files. */
  visualClear: (id: string) => invoke<null>("task_visual_clear", { id }),

  reviewStats: (id: string) => invoke<{ files: number; add: number; del: number }>("task_review_stats", { id }),
  revertFile: (id: string, path: string) => invoke<void>("task_revert_file", { id, path }),
  bisectEdits: (id: string) => invoke<BisectEditsInfo>("task_bisect_edits", { id }),
  /** Resolves with the final report; rejects with the refusal (a Deny rule,
   *  plan mode, another bisect running, a bad range). `timeout` is in ms. */
  bisectRun: (id: string, opts: { command: string; from?: number | null; to?: number | null; timeout?: number | null; runs?: number | null; share?: string[] | null }) =>
    invoke<BisectReport>("task_bisect_run", { id, command: opts.command, from: opts.from ?? null, to: opts.to ?? null, timeout: opts.timeout ?? null, runs: opts.runs ?? null, share: opts.share ?? null }),
  bisectCancel: (id: string) => invoke<boolean>("task_bisect_cancel", { id }),
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
  modelDetail: (prov: string, model: string) => invoke<Record<string, unknown>>("model_detail", { prov, model }),
  mcpStatus: () => invoke<McpStatus[]>("mcp_status"),
  mcpReconnect: () => invoke<void>("mcp_reconnect"),
  mcpOauthStart: (name: string, clientId?: string, scopes?: string[], redirectPort?: number) => invoke<{ flow_id: string; authorization_url: string }>("mcp_oauth_start", { name, clientId: clientId ?? null, scopes: scopes ?? null, redirectPort: redirectPort ?? null }),
  mcpOauthWait: (flowId: string) => invoke<void>("mcp_oauth_wait", { flowId }),
  mcpOauthCancel: (flowId: string) => invoke<void>("mcp_oauth_cancel", { flowId }),
  mcpOauthDisconnect: (name: string) => invoke<void>("mcp_oauth_disconnect", { name }),

  /** What the swap dialog lists. `rows` is one entry per model actually in use.
   *  `agents` names the subagent types a `spread` would reach — only the global
   *  dialog gets them, and only so the opt-in can say what it is about to touch. */
  /** The switches the router has made in a chat, oldest first. */
  routeLog: (id: string) => invoke<RouteEvent[]>("route_log", { id }),
  modelsInUse: (id?: string) => invoke<{ rows: { model: string; effort: number; uses: string[] }[]; agents: { id: string; name: string }[] }>("models_in_use", { id: id ?? null }),
  /** `effort` is keyed by the model being swapped *away from*, like `map`: the
   *  reasoning level picked for the destination travels with that row. `spread`
   *  is the separate "and the subagent types too" opt-in: without it nothing a
   *  future spawn of an agent type would read is touched. */
  modelsSwap: (map: Record<string, string>, id?: string, effort?: Record<string, number>, spread?: boolean, rules?: SwapRule[]) => invoke<Settings>("models_swap", { id: id ?? null, map, effort: effort ?? null, spread: spread ?? false, rules: rules ?? null }),
  githubPrs: (project: string, state: "open" | "closed" | "all" = "open") => invoke<{ repo: string; prs: { number: number; title: string; state: string; draft: boolean; html_url: string; user: string; head: string; base: string; updated_at: string }[] }>("github_list_prs", { project, state }),
  pluginsStatus: () => invoke<PluginsStatus>("plugins_status"),
  githubLoginStart: () => invoke<{ user_code: string; verification_uri: string; device_code: string; interval: number; expires_in: number }>("github_login_start"),
  githubLoginWait: (deviceCode: string, interval: number, expiresIn: number) => invoke<string>("github_login_wait", { deviceCode, interval, expiresIn }),
  githubLoginCancel: () => invoke<void>("github_login_cancel"),
  githubToken: (token: string) => invoke<void>("plugin_github_token", { token }),
  /** Save (or, with "", remove) a service-plugin credential such as (`slack`, `user_token`). */
  pluginSecretSet: (plugin: ServicePluginId, field: string, value: string) => invoke<void>("plugin_secret_set", { plugin, field, value }),
  /** "Test connection": who the credentials authenticate as. Never returns a token. */
  pluginServiceTest: (plugin: ServicePluginId) => invoke<ServiceTestResult>("plugin_service_test", { plugin }),
  project: (path: string) => invoke<{ path: string; exists: boolean; git: boolean; branch: string; branches: string[]; memory: string[]; docs: string[]; disabled_docs: string[]; trusted: boolean; decided: boolean }>("project_info", { path }),
  projectBranchCreate: (path: string, name: string, base: string) => invoke<string[]>("project_branch_create", { path, name, base }),
  /** Installed WSL distros (Windows only; empty elsewhere or without WSL). */
  wslList: () => invoke<WslDistro[]>("wsl_list"),
  /** Where the folder dialog should start inside a distro: its one user's home, else `/home`. */
  wslStartDir: (distro: string) => invoke<string>("wsl_start_dir", { distro }),
  /** What cloning `url` would do — the normalised URL and the folder — without doing it. */
  clonePreview: (url: string) => invoke<ClonePlan>("clone_preview", { url }),
  /** Clone into `~/openleash/<name>` (or reuse a checkout of the same repo there); resolves to its path. */
  cloneRepo: (url: string) => invoke<string>("clone_repo", { url }),
  /** What a folder would inject, trusted or not — the trust dialog's evidence
   *  and the manifest panel's data. */
  trustManifest: (path: string) => invoke<TrustManifest>("trust_manifest", { path }),
  /** Apply the saved trace endpoint to the running exporter and report what it
   *  did. Takes no argument on purpose: a webview cannot point traces somewhere
   *  the settings file does not already say. */
  traceConfigure: () => invoke<{ enabled: boolean; endpoint: string; dropped: number; error: string | null }>("trace_configure"),
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
  routeDryRun: (route: Route, situation: RouteSituation) => invoke<RouteDecision>("route_dry_run", { route, situation }),
  routeSituation: (id: string, agent: string | null = null) => invoke<RouteSituation>("route_situation", { id, agent }),
  /** The chat a toast click asked for before the window was listening, or null.
   *  Call once the `ol://toast-click` listener is attached: the call is what
   *  tells the backend clicks may be emitted from now on. */
  takeToastClick: () => invoke<string | null>("take_toast_click"),
  schedulesList: () => invoke<SchedulesState>("schedules_list"),
  scheduleSave: (schedule: ScheduleDraft) => invoke<Schedule>("schedule_save", { schedule }),
  scheduleDelete: (id: string) => invoke<void>("schedule_delete", { id }),
  scheduleSetEnabled: (id: string, enabled: boolean) => invoke<Schedule>("schedule_set_enabled", { id, enabled }),
  scheduleRunNow: (id: string) => invoke<Schedule>("schedule_run_now", { id }),
  schedulesSetPaused: (paused: boolean) => invoke<SchedulesState>("schedules_set_paused", { paused }),
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
 *  and unknown non-empty ids fail closed through permRung.
 *
 *  `reviewer` is the one entry that is not a rung of its own: it behaves as
 *  Allowlist only and adds a model in place of the user's click, so it sits
 *  next to allowlist rather than above it. */
export const PERMS: { id: Perm; name: string; color: string; desc: string }[] = [
  { id: "disabled", name: "Disabled", color: "#8b8f98", desc: "Read-only commands only. Edits and commands are refused, not asked about." },
  { id: "allowlist", name: "Allowlist only", color: "#33d6ff", desc: "Reads freely, runs allow-listed commands. Asks before anything else." },
  { id: "reviewer", name: "Reviewer", color: "#b9a779", desc: "Same as Allowlist only, except a second model answers each prompt instead of you. A refusal is final for that call; an allow rule still overrides it." },
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
    // Reviewer mode asks exactly what allowlist asks; a model answers instead
    // of the user. Named explicitly so a later rung cannot silently swallow it.
    case "reviewer": return "allowlist";
    default: return "allowlist";
  }
}

/** The ladder entry a stored id should be *shown* as.
 *
 * The same as [`permRung`] except that reviewer mode is its own entry rather
 * than folding into Allowlist only: the difference between "you will be asked"
 * and "a model will answer for you" is the whole point of the setting, and a
 * label that hid it would misdescribe who is approving what. */
export function permId(id: string | undefined): Perm {
  return id === "reviewer" ? "reviewer" : permRung(id);
}

/** The display name of a rung, tolerant of legacy ids. */
export function permLabel(id: string | undefined): string {
  return PERMS.find((p) => p.id === permId(id))?.name ?? "";
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
/** A path key for comparisons. Local paths fold slashes and case (Windows).
 *  A remote `ssh://<host>/<path>` URI only folds its scheme and host: the
 *  path is POSIX and case-sensitive, and a backslash is a legal character
 *  in it, so `/app/Readme` and `/app/README` must stay two files. */
export const normPath = (p: string) => {
  const m = /^ssh:\/\/([^/]*)(.*)$/i.exec(p);
  if (m) return "ssh://" + (m[1] ?? "").toLowerCase() + ((m[2] ?? "").replace(/\/+$/, "") || "/");
  return p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
};
/** One line for a list row: runs of whitespace collapsed, trimmed to length. */
export const oneLine = (s: string, n = 160) => (s.replace(/\s+/g, " ").trim().length > n ? s.replace(/\s+/g, " ").trim().slice(0, n).trimEnd() + "…" : s.replace(/\s+/g, " ").trim());

export function ago(iso: string) {
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 45) return "now";
  if (s < 3600) return Math.round(s / 60) + "m";
  if (s < 86400) return Math.round(s / 3600) + "h";
  return Math.round(s / 86400) + "d";
}

/** Built-in subagents that are always on: can't be deleted or toggled off. */
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

// ── Best-of-N race ─────────────────────────────────────────────────────────
// One prompt, N independent attempts (each its own chat in its own git
// worktree), then project checks and a read-only judge rank them. Mirrors the
// Rust `race` module's serialized types; keep the field names aligned.

/** One attempt's model and effort (0 = max … 4 = low, as on a task). */
export interface RaceAttemptSpec { model: string; effort: number }
export interface NewRace {
  prompt: string; project: string; base_branch: string; perm: Perm; plan: boolean; assist: Assist; agents: string[]; route?: string;
  attempts: RaceAttemptSpec[]; judge_model: string; judge_effort: number;
  images?: string[]; image_names?: string[]; files?: string[];
  /** "worktree" (default) or "docker": a private clone with commands in the project's container. */
  workspace?: "worktree" | "docker";
}
export interface RaceCheck { label: string; cmd: string; ok: boolean; output: string }
export interface RaceAttempt { task_id: string; model: string; effort: number; checks: RaceCheck[]; checked: boolean }
export interface RaceRank { task_id: string; rank: number; score: number | null; reason: string }
export interface RaceJudge { model: string; ranking: RaceRank[]; summary: string; at: string; cost: number; error: string | null }
export type RaceState = "running" | "checking" | "judging" | "judged" | "failed";
export interface Race {
  id: string; prompt: string; project: string; base_branch: string; created_at: string; attempts: RaceAttempt[];
  judge_model: string; judge_effort: number; state: RaceState; judge: RaceJudge | null;
  winner: string | null; merged: string | null; error: string | null;
  /** Put away with its attempt chats; `archived_chats` is what archiving moved, which a restore brings back. */
  archived: boolean; archived_chats: string[];
}
export interface RaceAttemptStats { task_id: string; files: number; add: number; del: number; error: string | null }
export interface RaceDetail { race: Race; stats: RaceAttemptStats[] }
/** The `ol://race` payload: a race as it is now, or the id of one that was forgotten. */
export type RaceEvent = Race | { id: string; removed: true };

export const MIN_RACE_ATTEMPTS = 2;
export const MAX_RACE_ATTEMPTS = 6;

export const raceApi = {
  /** Start N attempts at once. Rejects unless 2–6 attempts and a git project. */
  create: (req: NewRace) => invoke<{ race: Race; tasks: TaskSummary[] }>("race_create", { req }),
  list: () => invoke<Race[]>("race_list"),
  /** The race plus live diff stats per attempt. Slow-ish: on open and refresh only. */
  detail: (id: string) => invoke<RaceDetail>("race_detail", { id }),
  /** Run the checks and the judge in the background. Rejects with "budget:…"
   *  when the monthly cap is reached; `force` is the user's go-ahead past it. */
  judge: (id: string, model: string | null, force = false) => invoke<Race>("race_judge", { id, model, force }),
  /** Mark (or with null clear) the winner. Does not merge. */
  pick: (id: string, taskId: string | null) => invoke<Race>("race_pick", { id, taskId }),
  /** Commit the winner worktree's pending changes and merge its branch into the project's current branch. */
  merge: (id: string) => invoke<{ race: Race; message: string }>("race_merge", { id }),
  /** Archive the race and its attempt chats, or restore exactly the chats archiving moved. */
  archive: (id: string, archived: boolean) => invoke<Race>("race_archive", { id, archived }),
  /** Open a worktree folder in the file manager. Confined to that folder by the backend. */
  openFolder: (cwd: string) => invoke<void>("open_path", { cwd, path: "." }),
  /** Drop the race record; the chats are untouched. */
  forget: (id: string) => invoke<void>("race_forget", { id }),
};
