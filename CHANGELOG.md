# Changelog

Notable changes to OpenLeash, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.3] - 2026-10-18

0.1.3 gives OpenLeash more to work with and more ways to reach it: a real
browser for the agent, Discord, Telegram and Slack chats, service plugins,
best-of-N races, model routers, schedules, and tools for finding what broke.

### Added

#### Reaching your agent

- **Discord, Telegram and Slack.** Message your agent from a channel, thread or
  DM. Each conversation is a chat on your machine, replies come back as the
  agent writes them, and approvals and questions are relayed (`yes`, `always`,
  `no`, or an option's number). `/new`, `/stop`, `/status` and `/help` work
  everywhere (Slack uses `!` instead of `/`). Only the user ids you list can
  reach the bot, groups are off by default, and new chats default to
  **Auto-edit**.
- **Phone approvals with ntfy.** With remote control on, approvals and questions
  can arrive as ntfy notifications. The link carries a one-use grant that
  expires after 15 minutes.
- **Linear, Jira, Slack, Notion and GitLab plugins.** Service plugins the agent
  can call, switched on in Settings → Plugins.

#### Agents

- **Browser.** A headless Chrome or Edge the agent can drive, on by default and
  never using your everyday profile. It reads pages as text or as a compact
  snapshot with stable element refs, and handles forms, tabs, dialogs,
  downloads, uploads, emulation and PDF. A **shared Browser view** lets you
  watch it, and **Annotate** lets you click an element or drag a region and
  leave a comment for the agent.
- **Best-of-N race.** Turn on **Race** in the composer (or type `/race`) to send
  one prompt to 2-6 attempts, each in its own worktree with its own model. Check
  output is collected and a tool-less judge ranks the attempts. You merge only
  the one you pick.
- **Routers.** Settings → Routers picks the model for each request with rules
  (who is asking, context size, repeated failures) ahead of the fallback chain.
  It has presets, a test panel, an activity view and import/export. Usage caps
  and account pools still apply.
- **Schedules.** Send a prompt to a new chat every N minutes or hours, daily or
  weekly. A run is skipped if the last one is still going, missed runs are
  caught up at most once, and a folder without a trust decision is refused.
  Show the screen with Settings → Tabs → Show Schedules.
- **Spend quota before it resets.** Optional backlog of low-priority prompts
  that runs in a worktree on subscription quota about to be forfeited. Off by
  default and never past your cap.
- **Find the breaking edit.** Review → **Find breaking edit…** binary-searches
  the chat's edits with a test command and reports the first bad step, with
  **Jump to message** and **Revert this file**. The agent has the same
  `bisect_edits` tool, under the same permission checks as `bash`.
- **`http_request` tool.** One HTTP request to an API, local dev server or
  webhook. `GET`/`HEAD` to public hosts run without asking; other methods and
  any private-network target ask.
- **Code intelligence (`lsp`).** Definitions, references, hover, symbols,
  diagnostics and call hierarchy from the project's language server
  (rust-analyzer, typescript-language-server, pyright, gopls, clangd).
- **Waiting on more events.** `wait_for_event` can now park on a file, a
  background job (optionally a matching output line) or another chat.
- **Save a chat as a skill.** Right-click a chat → **Save as skill…** drafts a
  `SKILL.md` for you to edit and save globally or to the project. Only you can
  start it.
- **Agent memory.** Global and per-project notes the agent keeps in plain
  markdown and loads into new chats.
- **Ultrathread.** `/ultra` lets the main agent run a tree of nested subagents;
  `/ultrawt` gives each worker its own worktree.

#### Interface

- **Run timeline.** A Gantt view of where a chat spent time and money: model
  requests, retries, tool calls, waits on you, and subagent lanes.
- **Stats.** Settings → Stats shows tokens, cost, time and errors by model, a
  per-chat panel, and a monthly budget gate.
- **Welcome tour** for new installs, reopenable from the command palette.
- **Archive races.** **Archive race** puts a race and its attempt chats away
  (with Undo); Settings → Chats lists them with **Restore**.
- **Race attempt cards** show **Stats** (tokens, cache hit rate, context used,
  cost per changed line, time, model price) and **Open folder**. The sidebar
  group shows progress (`Running 2/3`).
- **Edit artificts yourself.** Press **Edit** (`E`) on an HTML artifict and
  click an element to change its text, colours, font size, weight, padding or
  corner radius. **Save** keeps it as a new version marked `author: "user"`.
  **Edit source** works on any kind, and **Restore** puts an older version back
  on top.
- **Export artificts** as standalone HTML, SVG, Markdown, text, JSON or an
  embedded picture.
- **Project design system.** `.openleash/DESIGN.md` is read like the other
  instruction files (trusted folders only, listed in the trust manifest,
  switchable in Settings).
- **Ask the agent.** One-click requests in the Artificts studio: review the
  design, explore three directions, implement it in the project, and set up or
  check the design system.

### Changed

- **Web search is one tool.** The main agent's `web_search` takes 1-5 queries in
  one call and replaces "Deep search" (`web_search_deep`). Each query runs
  against DuckDuckGo and Brave, results are merged and ranked, and the best
  pages are read. Subagents keep the single-query `web_search`.
- **Tool schema deferral.** `tool_search` holds back MCP tool schemas until the
  agent searches for one; **Also defer plugin tools** and per-server `defer`
  switches are in Settings → MCP servers.
- **Compaction summaries** ask for exact verification commands, scope, exit
  codes and evidence paths, and keep unresolved failures and pending decisions.
- Markdown tables with `Total` and `Implemented` / `Tested` / `Verified` /
  `Accepted` columns show a separate progress bar per stage.

### Security

- Nothing in this release relaxes a permission decision. New tools (`http_request`,
  `bisect_edits`, browser, `lsp`, service plugins) go through deny rules, plan
  mode, read-only subagent limits and the reviewer mode like existing ones.
- **Chat connectors are allowlists.** Nobody can use a bot until you list their
  user id, and connector messages cannot create skills or change settings.
- **The agent's browser is not your browser.** It uses its own profile and
  cannot control your desktop.

## How to read this file

Not every change belongs here. Include: anything a user of the app would
notice, anything that changes a default, anything that touches the permission
layer or the security model, and any behaviour change to instructions, prompts
or compaction. Leave out: refactors with no behaviour change, test-only changes,
dependency bumps that change nothing, and internal renames.

Keep current: add what you change to a new `Unreleased` section in the same pull
request, and at release time rename it to the version and add the date.
