# Configuration

Add API keys in Settings → Models, or set `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `OPENROUTER_API_KEY`, `ZAI_API_KEY` or `OPENCODE_API_KEY` (same key covers both OpenCode Zen and OpenCode Go). Keys, settings and tasks live in `~/.openleash/` (override with `OPENLEASH_HOME`).

Settings → Plugins contains computer use and the headless browser. Each card's **Details** button opens its description and configuration; the switch controls whether agents receive its tools. Screen/browser availability is separate from whether the plugin is enabled. Plugin changes apply to new chats or after `/compact` in existing chats.

Computer use lists the displays it can see in the order the tools index them (the primary first, so `monitor: 0` is always the primary). Only the primary unrotated display takes clicks and keystrokes: absolute input coordinates are normalised over the primary screen, so the card marks any other one as available to look at but not to click, and the agent is refused rather than clicking the right place on the wrong screen. **Settle time** is how long it waits after an action before the screenshot it returns. While an agent is driving your screen a small always-on-top banner shows the batch and a Stop button, and `Esc` — which is caught globally on Windows, so it works while another window has focus — tells it to stop touching your screen; pressing it twice in quick succession ends the run.

GitHub authentication and its tool switch live in **Settings → Connectors**. Existing GitHub settings and tokens are preserved. Other app connectors use the MCP engine and retain its permission checks; connecting an app does not pre-approve its tools. Authentication methods and service prerequisites are shown inside each connector's **Details**. Hosted MCP connectors support browser authorization with PKCE. OAuth tokens are kept in a separate backend-only `mcp-oauth.json` file and are never returned to the settings UI. They are still plaintext local credentials, not encrypted storage; do not share the OpenLeash data folder. Disconnect removes local credentials and disables the server, but does not revoke consent at the provider.

### Linear, Jira, Slack, Notion and GitLab

These five built-in plugins also live in **Settings → Connectors**, below GitHub, each off until you switch it on (changes apply to new chats or after `/compact`). Open a card's **Details** to paste a token, set its URL fields, follow the link to the provider's token page, and **Test connection** (shows who the token authenticates as and where it came from). Tokens are write-only in the UI: they are saved with their own command, shown afterwards only as "Saved", and never sent back to the settings screen. When nothing is saved, the environment is used:

| Plugin | Settings | Environment fallback | Notes |
|---|---|---|---|
| `linear` | `plugins.linear.token` | `LINEAR_API_KEY` | Personal API key from Linear → Settings → Security & access. |
| `jira` | `plugins.jira.base_url`, `.email`, `.token` | `JIRA_BASE_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN` | With an email: Jira Cloud, basic auth with an [API token](https://id.atlassian.com/manage-profile/security/api-tokens). Without one: Server/Data Center personal access token (bearer). |
| `slack` | `plugins.slack.token`, `.user_token` | `SLACK_BOT_TOKEN`, `SLACK_USER_TOKEN` | Bot token (`xoxb-`) from a [Slack app](https://api.slack.com/apps) installed in your workspace, with the scopes for what you want (e.g. `channels:read`, `channels:history`, `groups:*` for private channels, `chat:write`, `reactions:write`, `users:read`, `users:read.email`); invite the bot to channels it should read. Message search needs a user token (`xoxp-`) with `search:read`. |
| `notion` | `plugins.notion.token` | `NOTION_TOKEN` | [Internal integration](https://www.notion.so/profile/integrations) secret; share each page or database with the integration (… → Connections). |
| `gitlab` | `plugins.gitlab.base_url`, `.token` | `GITLAB_URL`, `GITLAB_TOKEN` | Base URL defaults to `https://gitlab.com`; self-hosted instances (including a sub-path) work. Personal access token with `api` (or `read_api` to only read). |

Settings files written before these plugins existed load unchanged, with all five off.

## App connectors

- **Notion, Linear and Atlassian:** use **Sign in** for hosted MCP browser authorization. Existing API-key and legacy Notion token configurations remain available.
- **Slack:** create a registered internal or Marketplace Slack app, enable public PKCE, register `http://127.0.0.1:42817/callback`, and enter its client ID in Details. PKCE enablement is a one-way Slack app setting. Workspace administrator approval may be required; OpenLeash cannot grant it.
- **Google Workspace:** select a product and use a **Desktop app** OAuth client ID from your Google Cloud project. Join Google's Workspace Developer Preview and enable the product API and MCP service. Each product requests explicit read-only scopes rather than every scope advertised by the server. OAuth consent-screen restrictions and domain policy still apply.
- **Microsoft 365:** **Install and connect** configures Microsoft's official `@microsoft/workiq` stdio MCP server. It downloads/runs through `npx`, requires Node.js/npm, and handles Microsoft authentication itself on first use. Tenant consent, licensing and Microsoft's terms remain your responsibility; OpenLeash does not accept agreements on your behalf. Removing this connector does not clear Work IQ's own sign-in cache.

Official setup references: [Slack MCP](https://docs.slack.dev/ai/slack-mcp-server/), [Slack PKCE](https://docs.slack.dev/authentication/using-pkce/), [Google Workspace MCP](https://developers.google.com/workspace/guides/configure-mcp-servers), [Google desktop OAuth](https://developers.google.com/identity/protocols/oauth2/native-app), [Microsoft Work IQ](https://github.com/microsoft/work-iq).

## Chat platforms (Discord, Telegram, Slack)

**Settings → Connectors → Chat with your agent** sets up a Discord bot, a Telegram bot or a Slack app that drives OpenLeash chats; see [Features](features.md#chat-with-your-agent-from-discord) ([Telegram](features.md#chat-with-your-agent-from-telegram), [Slack](features.md#chat-with-your-agent-from-slack)) for setup. Their configuration and tokens are saved in `~/.openleash/connectors.json`, separate from `settings.json`. Tokens are write-only from the UI. The conversation → chat links are in `discord-chats.json`, `telegram-chats.json` and `slack-chats.json`. Delete one of those files, or use that platform's **Start all fresh**, to make every conversation there open a new chat.

## Browser

The Browser plugin is **on by default**, unlike computer use. The first launch of
this build turns it on once for existing settings too; turning it off afterwards
sticks (`browser_default_applied` records that migration). Chromium is started
only when the browser is first used. Chrome or Edge must be installed; an enabled
plugin does not imply a browser is available.

Settings → Plugins → Browser stores these fields under `plugins.browser`:

| Field | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Offer the browser tool and shared Browser view. |
| `width`, `height` | `1280`, `800` | Default viewport in CSS pixels. |
| `persistent_profile` | `false` | **Keep sign-ins between chats** in OpenLeash's separate browser profile, never your normal Chrome/Edge profile. Sites you sign into can then be used as you by the agent. |
| `headed` | `false` | **Show browser window**, rather than running headless. |
| `developer_mode` | `false` | Enable the raw CDP action; every call still asks for approval. |
| `interactions` | `"ask"` | Default for page interactions: `ask`, `allow`, or `block`. Sensitive actions still ask unless explicitly trusted for the site. |
| `download_dir` | `""` | Download folder; blank uses the project's `.openleash/browser-downloads/<task>/` (an OS-temp folder for remote projects). |

**Site exceptions** and **Allowed LAN hosts** edit the same permission rules as
Settings → Permissions, not a second allowlist:

| Rule | Effect |
|---|---|
| `browser:site example.com` | Allow interactions on that site, including sensitive ones. Grant only to sites you trust the agent to use as you. |
| `browser:ask example.com` | Ask on that site even with default Allow or Full Access. |
| `!browser:site example.com` | Block interactions on that site; reads and navigation remain available. |
| `browser:click example.com` | Allow one action on that site, but not sensitive clicks without site trust. |
| `browser:lan 192.168.1.20` | Allow reaching that private-network host. This does not approve page interactions. |

Hosts may be exact or `*.example.com` (covering the apex and its subdomains).
Existing `browser:click`, `browser:type`, and `browser *` rules still cover
ordinary interactions, but not sensitive actions or raw CDP. Typing a LAN URL in
the shared Browser view is your approval for that host for the session. Metadata,
link-local and reserved targets remain blocked regardless of rules. See
[Security](security-model.md#browser-plugin) for network and profile limitations.

## Memory

Settings → General → **Memory** (`memory` in `settings.json`, on by default) controls long-term agent memory. Notes are plain markdown under `~/.openleash/memory/`: `_global/` holds notes about you and applies to every project, and each `<project>-<hash>/` folder holds one project's notes. Edit or delete them freely. A `MEMORY.md` line whose file is gone is ignored. See [Features](features.md#agent-memory).

## Quota burner

Settings → Accounts → **Spend quota before it resets** is stored as `burner` in `settings.json`: `enabled` (default `false`), `reset_within_min` (30), `min_headroom` (20, percentage points), `spend_cap` (90, percent used) and `queue`, the backlog items (`id`, `project`, `prompt`, `model` — empty for any subscription provider — `perm` — empty for your default mode — `created_at`, `error`). A settings file without the key loads with the burner off and an empty queue. The chats it has started, live and finished, are recorded separately in `~/.openleash/burner.json` (the last 50 finished runs are kept). See [Features](features.md#spend-quota-before-it-resets).

On Windows, bash commands run in Git Bash if it's installed, otherwise in PowerShell. Set `OPENLEASH_BASH` to choose a different shell.

Other environment variables the app reads: `OPENLEASH_ANTHROPIC_BASE` and `OPENLEASH_CODEX_BASE` (override the Anthropic / OpenAI base URL, useful behind a proxy), `CODEX_HOME` (where to read existing Codex auth) and `OPENLEASH_GH_CLIENT_ID` (override the GitHub OAuth client id), plus the service-plugin token variables in the table above (used only when the setting is empty). Also read: `OPENLEASH_ZEN_BASE` (OpenCode Zen base URL, default `https://opencode.ai`); `OPENCODE_DATA_DIR` and `XDG_DATA_HOME`, which locate an existing OpenCode login to import read-only; `GH_TOKEN` and `GITHUB_TOKEN`, the GitHub plugin's fallback when its token is empty; `OPENLEASH_DOCKER`, the container engine command (for example `podman`, see [Workspaces](workspaces.md)); and `OPENLEASH_SSH`, the `ssh` binary used for SSH projects (see [Remote](remote.md)). `OPENLEASH_KEEPALIVE_MS`, `OPENLEASH_GH_POLL_MS` and `OPENLEASH_WAKE_POLL_MS` override how often the cache keep-alive, GitHub and wake-up checks run. They are read in normal builds too; leave them unset unless you are debugging. There is no env var for Ollama — its base URL is set in Settings.

## Settings pages

Settings has eighteen pages, in this order. The keys are in `settings.json` unless another file is named. Defaults are shown in parentheses.

| Page | Main options |
|---|---|
| **General** | Interface zoom `ui_zoom` (110); close to tray `close_to_tray` (on); desktop notifications `notify` (on); send key `send_with` (Enter); `phone_enter_sends` (off); show thinking `show_thinking` (off); context breakdown `ctx_breakdown` (off); where new tasks run `workspace` (local); assist mode `assist` (default); **Memory** `memory` (on); automatic resume `automatic_resume` (on); project docs `project_docs`; `inject_global_claude` (off, reads `~/.claude/CLAUDE.md`); trace export endpoint `trace_endpoint` (empty). |
| **Theme** | `theme` (`raycast`), `mode` (`dark`). |
| **Models** | Provider keys and key pools (`providers`), custom providers and models, per-model settings (`model_configs`), disabled, favourite and recent models. |
| **Accounts** | Subscription and API accounts with priority, enable switch and usage caps (`accounts`); the spend-quota **burner** (`burner`: off, reset within 30 minutes, headroom 20 points, spend cap 90%); subagent defaults (`agents`, `default_agents`: `explore`, `general`). See [Features](features.md#spend-quota-before-it-resets). |
| **Routers** | Routes with heads, fallback steps and `on_exhausted` (`routes`). See [Features](features.md#routers). |
| **Subagents** | Subagent definitions, their effort and instruction injection (`agents`). |
| **Skills** | Disabled skills (`disabled_skills`). |
| **Commands** | Prompt templates for `/name` (`custom_commands`). |
| **Chats** | Agent-generated titles `agent_titles` (on); **Other chats** `cross_chat` (on, see [Features](features.md#agents-seeing-and-messaging-your-other-chats)); `automatic_chat_reorder` (on). |
| **Tabs** | `all_projects` (off); the shortcut button's target `sidebar_button_tab` (`accounts`); Needs you `needs_you` (off); Pull requests `show_pull_requests` (off); Schedules `show_schedules` (off); Saved sort `saved_sort`. |
| **Workspaces** | Default workspace (`workspace`); docker idle timeout `docker_idle_minutes` (15); `docker_auto_rebuild` (on). See [Workspaces](workspaces.md). |
| **Permissions** | Permission mode `perm`; allow and deny rules `allow` (a `!` prefix denies); `deny_env_files` (on). Stored modes are `disabled`, `allowlist`, `reviewer`, `auto` and `turbo`, where `turbo` is Full Access. New installs start at Full Access. Seeded allow rules are `git status*`, `git diff*` and `git log*`. |
| **Plugins** | Computer use `plugins.computer` (off; `settle_ms` 600); Browser `plugins.browser` (on, see [Browser](#browser)); custom MCP plugins. |
| **Connectors** | GitHub and the five service plugins (`plugins.github`, `plugins.linear`, `plugins.jira`, `plugins.slack`, `plugins.notion`, `plugins.gitlab`), each off by default, and the chat platforms (Discord, Telegram, Slack), which are stored in `connectors.json`. |
| **Checks & hooks** | Diagnostics `diagnostics` (on); verify nudge `verify_nudge` (on); checkpoints `checkpoints` (on); `hooks` and `trusted_hooks`. See [Security](security-model.md). |
| **MCP servers** | Servers `mcp` (each with `enabled`, `auto_approve`, `oauth` and `defer`); `tool_search` (on); `defer_plugins` (off). |
| **Remote** | Stored in `remote.json`, not `settings.json`: remote control, reachability, port (17420), paired devices and the ntfy settings. See [Remote](remote.md). |
| **Stats** | Monthly budget `budget` (50, in dollars; 0 disables it); `keep_chat_stats` (on); `cache_keepalive` (off); `cache_keepalive_pings` (4). See [Features](features.md#stats). |

Two switches sit outside Settings. The **Review** page has `git_attribution` (what a commit says about the agent) and `git_commit_verify` (off, whether hooks run on agent commits). The sidebar usage bar toggles `home_usage`.

Unknown keys in `settings.json` are preserved when the file is saved. At startup, unrecognised keys are printed to the console log, and nothing else warns about them, so check the spelling if a hand-made change has no effect.

## Data files

Everything the app stores lives under `~/.openleash/`, or under `OPENLEASH_HOME` if that is set. Files are plain JSON or markdown unless noted.

| Path | Contents |
|---|---|
| `settings.json` | Settings above. Contains API keys and tokens in plaintext. |
| `tasks/<id>.json` | One chat: transcript, todos, state and timing records. |
| `connectors.json` | Chat-platform configuration and tokens. |
| `discord-chats.json`, `telegram-chats.json`, `slack-chats.json` | Conversation → chat links. Delete one, or use that platform's **Start all fresh**, to open a new chat for every conversation. |
| `remote.json` | Remote control state: paired devices (hashed secrets only), SSH hosts and the ntfy settings. |
| `mcp-oauth.json` | MCP OAuth tokens, backend-only, plaintext. |
| `burner.json` | Spend-quota runs (the last 50 are kept). |
| `schedules.json` | Scheduled runs. A corrupt file is moved aside with a `.bad` suffix. |
| `races.json` | Best-of-N races. |
| `stats.json` | Usage and spend ledger for the Stats page. |
| `memory/_global/`, `memory/<project>-<fingerprint>/` | Agent memory notes and their `MEMORY.md` indexes. |
| `OPENLEASH.md` | Personal instructions loaded into every chat. |
| `skills/<name>/SKILL.md` | Global skills. |
| `worktrees/` | Git worktrees the app created for chats. |
| `workspaces/<task>` | Docker workspace copies. |
| `checkpoints/` | Shadow checkpoint stores for file snapshots and rewind. |
| `proof/<chat>/` | Proof clips and visual-diff screenshots, deleted with the chat. |
| `browser-profile/`, `browser-downloads-staging/` | The browser's profile (when **Keep sign-ins** is on) and download staging. |
| `imports/` (and `failed/`) | Chat files waiting to be imported, and those that failed. |
| `spill/` | Long command output saved to disk. |

Outside the data folder, the app keeps a toast icon cache in the OS cache directory (`openleash/toast-icon.png`).

The app reads credentials that other tools already store, read-only: `~/.codex/auth.json` (or `$CODEX_HOME`) for ChatGPT, `~/.claude/.credentials.json` for Claude subscriptions, and the OpenCode `auth.json` under `$OPENCODE_DATA_DIR` or `$XDG_DATA_HOME`. OpenLeash never writes to these files.

## Project files

These live in the project folder. Instruction files and project skills load only once the folder is trusted.

| Path | Role |
|---|---|
| `OPENLEASH.md`, `AGENTS.md`, `CLAUDE.md` | Project instructions, injected into the system prompt. Switch each off under **General → Project docs**. |
| `.openleash/instructions.md` | Project instructions, like the above. |
| `.openleash/DESIGN.md` | The project's design system, read for visual artifacts. |
| `.openleash/hooks.json` | Hook definitions. They run only after review by hash. See [Security](security-model.md). |
| `.openleash/skills/`, `.claude/skills/` | Project skills. |
| `.openleash/agents/`, `.claude/agents/` | Project subagent definitions. |
| `.claude/settings.json` | Read for compatibility with Claude Code projects. |
| `.openleash/artificts/` | Saved artifacts, shared between chats for the project. See [Artifict collaboration](artifict-collaboration.md). |
| `.openleash/browser-downloads/` | Browser downloads when no download folder is set. |
| `.openleash_docker/` | Docker workspace definition (`Dockerfile`, and `openleash.json` inside it). See [Workspaces](workspaces.md). |
