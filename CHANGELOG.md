# Changelog

Notable changes to OpenLeash, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **About the older entries:** this file was added late, and the project kept no
> changelog before it, so earlier releases are deliberately *not* reconstructed
> here — inventing dates and groupings for work whose intent is only visible in
> the commits would be worse than saying nothing. See `docs/features.md` for the
> feature set as it stands, and `docs/releasing.md` for how a release is cut.
>
> **Keeping it current:** add what you changed to `Unreleased` in the same pull
> request. At release time, rename that section to the version number, add the
> date, and start a fresh `Unreleased`. Group by Added / Changed / Fixed /
> Removed / Security, and write entries for what a *user* would notice — not a
> file-by-file summary of the diff.

## [Unreleased]

### Added

- **Headless mode** — a new `openleash-headless` binary drives one task to
  completion with no window, writing newline-delimited JSON to stdout (a session
  event carrying the id, then per-step events, then a terminal result with the
  provider's own token usage and cost), with `--resume <id>` and a documented
  exit-code contract. Makes the harness scriptable and usable in CI.
- **ACP server** — a new `openleash-acp` binary speaks the Agent Client Protocol
  over stdio, so editors that support ACP (Zed, JetBrains, VS Code, Neovim,
  Emacs and others) can drive the same harness. Sessions are ordinary chats, so
  they appear in the desktop app and can be resumed there. Permission requests
  are forwarded to the client and a declined tool does **not** run.
- **Path-scoped rules** — instruction files can declare the paths they apply to,
  so a rule for `src/**/*.ts` is not injected into work on anything else.
- **Reviewer permission mode** — `reviewer` behaves as *Allowlist only* except
  that a second, cheaper model answers the approval prompt in your place. It
  fails closed: an unreachable or unclear reviewer asks you instead of assuming.
  Deny rules, plan mode and chaining refusal all still decide before it is
  consulted, and a reviewer's approval is one-shot — it never writes a rule.
- **Context attribution** — the context meter now shows *what* is filling the
  window (system prompt, project instructions, skills, tool schemas,
  attachments, conversation, compaction summary), measured from the request and
  scaled to the provider's own prompt count.
- **Unknown settings are preserved and reported** — a key in `settings.json`
  that this build does not recognise is kept rather than silently dropped when
  the file is next written, and is named in a warning so a typo does not look
  like a setting that did nothing.
- **Undo for archiving**, and the sidebar's automatic sweep now says what it
  did. The toast can carry one action, and archiving offers the way back.
- **Keyboard shortcut reference** in the app (command palette → *Keyboard
  shortcuts*).
- **Light/dark/system** is now a real setting under Settings → Theme. The
  stylesheet and the setting both existed; only the switch was missing.

### Changed

- **The command palette reaches every settings page** and the open chat's
  lifecycle (pin, rename, duplicate, archive). Previously six of seventeen
  settings pages were click-only and no chat action was reachable from the
  keyboard.
- **Instruction files that are truncated are now marked** with the cap and the
  number of characters dropped. A file over the limit used to be sent as a
  partial rule set with nothing to say so.
- **Bulk "delete archived chats" uses one backend call** instead of a delete per
  chat, so a stale list cannot leave a chat behind or remove one that was just
  unarchived. It now reports what it actually removed.
- **Confirmation dialogs and the API-key prompt use the app's own dialogs** in
  place of `window.confirm` / `window.prompt`, which paint browser chrome inside
  a native window.
- **A chat that fails to open says why** — with a retry — instead of showing the
  same "Task not found" as a chat that does not exist. A failed subagent/skills
  listing says so too, rather than looking like "none configured".
- **The streaming reply is announced to screen readers** once it finishes, from
  a polite live region, rather than not at all.
- Tool-schema deferral (`tool_search`, plugin tools, and per-server overrides)
  is now configurable under Settings → MCP servers.
- `.env` read denial is a switch under Settings → Permissions rather than a
  paragraph describing a setting you could not change.
- **The context breakdown is off until you ask for it**, at Settings → General →
  *Context breakdown*. The meter always shows the measured ring and its
  used/remain pair; the per-category split is an estimate rescaled to the
  provider's own prompt count, so it is no longer drawn by default.

### Fixed

- A chat's context breakdown no longer keeps showing pre-compaction categories
  after the conversation is compacted, forked or rewound.

### Removed

- **Archive by age** — the *archive chats you have not used in* window under
  Settings → Chats, and the *Archive idle chats now* sweep that used it. Nothing
  archived on a timer, so no chat moves because of this: archiving by hand
  (sidebar, right-click, command palette) and the sidebar's own trim past 100
  chats are unchanged.

### Security

- Nothing in this release relaxes a permission decision. The reviewer mode only
  ever *replaces the click*: the deny tier, the read-only shortcut, allow rules,
  plan mode and the refusal of chained commands all still decide first, and a
  reviewer that cannot answer falls back to asking you.

## How to read this file

Not every change belongs here. Include: anything a user of the app would
notice, anything that changes a default, anything that touches the permission
layer or the security model, and any behaviour change to instructions, prompts
or compaction. Leave out: refactors with no behaviour change, test-only changes,
dependency bumps that change nothing, and internal renames.
