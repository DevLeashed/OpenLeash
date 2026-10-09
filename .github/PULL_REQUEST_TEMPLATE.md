## What this changes

<!-- One or two sentences. Link the issue it fixes, if there is one: Fixes #123 -->

## How it was tested

<!-- What did you run, and what did you check by hand? -->

- [ ] `npm run check` passes
- [ ] `cargo fmt --all` (or: I did not touch Rust)
- [ ] `cargo test --locked` passes
- [ ] I added or updated a test that would have failed before this change

## If this touches the permission layer

`permissions.rs`, `tools.rs`, or tool dispatch in `runner.rs` are security-critical,
so a PR there needs more than a passing suite:

- [ ] The new behaviour has a test that **fails without this change**
- [ ] Chained-command and command-substitution cases are covered explicitly
- [ ] Any new shell metacharacter is added to the denylist **and** to the test corpus
- [ ] I have explained, above, what an attacker could do before that this stops

## Notes for the reviewer

<!-- Anything non-obvious: a deliberate decision, a rejected alternative, a place
     where you were unsure. This is more useful to a reviewer than the diff is. -->

---

By opening this pull request you confirm that your contribution is licensed under
the [MIT licence](LICENSE.md) covering this repository.