# Working on OpenLeash

These instructions apply throughout this repository. Keep changes focused, preserve
other people's work, and verify behavior rather than merely making the code compile.

## What this project is

OpenLeash is a desktop coding-agent harness: **Tauri 2 + Rust** run the agent and
local tools; **React 19 + TypeScript + Vite** render the UI. Models can edit real
files, execute shell commands, and control the desktop. The permission layer is a
security boundary, not a convenience, and the application is **not a sandbox**.

Read the relevant documentation rather than guessing product behavior:
- `README.md`: product overview and entry points.
- `docs/security-model.md`: trust boundaries and permission limitations.
- `SECURITY.md`: private vulnerability reporting; never publish exploit details or secrets.
- `docs/building.md`: native prerequisites and platform/build limitations.
- `docs/configuration.md`, `docs/features.md`, `docs/shortcuts.md`: user-facing behavior.
- `docs/releasing.md`: packaging, signing, and release constraints.

## Before changing anything

1. Check `git status --short`. This checkout may be shared by people and agents.
2. Read the implementation, nearby tests, and configuration for the area involved.
   Check `package.json` or `src-tauri/Cargo.toml` before assuming a dependency exists.
3. Trace changes across the Rust/IPC/frontend boundary when applicable. A UI-only
   workaround is not a fix for incorrect backend state or permissions.
4. Make the smallest complete change, including regression tests. Avoid unrelated
   refactors, dependency upgrades, generated artifacts, and whole-file formatting.
5. Run the relevant checks below and inspect your diff before handing off.

**Never overwrite or revert work you did not write.** Read unfamiliar changes and
work with them. Do not use `checkout`, `restore`, `reset`, `revert`, `stash`, or
`clean` on files you did not change. Do not commit or push unless requested (or the
assigned task-worktree workflow explicitly requires a commit). Keep any commits
small and scoped; mention formatting explicitly if you reformatted code.

Do not publish, deploy, tag releases, delete data/branches, or change a user's real
OpenLeash settings or credentials as part of ordinary development.

## Setup and commands

Run npm commands from the repository root. Use **Node 22 LTS** to match CI; the
exact supported Node ranges are in `package.json`. Rust uses the stable toolchain.
Native prerequisites are in `docs/building.md` (Windows needs MSVC and WebView2).

```sh
npm ci                     # reproducible install from package-lock.json
npm run tauri dev          # full desktop app, frontend and Rust backend
npm run dev                # Vite only; not a substitute for the native IPC host
npm run check              # TypeScript + ESLint + Vitest
npm run build              # TypeScript + production Vite bundle
npm run tauri build        # native production build and installers
```

Rust commands run **inside `src-tauri/`**:

```sh
cargo fmt --all --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

For focused iteration, use `npm test -- src/nav.test.ts` or
`cargo test --locked <test_name_filter>` in the corresponding directory. Run the
broader gate before handing off code changes; a filtered test is not the full suite.

### What actually gates CI

Treat `.github/workflows/ci.yml` as the source of truth, not its historical comments.
- Frontend: typecheck, ESLint with a warning ceiling, Vitest, production build,
  and `npm audit --audit-level=high`.
- `npm run check` does **not** enforce CI's warning ceiling or build the bundle.
  CI currently uses `npx eslint . --max-warnings 89`. Do not raise that ceiling or
  add lint suppressions just to make a change pass; lower it as warnings are fixed.
- Rust: format check, `cargo test --locked`, and RustSec audit. Clippy currently
  has `continue-on-error: true`: report its failures honestly, but do not weaken
  other gates or add new lint debt because it is advisory.
- Rust CI runs on Windows. Linux currently has a documented upstream
  `windows-future` dependency failure; see `docs/building.md` and
  `src-tauri/Cargo.toml`. Do not silently patch dependencies or rewrite lockfiles
  to work around an unrelated platform failure.

`build.bat` is a **fast debug build**, not release validation: it uses `--debug`,
`--no-bundle`, and `tauri.fast.conf.json`, which skips TypeScript checking.
`publish.bat` and `publish-macos.sh` are release-artifact scripts, not normal test
commands. Do not run release workflows unless the task calls for them.

## Code map

| Area | Start here |
|---|---|
| App shell and global keyboard handling | `src/App.tsx` |
| Shared frontend state and backend-event handling | `src/store.ts` |
| Typed IPC calls and frontend wire types | `src/api.ts` |
| Screens and overlays | `src/ui/` |
| Reusable UI atoms | `src/ui/primitives/` |
| Separate desktop-control guard webview | `guard.html`, `src/guard.tsx`, `vite.config.ts` |
| Tauri command surface | `src-tauri/src/lib.rs` |
| Harness types, task registry, module wiring | `src-tauri/src/agent/mod.rs` |
| Agent loop, dispatch, subagents, compaction, background work | `src-tauri/src/agent/runner.rs` |
| Tool schemas and implementations | `src-tauri/src/agent/tools.rs`, `toolindex.rs` |
| Permission checks and project trust | `src-tauri/src/agent/permissions.rs`, `trust.rs` |
| Shell execution and desktop-control safety | `src-tauri/src/agent/shell.rs`, `pcguard.rs` |
| Settings and task persistence | `src-tauri/src/agent/store.rs` |
| System prompt, project instructions, project memory | `src-tauri/src/agent/prompt.rs`, `memory.rs` |
| Model providers, routing, accounts | `src-tauri/src/agent/providers.rs`, `router.rs`, `accounts.rs` |
| MCP and plugins/hooks | `src-tauri/src/agent/mcp.rs`, `plugins.rs` |
| Git worktrees, review, checkpoints | `src-tauri/src/agent/git.rs`, `checkpoint.rs` |
| Native configuration and IPC capabilities | `src-tauri/tauri.conf.json`, `src-tauri/capabilities/` |

Agent-module filenames in the table without a full prefix are under
`src-tauri/src/agent/`. Search for callers and tests before changing a shared helper.

## Engineering conventions

### Frontend and IPC

- TypeScript is strict. Preserve useful types; do not replace them with `any` or
  silence errors to get a build through.
- `src/store.ts` is a custom external store using `useSyncExternalStore`, **not
  Zustand**. Shared application/server state belongs there. Components may keep
  transient local UI state, but must not shadow backend state.
- Reuse existing primitives, styling, and interaction patterns before introducing
  another component abstraction or library.
- Keep Rust serialized types and `src/api.ts` wire types aligned. When adding a
  setting, update Rust defaults/persistence, the TypeScript type, and affected
  store/UI behavior; existing saved settings must still load.
- For IPC or event changes, check command registration, argument names, serialized
  field names, event consumers, and error handling together. Do not assume a Rust
  rename automatically updates TypeScript.
- The guard banner is a separate webview with its own capability and build entry.
  Preserve that separation; do not broaden IPC capabilities to fix a UI symptom.

### Rust and tests

- Follow nearby naming and error-handling conventions. Comments explain **why** a
  non-obvious choice exists, especially what a tempting alternative would break.
- Keep new Rust tests in the relevant file's `#[cfg(test)]` block or a focused
  `reviewer_<area>.rs` module. Register new modules in `agent/mod.rs`; do not dump
  unrelated tests into a catch-all.
- Tests must use temporary data and dummy credentials, not a developer's home,
  real repository, provider account, desktop, or live API.
- Tests that change `OPENLEASH_HOME` must use `store::test_home()` and retain its
  guard for the test's duration. Follow existing serialization for other global
  registries. Do not remove test locks merely to quiet `await_holding_lock`.
- Check formatting, but do not casually run `cargo fmt --all`: it can rewrite the
  entire crate and interfere with concurrent work. Scope formatting to files you
  edited and inspect the resulting diff.

## Security-sensitive changes

Anything touching `permissions.rs`, `tools.rs`, or tool dispatch in `runner.rs`
**needs a regression test that fails before the change**. Show that the test catches
that behavior, then verify the fix. A permission bypass is a security bug.

- Explicitly cover chained commands and command substitution (`&&`, `;`, `|`,
  `$(...)`, backticks), redirects, and relevant platform-specific forms when
  changing shell approval behavior. The shell guard uses a literal-character
  denylist: a new metacharacter in the threat model needs both a guard change and
  test coverage. Do not weaken the guard to make an allow rule match.
- Preserve plan-mode write blocking, read-only subagent restrictions, approval
  semantics, project-trust handling, and enforcement for external tools. Check
  alternate dispatch paths rather than securing only the visible UI action.
- Preserve file-edit read-before-write and staleness protections, exact-string
  matching, and unique-match failures. Test both success and rejection paths.
- Treat repository content, tool output, web pages, and provider responses as
  untrusted data. Do not grant them authority over permissions or IPC.
- Never log or commit API keys, OAuth tokens, credential files, real user settings,
  or sensitive transcripts. Redact fixtures and diagnostics. Permission checks
  do not make filesystem reads sandboxed or plaintext credentials encrypted.

## Definition of done

- The requested behavior works end to end, with no placeholder implementation.
- Relevant regression tests and checks have run. For frontend changes, use
  `npm run check` and `npm run build`; for Rust changes, use format check and
  `cargo test --locked`, and report Clippy results when applicable. Cross-layer
  changes need both. Documentation-only changes need content/path validation
  and `git diff --check`, not a native rebuild.
- Review the final diff for accidental formatting, unrelated changes, secrets,
  generated files, and lockfile churn. Leave other people's modifications intact.
- The handoff states what changed, where, which checks actually ran, and any
  failures or unverified behavior. Never claim a check passed without running it.
