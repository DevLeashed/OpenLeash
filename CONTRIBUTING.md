# Contributing to OpenLeash

Thanks for considering a contribution. This document covers the practical part:
getting set up, what a change needs, and what "done" means here.

If you have found a **security** bug rather than a feature or fix, do not open a
public issue — see [SECURITY.md](SECURITY.md).

## Getting set up

You need **Node.js 22 LTS** (the package engine accepts Node `^20.19.0` or `^22.12.0`, but CI and release builds use 22), **Rust** (via [rustup](https://rustup.rs)) and your platform's native webview libraries. Per-platform prerequisites are in [docs/building.md](docs/building.md).

```sh
git clone https://github.com/DevLeashed/OpenLeash.git
cd OpenLeash
npm install
npm run tauri dev
```

Note that the Rust crate does not currently compile on Linux — see the note in
`src-tauri/Cargo.toml` and the `rust` job in `.github/workflows/ci.yml`. CI runs
the Rust tests on Windows for this reason. Frontend work is unaffected.

## Before you open a pull request

```sh
npm run check                           # tsc --noEmit + eslint + vitest
cd src-tauri && cargo fmt --all
cd src-tauri && cargo test --locked
```

`cargo clippy --locked --all-targets -- -D warnings` is **not** required — it still
reports whole-crate debt that is unrelated to any one change, and CI treats it as
report-only.

Two conventions matter more than the rest:

- **Don't run `cargo fmt --all` casually.** It reformats every file in the crate.
  If you touch Rust, format only what you edited (`rustfmt src/agent/yourfile.rs`),
  or run `cargo fmt --all` in a commit of its own with a note saying you did.
- **Scope your commit.** One logical change per commit, named for the area it
  touches (`permissions: reject chained redirects in a subshell`).

## Tests

Anything touching `permissions.rs`, `tools.rs`, or the tool-dispatch path in
`runner.rs` needs a test that **fails before the change**. That is not a style
preference: those files decide which commands a language model is allowed to run
on your machine.

Two cases deserve specific attention in `permissions.rs`, because the guard is a
literal-character denylist rather than a parser:

- **Chained commands** — `&&`, `;`, `|`, `$(...)`, backticks, redirects.
- **Command substitution** — including nested and quoted forms.

If you extend the threat model with a new shell metacharacter, add it to the
denylist *and* to the test corpus in the same change. A character that is blocked
in the code but absent from the tests is a silent regression waiting for a
proof-of-concept.

New Rust tests go in a `reviewer_<area>.rs` module or the `#[cfg(test)]` block of
the file they cover — not in a catch-all. Tests that share process-global state
need to serialize themselves; `agent/pcguard.rs` has an example.

## Working in this tree

More than one person or agent may be editing the repository at a time, and the
git history is not linear. Please:

- **Do not rewrite or revert work you did not write.** If a file has changes you
  do not recognise, read them and work with them.
- **Do not use git to undo anything.** No `checkout`, `restore`, `reset`,
  `revert`, `stash` or `clean` on files you did not change.
- Keep commits small and scoped to the area you touched.
- If you reformat, say so explicitly in the commit message, and scope it to the
  files you actually edited.

[AGENTS.md](AGENTS.md) has the fuller version of these conventions, and is worth
reading before a large change — it also documents the layout and why the
permission layer is shaped the way it is.

## Comments

The house style is a short block **above** the non-obvious decision explaining
**why**, often noting the alternative that was rejected and what would break if
someone "fixed" it. Match that rather than restating what the code does.

## Reporting bugs

Open an issue with: what you did, what you expected, what happened, and your OS
and OpenLeash version (`Settings → About`). If the bug involves the agent
running commands, **use a throwaway VM or a scoped directory** — a PoC for an
agent harness can be destructive by definition, and we would rather not have to
ask.

## Licence

Contributions are accepted under the [MIT licence](LICENSE.md) that covers this
repository. By opening a pull request you confirm you have the right to submit the
code under it.