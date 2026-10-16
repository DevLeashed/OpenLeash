# Changelog

Notable changes to OpenLeash, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.2] - 2026-10-16

0.1.2 is the release that lets OpenLeash leave the window it runs in: drive it
from a phone, run agents on another machine, work on a project over SSH or inside
WSL, or run a task in a container. It also adds a headless mode and an editor
(ACP) server.

> **Read before updating:** the in-app updater no longer verifies a signature.
> See [Security](#security). The `.openleash/artifacts/` folder is now
> `.openleash/artificts/` and existing artifacts are not carried over. See
> [Changed](#changed).

### Added

#### Remote connectivity

- **Remote control** (Settings → Remote). OpenLeash serves its full interface to
  paired browsers, with no reduced mobile mode: chats, approvals, questions, plan
  review and settings all work. Pair with a QR code or a five-minute, single-use
  six-digit code. Each device has its own session that you can revoke, there is a
  *Sign out all devices* button, and the titlebar shows a chip whenever a device
  is connected. It is off by default and listens on loopback unless you choose
  *Network*. Details in `docs/remote.md`.
- **Other machines over SSH.** Add a machine by SSH alias or `user@host` and
  press *Connect*. OpenLeash forwards a port and opens that machine's OpenLeash
  in your browser, already paired. Agents there keep running if your laptop
  sleeps.
- **Send a chat to another machine** (right-click a chat → *Send to another
  machine…*). *Copy* or *Move* a conversation to one of your saved SSH machines,
  optionally continuing it there. Only the conversation travels, not the code,
  and the dialog warns about uncommitted or unpushed work. The receiving machine
  rebuilds its own prompt, tools and trust, and caps the permission level at its
  own default.
- **Open over SSH** (project menu). Open a folder on another machine as a project,
  VS Code Remote-SSH style. The agent runs here and its file tools, shell and git
  run on the host, over your own `ssh` client and SFTP. Needs a POSIX shell and
  SFTP on the host, not OpenLeash. Writes are checked against the host's
  `realpath`, so a symlink that points out of the project counts as outside it.
  Checkpoints, worktree and docker workspaces, artificts and project-level
  skills/agents/hooks/MCP servers are not available for SSH projects yet, and each
  refuses clearly instead of falling back to a local folder.
- **Open from WSL** (Windows). Pick a distro and a folder, or type a
  `\\wsl.localhost\…` path. Shell commands, background jobs, hooks and git run
  inside the distro with `wsl.exe --exec`, and the agent is told it is on Linux.

#### Projects and workspaces

- **Docker workspaces.** A task can work in a private copy of the project, with
  its commands run in a container built from `.openleash_docker/`. The harness
  builds the image (and rebuilds it when that folder changes), starts the
  container when you message the chat, stops it when idle and removes it with the
  chat. The chat header has start/stop and *Push branch*. Only offered for
  trusted folders. Podman works via `OPENLEASH_DOCKER=podman`. Details in
  `docs/workspaces.md`.
- **Settings → Workspaces** lists every worktree and container OpenLeash created,
  finds leftovers whose chat is gone, and removes them.
- **Clone repository…** in the project menu. Accepts GitHub links, `https://` /
  `ssh://` URLs and `git@host:owner/repo`, clones into `~/openleash/<repo>`, and
  opens an existing checkout of the same repository instead of cloning again.
- **Import another tool's rules and subagents**, read-only scan first and applied
  only when you accept. Never writes over your instruction file, is idempotent,
  and only runs for trusted folders.
- **Path-scoped rules.** Instruction files in `.openleash/rules/` or
  `.claude/rules/` can declare the paths they apply to, so a rule for
  `src/**/*.ts` is not injected into work on anything else.

#### Agents

- **Headless mode.** The new `openleash-headless` binary runs one task to
  completion with no window, writing newline-delimited JSON to stdout (session
  event, per-step events, then a result with provider-reported token usage and
  cost). Supports `--resume <id>` and a documented exit-code contract.
- **ACP server.** The new `openleash-acp` binary speaks the Agent Client Protocol
  over stdio, so ACP-capable editors (Zed, JetBrains, VS Code, Neovim, …) can
  drive the same harness. Sessions are ordinary chats and show up in the desktop
  app. Permission requests go to the client, and a declined tool does not run.
- **Other chats** (Settings → Chats, off by default). A chat's main agent gets
  `list_chats` and `message_chat` to see what your other chats are doing and send
  them a note. See [Security](#security).
- **Reviewer permission mode.** Behaves like *Allowlist only*, except that a
  second, cheaper model answers the approval prompt. It fails closed, so an
  unreachable or unclear reviewer asks you.
- **Usage caps.** Double-click a usage line in Settings → Accounts, or a
  provider's aggregated line, to cap it. The harness then stops using that account
  once the window reaches the cap. A cap can be permanent or lift after 1, 4 or 24
  hours. Chats fall through to other accounts, and a chat with nowhere to go
  pauses and resumes when the cap lifts.
- **Resume choices for a stopped chat:** *Continue*, *Summarise and continue*, or
  *Wrap up*.
- **Opt-in trace export.** Setting a trace endpoint sends one OTLP/HTTP span per
  model request and per tool call, using OpenTelemetry GenAI conventions. Off
  until you set a destination, and it can never delay or fail a run.
- **Computer use** shows multi-display layouts. Only the primary display takes
  input, and the agent is refused rather than clicking the right place on the
  wrong screen. It can also zoom into a region and hold keys or buttons across
  actions.
- **Plans are saved as artificts.** `exit_plan_mode` files the plan as a Markdown
  artifict before asking you to approve it. Refining it appends a revision, and
  the approval card has *Open in Artificts*.

#### Interface

- **Context attribution.** The context meter can show what is filling the window
  (system prompt, project instructions, skills, tool schemas, attachments,
  conversation, compaction summary). It is off until you enable it at Settings →
  General → *Context breakdown*.
- **Artificts workspace.** Comment mode lets you click a point in a preview to pin
  a comment or select text to quote it. An *Ask for…* menu stages a request, and
  **Send** is the one action that shares it with the agent.
- **Keyboard shortcut reference** in the command palette, and *Send message with*
  (Enter or Ctrl Enter) under Settings → General.
- **Light / dark / system theme** switch under Settings → Theme.
- **Undo for archiving.** The toast carries the way back, and the sidebar's
  automatic trim says what it did.
- **Sidebar shortcut button** (beside the usage bar) can point at any settings
  page, not only Accounts.
- **Saved prompts follow the current project**, with a sort control when *show all
  chats* is on.
- **Unknown settings are kept and reported.** A key in `settings.json` that this
  build doesn't recognise is preserved on the next write and named in a warning,
  so a typo no longer looks like a setting that did nothing.
- Tool-schema deferral (`tool_search`, plugin tools and per-server overrides) is
  configurable under Settings → MCP servers.

#### Models and providers

- Model metadata (context length, output limit) is read from provider list
  responses when present, and per-model details are fetched where a provider only
  lists IDs.
- DeepSeek-style thinking models get their reasoning replayed on each turn, which
  fixes the hard 400 they return on tool-using conversations.
- A gateway reporting that its upstream endpoint is down is retried a few times
  rather than cooling the account and pausing the chain.
- Free-named models may use an account that is benched only for paid-usage limits.
- `.env` read denial is a switch under Settings → Permissions.

#### Build and release

- `build-linux-arm64.sh` builds natively on an aarch64 Ubuntu machine.
- Download builds are staged to release names by `scripts/stage-downloads.py`,
  which refuses a partial set.

### Changed

- **"Artifacts" is now "Artificts"**, including the tools, the UI and the project
  folder, which moved from `.openleash/artifacts/` to `.openleash/artificts/`.
  **Existing artifacts in the old folder are not read or migrated.** Move them
  across by hand if you want them. Artificts can now also be text and image
  deliverables.
- The shared Browser view now lives inside the Artificts workspace, not a separate
  dialog.
- The command palette reaches every settings page and the open chat's lifecycle
  (pin, rename, duplicate, archive).
- Instruction files over the size cap are marked with the cap and the number of
  characters dropped, instead of being silently cut.
- Bulk *delete archived chats* uses one backend call and reports what it actually
  removed.
- Confirmation dialogs and the API-key prompt use the app's own dialogs, not
  `window.confirm` / `window.prompt`.
- A chat that fails to open says why and offers a retry, rather than showing the
  same "Task not found" as a chat that doesn't exist.
- The streaming reply is announced to screen readers when it finishes.
- macOS is distributed as a DMG, with no in-app install. The release workflow no
  longer assembles updater assets.

### Fixed

- A chat's context breakdown no longer keeps pre-compaction categories after the
  conversation is compacted, forked or rewound.
- Removing a worktree goes through the repository that owns it, and a clean
  worktree is no longer confused with a failed commit.

### Removed

- **Archive by age**: the *archive chats you have not used in* window under
  Settings → Chats and the *Archive idle chats now* sweep. Archiving by hand and
  the sidebar's own trim past 100 chats are unchanged.
- Updater signature verification and the updater signing workflow
  (`scripts/assemble-updater.py`, `scripts/updater_assets.py`,
  `tauri.updater.conf.json`). See [Security](#security).

### Security

- **In-app updates are not signature-verified.** Released packages are unsigned.
  The only protection is TLS plus the pinned `DevLeashed/OpenLeash` release path
  that `src-tauri/src/updates.rs` enforces before any bytes are fetched. Whatever
  can serve the update manifest also decides what the app will run. Installing and
  restarting is always an explicit action and is refused while agent or background
  work is active. Portable Windows, macOS and non-AppImage Linux builds only
  announce releases. They are not Authenticode-signed or notarized, so
  SmartScreen and Gatekeeper warnings still apply.
- **A paired device is you.** Remote control has full parity with the desktop
  window: a paired browser can approve tool calls, change settings and read files.
  Treat a pairing link or code like a password and revoke devices you no longer
  use. Every request needs a session cookie even on loopback, WebSocket upgrades
  check `Origin`, and only hashes of session secrets are stored. *Network* mode is
  plain HTTP, so prefer loopback plus an SSH tunnel, or a private overlay such as
  Tailscale.
- **SSH and WSL are not sandboxes.** The permission layer applies as it does
  locally, but reads are unconfined and an approved command can do anything your
  account on that host can. OpenLeash uses your own `ssh` with `BatchMode=yes` and
  never handles keys, passwords or host-key decisions. Trust is per host and per
  path, and a trusted remote project contributes instruction files only.
  `ssh://` strings are never passed to local file or process APIs.
- **A docker workspace is isolation for the project, not a sandbox for the
  agent.** File tools still run on the host. OpenLeash never passes
  `--privileged`, extra capabilities, the Docker socket or host networking, and
  `openleash.json` cannot add them. Published ports bind `127.0.0.1` only.
- **A transferred chat is untrusted data.** The import rebuilds or drops anything
  the app would act on, including the system prompt, tool list, read-before-write
  timestamps and revert contents, and caps the permission level. Trust and
  approvals never travel in a bundle.
- **Other chats is off by default.** A message arrives as a note from another
  agent, never as your words. It cannot approve, answer or unpause anything, and
  paused or archived chats refuse. The receiving chat still obeys its own
  permissions, plan mode and deny rules, but an agent allowed to edit files can be
  asked by another agent to edit them. `message_chat` goes through the permission
  gate.
- **The GitHub connector's token only goes to GitHub.** A raw `api` call to a
  path off `api.github.com` is treated as a write and asks first.
- The reviewer mode only ever replaces the click. Deny rules, the read-only
  shortcut, allow rules, plan mode and the refusal of chained commands all decide
  first, and a reviewer's approval is one-shot and never writes a rule.

## How to read this file

Not every change belongs here. Include: anything a user of the app would
notice, anything that changes a default, anything that touches the permission
layer or the security model, and any behaviour change to instructions, prompts
or compaction. Leave out: refactors with no behaviour change, test-only changes,
dependency bumps that change nothing, and internal renames.

Keep current: add what you change to a new `Unreleased` section in the same pull
request, and at release time rename it to the version and add the date.
