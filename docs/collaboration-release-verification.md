# Collaboration release verification — 2026-10-12

This records checks run locally against the shared D:/openleash working tree. It is not a publication/signing certificate or a cross-platform runtime verdict.

## Implemented

- Optional chat/workspace collaboration with narrow-window stacked layout.
- Searchable artifict overview, unresolved feedback counts, safe inert SVG/embedded-image thumbnails and author-reported stages.
- Exact-version feedback and implementation/architecture/visual-investigation request staging, with explicit send and unchanged tool permissions.
- Bounded JSON collaboration records: component/dependency discussion targets, relative source refs, implementation/evidence entries and limitations.
- Keyboard-accessible percentage screenshot-region proposal, marker discussion and before/after comparisons preserving original images.
- Selected-text targeting restricted to the artifict preview; all-version response history.
- User and agent guide: `docs/artifict-collaboration.md`.

## Checks actually run

- `npm run check`: 86 files, **808 tests passed**, TypeScript/ESLint passed.
- `npm run build`: production frontend bundle passed; Vite large-chunk warning remains.
- `npx eslint . --max-warnings 89`: exit 0.
- `npm audit --audit-level=high`: **0 vulnerabilities** after compatible Vitest 4.1.11 upgrade and source-map-js transitive patch. Node on this machine is 24.12.0, outside this package's declared 20/22 range; Node 22 CI/runtime compatibility was not locally exercised.
- `cargo fmt --all --check`: passed.
- `cargo test --locked -- --test-threads=1`: **616 passed, 1 ignored**, no failures. Serial execution is not proof that earlier parallel flakiness is repaired.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo audit`: exit 0, unmaintained-dependency warnings remain (proc-macro-error and unic family).
- `CI=true npm run tauri build -- --no-bundle`: optimized Windows executable built at `src-tauri/target/release/openleash.exe`. The first invocation failed because inherited CI=1 was rejected; setting CI=true fixed the invocation. No installer/signing/publishing was performed. This build predates commits `8065a1f`–`a722967`; rebuild before smoke testing.

## Second pass on HEAD `a722967` (Node 22, parallel Rust)

Commits after `2ebd3ee` changed archiving of paused chats (`src-tauri/src/lib.rs`), `providers.rs` and the model manager; this pass covers them.

- Node **22.23.3** (npm 11.6.2 run through the Node 22 executable; the system `npm` launcher otherwise uses Node 24): `npm ci`, `npm run check` (86 files, **809 tests passed**), `npm run build`, `eslint . --max-warnings 89`, `npm audit --audit-level=high` (**0 vulnerabilities**) all exited 0.
- An earlier attempt that actually ran under Node 24 while the Rust suite compiled concurrently failed one test: `version-metadata.test.ts` "runs npm version patch…" timed out at its 5 s limit (it spawns a real `npm version`). It passed in the unloaded Node 22 run and every prior run, so it reads as load-sensitive, not a regression.
- `cargo fmt --all --check` and `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo test --locked` in default **parallel** mode: first run **616 passed, 1 failed, 1 ignored**. The failure was `reviewer_notices::notice_dismissal_reports_failure_and_preserves_pending_card` at its final cleanup line, `remove_dir_all(home)`: Windows error 32, file in use by another process, after all of its real assertions passed. It then passed alone 3/3 times and a full parallel rerun passed **617 passed, 0 failed, 1 ignored**. Treat it as an intermittent Windows temp-folder cleanup race in the test; it is not fixed.
- The three persistence/pause failures seen in an earlier parallel run did not recur in either parallel run here. Two clean runs do not prove they are gone.

## Required desktop smoke before release

- Open the new build using isolated/dummy settings and project data, not real credentials.
- Create a collaboration JSON artifict; explore a component and dependency, review the private proposal, explicitly send, revise/respond and compare exact versions.
- Select a screenshot region, verify its overlay/percentage coordinates, send it and confirm the agent receives only reviewed fields.
- Check chat/composer remains usable alongside artificts at 900/1200/large widths and with details pane open.
- Verify image failure/retry, diagram/lightbox keyboard focus, preview size changes and theme readability.
- Exercise fresh install and upgrade on intended platforms; verify updater signatures/installers separately.

No desktop interaction smoke, installer test, cross-platform runtime test, publication or version-number change is claimed by this record.
