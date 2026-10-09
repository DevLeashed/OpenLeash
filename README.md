<table>
  <tr>
    <td align="center" bgcolor="#ff6363"><strong>⚠️ BETA — OpenLeash is still in beta. Some features may be missing, and the app may be unstable.</strong></td>
  </tr>
</table>

<div align="center">

<img src="public/logo.svg" alt="OpenLeash logo" width="96" />

# OpenLeash

**A desktop harness for coding agents.**

Runs language models on your machine with file, shell and web tools, parallel tasks, and permission gates.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE.md)
![Tauri 2](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)
![Rust](https://img.shields.io/badge/core-Rust-DEA584?logo=rust&logoColor=white)
![React 19](https://img.shields.io/badge/UI-React%2019-61DAFB?logo=react&logoColor=black)

</div>

## What is OpenLeash?

OpenLeash is a desktop app that runs a language model against a project on your machine. The model can read files, make edits, run shell commands, search the web, and delegate to sub-agents. Output streams live, and anything that is not read-only asks for approval first.

The core is written in Rust (Tauri 2) and the interface in React 19. You bring your own API key; supported providers are listed under [Highlights](#highlights).

The agent executes model-authored commands, so the permission layer is security-critical. It is the most heavily tested part of the codebase. See the [security model](docs/security-model.md) for what it does and does not protect against.

## Highlights

- **Tools.** `read_file`, `edit_file`, `write_file`, `glob`, `grep`, `bash`, `todo_write`, `web_fetch`, sub-agents and more. Each tool's description states when to use it and when not to.
- **Edits.** Exact-string, unique-match edits. They fail if the agent has not read the file or if the file changed since.
- **Permission gates.** Read-only commands run without asking. Everything else asks. Allow rules never match chained commands (`&&`, `;`, `|`, `$(...)`, backticks, redirects). Plan mode blocks edits until you approve.
- **Parallel tasks.** Each task runs in its own **git worktree**, so concurrent agents do not collide. Review and commit per branch.
- **Sub-agents.** Large searches run in a fresh, read-only context to keep the main one clean.
- **Providers.** Anthropic natively (adaptive thinking, effort), plus OpenAI, Gemini, OpenRouter, Z.ai, OpenCode, Ollama, DeepSeek, Groq, xAI, Mistral, LM Studio, vLLM and custom endpoints.
- **Controls.** A live todo list, mid-turn steering, `Esc` to pause or kill the whole process tree, and a budget cap.
- **Goal mode.** `/goal <what done looks like>` keeps the agent working until it shows the goal is met.
- **Extensions.** Skills (`SKILL.md`), MCP servers (stdio and HTTP), hooks, and project memory via `OPENLEASH.md` / `AGENTS.md` / `CLAUDE.md`.
- **Review view.** Per-file revert and commit, with or without git.
- **No telemetry.** Requests go only to the providers and services you use.

The full list is in [docs/features.md](docs/features.md).

## Roadmap

- Remote connectivity

## Getting started

You need **Node.js 22 LTS** (CI uses Node 22; the package engine accepts Node `^20.19.0` or `^22.12.0`), **Rust** (via [rustup](https://rustup.rs)) and your platform's native webview libraries. Per-platform details are in [docs/building.md](docs/building.md).

```sh
git clone https://github.com/DevLeashed/OpenLeash.git
cd OpenLeash
npm install
npm run tauri dev
```

Then open **Settings → Models**, add an API key, pick a project folder and start a task.

You can also set a key through the environment (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `OPENROUTER_API_KEY`, `ZAI_API_KEY`, `OPENCODE_API_KEY`). More in [docs/configuration.md](docs/configuration.md).

### Platform support

| Platform | Status |
|---|---|
| Windows (x64, ARM64) | Supported |
| macOS (Apple Silicon) | Supported |
| Linux (x86-64, ARM64) | Supported; native build dependencies are required. See [docs/building.md](docs/building.md). |

To make a production build, run `npm run tauri build`.

## Shortcuts

| Key | Action |
|---|---|
| `Ctrl K` | Command palette |
| `Ctrl N` | New task |
| `Shift Tab` | Toggle plan mode |
| `Alt Enter` | Steer the agent without stopping its turn |
| `Esc` | Pause. Press twice to hard-stop. |
| `Ctrl O` | Open a project folder |

Type `/` in the composer for slash commands (`/goal`, `/plan`, `/compact`, `/model`, `/review`, `/cost` and more). The full list is in [docs/shortcuts.md](docs/shortcuts.md). On macOS, `Ctrl` shortcuts appear as `⌘`.

## Security

OpenLeash runs a model on your machine with the tools to read and write files and execute shell commands. **Nothing sandboxes it.** Before pointing it at a repository you do not trust, note that:

- reads are not sandboxed, so the agent can read anything your account can;
- a repo's `AGENTS.md` / `CLAUDE.md` is injected into the agent's prompt, so open untrusted repos in plan mode;
- enabled MCP servers start when the app launches;
- API keys are stored in plaintext in `~/.openleash/settings.json`.

Read the full [security model](docs/security-model.md). To report a vulnerability, follow [SECURITY.md](SECURITY.md).

## Development

```sh
npm run check                         # typecheck + ESLint + Vitest
npm run build                         # TypeScript + production Vite bundle
cd src-tauri && cargo fmt --all --check
cd src-tauri && cargo test --locked
```

| Path | What lives there |
|---|---|
| `src/` | React frontend (`store.ts` is the custom external store, `api.ts` the Tauri bridge) |
| `src/ui/` | Screens, overlays and shared primitives |
| `src-tauri/src/agent/permissions.rs` | The permission gate (**security-critical**) |
| `src-tauri/src/agent/runner.rs` | The turn loop: tool dispatch, compaction, goals |
| `src-tauri/src/agent/tools.rs` | Tool schemas and implementations |
| `src-tauri/src/lib.rs` | Every `#[tauri::command]` |

Changes to `permissions.rs`, `tools.rs` or tool dispatch need a test that fails before the change. Read [AGENTS.md](AGENTS.md) for the full conventions, including how to work in a tree that other people or agents are editing at the same time.

## Documentation

- [Features](docs/features.md)
- [Building from source](docs/building.md)
- [Configuration](docs/configuration.md)
- [Shortcuts and slash commands](docs/shortcuts.md)
- [Security model](docs/security-model.md)
- [Releases and signing](docs/releasing.md)
- [Maintainer release checklist](docs/release-checklist.md)

## Contributing

[CONTRIBUTING.md](CONTRIBUTING.md) covers the setup, the checks to run before
opening a pull request, and what a change to the permission layer needs to
include. [AGENTS.md](AGENTS.md) has the fuller engineering conventions.
Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).

Security bugs go through a private advisory, not an issue — see
[SECURITY.md](SECURITY.md).

## License

MIT. See [LICENSE.md](LICENSE.md) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
