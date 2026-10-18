//! Path-scoped instruction rules.
//!
//! OpenLeash's five flat instruction files ([`super::MEMORY_FILES`]) are all or
//! nothing per project: `AGENTS.md` is injected whole or not at all, and there is
//! no way to say "this rule applies only to `src/**/*.ts`". Every comparable tool
//! has that knob — Claude Code's `.claude/rules/*.md` with `paths:` frontmatter,
//! Copilot's `applyTo`, Cursor's `globs`, Cline, Windsurf, OpenHands' path
//! triggered injection — so a project that already ships such rules can hand them
//! to OpenLeash unchanged.
//!
//! Discovery reads `*.md` one level deep from two directories under the project
//! root: `.openleash/rules/` (ours) and `.claude/rules/` (interop, the same reason
//! `CLAUDE.md` is read). A file may open with YAML frontmatter carrying a path
//! scope; a file with no scope is always on.
//!
//! Two properties are load-bearing and both are deliberately *fail closed*:
//!
//! - **A scope we cannot parse never becomes "always on".** Injecting a rule
//!   whose scope we misread applies a security-relevant instruction to files the
//!   user never named; injecting nothing merely makes the rule not fire where it
//!   should. So a malformed frontmatter block, an unknown key (which might be a
//!   scope declaration we do not understand), an unreadable file or a bad glob
//!   all drop the rule and record *why* — see [`discover`].
//! - **A rule's scope cannot escape the project.** Matching is done on the path
//!   *relative to the project root*, and a path that steps above the root
//!   (`../..`) or sits outside it (`C:/Windows/…`) resolves to `None` and matches
//!   nothing, so a scope can only ever cover files the user actually opened.
//!
//! The rendered section is deterministic — files are sorted, no timestamp or map
//! iteration order leaks in — because the bytes can reach the frozen system
//! prompt, which is reused verbatim for the provider's prompt cache.

use crate::agent::host;
use crate::agent::trust;
use std::path::Path;

/// Where OpenLeash looks for rule files, in a fixed order.
///
/// `.openleash/rules` is our own layout; `.claude/rules` is read for the same
/// interop reason `CLAUDE.md` is. Each directory is scanned a single level deep
/// and only `*.md` files are used — a directory of markdown, not a tree.
pub const RULE_DIRS: &[&str] = &[".openleash/rules", ".claude/rules"];

/// Frontmatter keys that *scope* a rule. Their value may be a YAML list or a
/// single string; both keys are merged when both are present.
const SCOPE_KEYS: &[&str] = &["paths", "globs"];

/// Frontmatter keys we recognise and deliberately ignore. These are inert
/// metadata that cannot be a path scope, so a rule that carries one is still
/// usable; any *other* key is treated as a scope we do not understand and drops
/// the rule (see the module docs).
const INERT_KEYS: &[&str] = &["description", "name", "title"];

/// The per-file cap on a rule body, matching the project instruction cap. Kept
/// here rather than derived so a rule cannot quietly become the largest thing in
/// the prompt.
const RULE_CAP: usize = 40_000;

/// A parsed, usable rule.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Project-relative path of the file, e.g. `.openleash/rules/ts.md`. This is
    /// what the `<file name="…">` block shows, and what sorts the section.
    pub name: String,
    /// The markdown body, frontmatter stripped and clipped like a memory file.
    pub body: String,
    /// `None` = always on. See [`Rule::matches`].
    scope: Option<Scope>,
}

/// A compiled path scope.
#[derive(Debug, Clone)]
struct Scope {
    /// At least one must match. Empty (a scope written only with `!` patterns)
    /// matches nothing — fail closed rather than "everything except".
    include: Vec<globset::GlobMatcher>,
    /// None may match. A `!`-prefixed pattern lands here.
    exclude: Vec<globset::GlobMatcher>,
}

impl Rule {
    /// Whether this rule governs the project-relative path `rel` (which the
    /// caller has already normalized to forward slashes and bounded to the
    /// project — see [`rel_path`]).
    fn matches(&self, rel: &str) -> bool {
        match &self.scope {
            None => true,
            Some(s) => {
                if s.include.is_empty() {
                    return false;
                }
                s.include.iter().any(|m| m.is_match(rel))
                    && !s.exclude.iter().any(|m| m.is_match(rel))
            }
        }
    }
}

/// Discover the rules under `root`, and the reason for every file left out.
///
/// Deliberately *not* trust-gated: this is the raw inventory, the same way
/// [`super::project_files`] is, so a manifest can show the user what a folder is
/// asking for before they trust it. The gate is applied by [`rules_for`] and
/// [`path_rules_section`], which are what actually reach the prompt.
///
/// The second element is one line per rejected file, naming the file and why, so
/// a fail-closed drop is visible rather than silent.
pub fn discover(root: &str) -> (Vec<Rule>, Vec<String>) {
    let mut rules = Vec::new();
    let mut warnings = Vec::new();
    // Rule files are read from the local filesystem; a remote-SSH project root is
    // not a path we can `read_dir`, so it contributes nothing (as its other
    // project-local items do not).
    if root.is_empty() || host::is_ssh(root) {
        return (rules, warnings);
    }
    for dir in RULE_DIRS {
        let abs = Path::new(root).join(dir);
        let Ok(read) = std::fs::read_dir(&abs) else {
            continue;
        };
        // `read_dir` order is filesystem-dependent; sort so the result is stable
        // before it is even filtered.
        let mut files: Vec<std::path::PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
            .collect();
        files.sort();
        for p in files {
            let name = format!(
                "{dir}/{}",
                p.file_name().unwrap_or_default().to_string_lossy()
            );
            let Ok(text) = std::fs::read_to_string(&p) else {
                warnings.push(format!("{name}: could not be read"));
                continue;
            };
            match parse(&text) {
                Ok((Parsed::Always, body)) => {
                    if let Some(body) = usable_body(&body) {
                        rules.push(Rule {
                            name,
                            body,
                            scope: None,
                        });
                    }
                }
                Ok((Parsed::Scoped(patterns), body)) => match compile(&patterns) {
                    Ok(scope) => {
                        if let Some(body) = usable_body(&body) {
                            rules.push(Rule {
                                name,
                                body,
                                scope: Some(scope),
                            });
                        }
                    }
                    Err(why) => warnings.push(format!("{name}: {why}")),
                },
                Err(why) => warnings.push(format!("{name}: {why}")),
            }
        }
    }
    rules.sort_by(|a, b| a.name.cmp(&b.name));
    (rules, warnings)
}

/// A body that is worth injecting, clipped to the same cap as a memory file.
fn usable_body(raw: &str) -> Option<String> {
    if raw.trim().is_empty() {
        // A rule with no text cannot instruct anything; keeping it would only add
        // an empty `<file>` block to the prompt.
        return None;
    }
    Some(super::clip(raw.trim(), RULE_CAP))
}

/// The rules that govern `path`, as `(name, body)` pairs in stable name order.
///
/// Trust-gated: an untrusted folder contributes nothing, exactly as
/// [`super::memory_for_prompt`] withholds a project's instruction files. A path
/// that escapes the project matches nothing.
pub fn rules_for(project: &str, path: &str) -> Vec<(String, String)> {
    if !trust::trusted_now(project) {
        return Vec::new();
    }
    let Some(rel) = rel_path(project, path) else {
        return Vec::new();
    };
    let (rules, _) = discover(project);
    rules
        .into_iter()
        .filter(|r| r.matches(&rel))
        .map(|r| (r.name, r.body))
        .collect()
}

/// The applicable rules formatted for injection, or `""` when there are none.
///
/// Shape mirrors the project-instructions block in [`super::system`]:
/// a `# ` heading, then one `<file name="…">…</file>` per rule. The heading says
/// these instructions are scoped to this path and may differ for other files, so
/// the model does not read them as project-wide.
///
/// Byte-deterministic for the same inputs — the content can reach the frozen
/// prompt and must not perturb the cached prefix.
pub fn path_rules_section(project: &str, path: &str) -> String {
    let rules = rules_for(project, path);
    if rules.is_empty() {
        return String::new();
    }
    // `rules_for` only returns when the path resolved inside the project, so this
    // re-resolves to the same value; sanitize it regardless, because a path can
    // carry control characters or backticks and this text is quoted into a prompt.
    let shown = sanitize(&rel_path(project, path).unwrap_or_default());
    let mut s = format!(
        "\n# Path-scoped rules\nThese rules are scoped to `{shown}` and may differ for other files. Apply them to `{shown}` only; do not assume they hold elsewhere in the project.\n"
    );
    for (name, body) in rules {
        s.push_str(&format!(
            "\n<file name=\"{}\">\n{body}\n</file>\n",
            sanitize(&name)
        ));
    }
    s
}

/// Make a string safe to inline into the prompt: drop control characters (a path
/// can contain a newline on every platform) and neutralize backticks so a value
/// cannot break out of the code span it is quoted in.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '`' { '\'' } else { c })
        .collect()
}

/// `path` as a project-relative, forward-slashed path, or `None` when it is not
/// inside the project.
///
/// `None` covers both shapes of escape the brief calls out: a relative path that
/// steps above the root (`../../etc/passwd`) and an absolute path on a different
/// tree (`C:/Windows/x` for a project elsewhere). Lexical, deliberately — a path
/// to a file that does not exist yet must still resolve, and following a symlink
/// would move a rule's scope onto its target.
fn rel_path(project: &str, path: &str) -> Option<String> {
    let norm = |s: &str| s.replace('\\', "/");
    let trim_slash = |mut s: String| {
        while s.len() > 1 && s.ends_with('/') {
            s.pop();
        }
        s
    };
    let p = trim_slash(norm(path));
    if p.is_empty() {
        return None;
    }
    let root = trim_slash(norm(project));
    // An absolute path (POSIX `/…` or a Windows drive/UNC) is only usable when it
    // lives under the project root; strip the root to make it relative.
    let mut rest = p.as_str();
    if p.starts_with('/') || is_abs_drive(&p) {
        let head = p.get(..root.len())?;
        let same = if cfg!(windows) {
            head.eq_ignore_ascii_case(&root)
        } else {
            head == root
        };
        // The character after the root must be a separator, or `D:/proj2` would
        // read as being under `D:/proj`.
        if !same || p.as_bytes().get(root.len()) != Some(&b'/') {
            return None;
        }
        rest = &p[root.len() + 1..];
    }
    // Resolve `.`/`..` lexically. A `..` with nothing to pop escapes the root.
    let mut parts: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        // The project root itself is not a file.
        return None;
    }
    Some(parts.join("/"))
}

/// Whether `p` looks like a Windows absolute path (`C:/…`, `c:/…`).
fn is_abs_drive(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
}

// ───────────────────────── frontmatter ─────────────────────────

/// The scope a file's frontmatter declared.
#[derive(Debug, PartialEq)]
enum Parsed {
    /// No frontmatter, or frontmatter with no scope key: always on.
    Always,
    /// One or more glob patterns (from `paths:` and/or `globs:`).
    Scoped(Vec<String>),
}

/// Split a rule file into its declared scope and its body.
///
/// Frontmatter must open on the first line (after a BOM) with `---` and close on
/// a later line that is exactly `---` or `...`. Anything else — a `---` that
/// never closes, an unknown key, an unrecognized line — is an `Err` naming the
/// problem, and the caller drops the file. That is the fail-closed direction: a
/// rule that tried to declare a scope we cannot read must not be treated as
/// always on.
fn parse(text: &str) -> Result<(Parsed, String), String> {
    let t = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut lines = t.lines();
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        // No frontmatter at all: the whole file is the body, always on.
        _ => return Ok((Parsed::Always, t.to_string())),
    }
    let mut front = Vec::new();
    let mut body = Vec::new();
    let mut closed = false;
    for line in lines {
        if closed {
            body.push(line);
        } else if line.trim() == "---" || line.trim() == "..." {
            closed = true;
        } else {
            front.push(line);
        }
    }
    if !closed {
        return Err("frontmatter opens with `---` but is never closed".into());
    }
    let scope = parse_front(&front)?;
    Ok((scope, body.join("\n")))
}

/// Parse the lines between the `---` fences.
fn parse_front(lines: &[&str]) -> Result<Parsed, String> {
    let mut scope: Vec<String> = Vec::new();
    let mut saw_scope_key = false;
    // The key the most recent line introduced, so a following `- item` list is
    // attributed to it (or ignored, for an inert key).
    let mut cur: Option<&str> = None;
    for raw in lines {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            match cur {
                Some(k) if SCOPE_KEYS.contains(&k) => push_pattern(&mut scope, item),
                Some(_) => {}
                None => return Err(format!("list item `{trimmed}` has no key")),
            }
            continue;
        }
        let Some((k, v)) = trimmed.split_once(':') else {
            return Err(format!("unrecognized frontmatter line `{trimmed}`"));
        };
        let (k, v) = (k.trim(), v.trim());
        if SCOPE_KEYS.contains(&k) {
            saw_scope_key = true;
            cur = Some(k);
            if v.is_empty() {
                continue;
            }
            if let Some(inner) = v.strip_prefix('[') {
                // Inline list: `paths: [a, b, c]`.
                for part in inner.strip_suffix(']').unwrap_or(inner).split(',') {
                    push_pattern(&mut scope, part);
                }
            } else {
                // A single scalar value on one line.
                push_pattern(&mut scope, v);
            }
        } else if INERT_KEYS.contains(&k) {
            cur = Some("inert");
        } else {
            // A key we do not know could be a scope declaration in a dialect we
            // do not read; refusing the file is the safe direction.
            return Err(format!("unsupported frontmatter key `{k}`"));
        }
    }
    if saw_scope_key && scope.is_empty() {
        return Err("declares a scope key but names no globs".into());
    }
    Ok(if saw_scope_key {
        Parsed::Scoped(scope)
    } else {
        Parsed::Always
    })
}

/// Push one (possibly quoted) pattern, dropping empties.
fn push_pattern(scope: &mut Vec<String>, raw: &str) {
    let p = unquote(raw.trim());
    if !p.is_empty() {
        scope.push(p);
    }
}

/// Strip one layer of matching single or double quotes.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

/// Compile the declared patterns into a [`Scope`].
///
/// `\` is normalized to `/` first, so a rule written on Windows
/// (`src\**\*.ts`) means the same as a `/`-separated one. Globs are anchored at
/// the project root and matched case-sensitively; `*`/`?` do not cross a `/`,
/// `**` may.
fn compile(patterns: &[String]) -> Result<Scope, String> {
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    for raw in patterns {
        let mut p = raw.replace('\\', "/");
        let negated = p.starts_with('!');
        if negated {
            p = p[1..].to_string();
        }
        while let Some(r) = p.strip_prefix("./") {
            p = r.to_string();
        }
        let p = p.trim_start_matches('/').to_string();
        if p.is_empty() {
            return Err(format!("empty glob pattern in `{raw}`"));
        }
        let m = globset::GlobBuilder::new(&p)
            .literal_separator(true)
            .build()
            .map_err(|e| format!("bad glob `{p}`: {e}"))?
            .compile_matcher();
        if negated {
            exclude.push(m);
        } else {
            include.push(m);
        }
    }
    Ok(Scope { include, exclude })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::trust::{TrustDecision, TrustKind, TrustState};

    /// A fresh project folder, unique per test (mirrors `reviewer_trust.rs`).
    fn project(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ol-pathrules-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Write a rule file, creating its directory.
    fn rule(root: &std::path::Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// Trust `project`, holding the process-global lock so a parallel test cannot
    /// have its rules read against this table (mirrors `reviewer_trust.rs`).
    fn trust(project: &str) -> std::sync::MutexGuard<'static, ()> {
        let g = trust::TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        trust::set_trust(&[TrustDecision {
            path: trust::norm_path(project),
            kind: TrustKind::Folder,
            decision: TrustState::Trusted,
            decided_at: chrono::Utc::now(),
        }]);
        g
    }

    fn no_trust() -> std::sync::MutexGuard<'static, ()> {
        let g = trust::TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        trust::set_trust(&[]);
        g
    }

    fn names(project: &str, path: &str) -> Vec<String> {
        rules_for(project, path)
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    }

    #[test]
    fn a_rule_scoped_to_src_ts_matches_only_ts_under_src() {
        let d = project("ts");
        rule(
            &d,
            ".openleash/rules/ts.md",
            "---\npaths: src/**/*.ts\n---\nOnly touch TypeScript under src.",
        );
        let p = d.to_string_lossy().into_owned();
        let _g = trust(&p);

        assert_eq!(
            names(&p, "src/a/b.ts"),
            vec![".openleash/rules/ts.md"],
            "a src/**/*.ts scope must match src/a/b.ts"
        );
        assert!(
            names(&p, "README.md").is_empty(),
            "README.md is not under src"
        );
        assert!(names(&p, "src/a/b.py").is_empty(), "b.py is not a .ts file");
        // `**` spans directories, so a deeper file under src still matches — and a
        // file outside src never does.
        assert_eq!(
            names(&p, "src/a/deep/b.ts"),
            vec![".openleash/rules/ts.md"],
            "src/**/*.ts must match src/a/deep/b.ts"
        );
        assert!(
            names(&p, "other/a/b.ts").is_empty(),
            "the scope is anchored at the project root"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn glob_wildcards_behave_star_question_and_double_star() {
        // Pure matcher checks, no filesystem or trust involved.
        let scoped = |pat: &str| {
            let s = compile(&[pat.to_string()]).unwrap();
            Rule {
                name: "r".into(),
                body: "b".into(),
                scope: Some(s),
            }
        };
        let star = scoped("src/*.ts");
        assert!(star.matches("src/a.ts"));
        assert!(!star.matches("src/a/b.ts"), "* must not cross a separator");
        let q = scoped("src/?.ts");
        assert!(q.matches("src/a.ts"));
        assert!(!q.matches("src/ab.ts"), "? is exactly one character");
        let deep = scoped("src/**/*.ts");
        assert!(deep.matches("src/a/b.ts"));
        assert!(deep.matches("src/b.ts"), "** matches zero directories too");
        assert!(
            !deep.matches("other/a/b.ts"),
            "scope is anchored at the root"
        );
        // Negation: any positive match, minus every `!` match.
        let neg = compile(&["src/**/*.ts".into(), "!src/gen/**".into()]).unwrap();
        let neg = Rule {
            name: "r".into(),
            body: "b".into(),
            scope: Some(neg),
        };
        assert!(neg.matches("src/a/b.ts"));
        assert!(!neg.matches("src/gen/b.ts"), "a `!` pattern excludes");
        // A scope that is only negations matches nothing (fail closed).
        let only_neg = compile(&["!src/**".into()]).unwrap();
        let only_neg = Rule {
            name: "r".into(),
            body: "b".into(),
            scope: Some(only_neg),
        };
        assert!(!only_neg.matches("anything.ts"));
    }

    #[test]
    fn a_rule_without_frontmatter_applies_to_every_path() {
        let d = project("always");
        rule(&d, ".openleash/rules/all.md", "Applies everywhere.");
        let p = d.to_string_lossy().into_owned();
        let _g = trust(&p);

        for path in ["README.md", "src/a/b.ts", "deep/nested/file.xyz"] {
            assert_eq!(
                names(&p, path),
                vec![".openleash/rules/all.md"],
                "an unscoped rule must reach {path}"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn malformed_or_unknown_scope_is_not_injected_and_the_reason_is_reported() {
        let d = project("bad");
        // An unknown key: it might be a scope in a dialect we do not read.
        rule(
            &d,
            ".openleash/rules/copilot.md",
            "---\napplyTo: src/**\n---\nplanted scope",
        );
        // Frontmatter that opens and never closes.
        rule(
            &d,
            ".openleash/rules/unclosed.md",
            "---\npaths: src/**\nbody with no closing fence",
        );
        let p = d.to_string_lossy().into_owned();
        let _g = trust(&p);

        // Raw inventory: both are left out, and each with a reason.
        let (rules, warnings) = discover(&p);
        assert!(rules.is_empty(), "no malformed rule may survive: {rules:?}");
        assert_eq!(
            warnings.len(),
            2,
            "one reason per rejected file: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("copilot.md")
                    && w.contains("unsupported frontmatter key `applyTo`")),
            "the unknown key must be named: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("unclosed.md") && w.contains("never closed")),
            "the unclosed fence must be named: {warnings:?}"
        );

        // And the fail-closed direction, through the prompt-facing API: the wrong
        // rule is NOT applied anywhere, rather than silently becoming always on.
        for path in ["README.md", "src/a/b.ts", "src/x/y/z.rs"] {
            assert!(
                names(&p, path).is_empty(),
                "a rule with a scope we cannot read must not be injected at {path}"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn windows_separators_match_the_same_as_forward_slashes() {
        let d = project("sep");
        rule(
            &d,
            ".openleash/rules/ts.md",
            "---\npaths: src/**/*.ts\n---\nonly src ts",
        );
        // A second rule whose pattern is *written* with backslashes.
        rule(
            &d,
            ".openleash/rules/win.md",
            "---\nglobs: lib\\**\\*.rs\n---\nonly lib rs",
        );
        let p = d.to_string_lossy().into_owned();
        let _g = trust(&p);

        assert_eq!(
            names(&p, "src\\a\\b.ts"),
            vec![".openleash/rules/ts.md"],
            "a backslash path must match a forward-slash scope"
        );
        assert_eq!(
            names(&p, "lib\\a\\b.rs"),
            vec![".openleash/rules/win.md"],
            "a backslash pattern must match a forward-slash path"
        );
        // Both spellings agree.
        assert_eq!(names(&p, "src/a/b.ts"), names(&p, "src\\a\\b.ts"));
        assert_eq!(names(&p, "lib/a/b.rs"), names(&p, "lib\\a\\b.rs"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_path_that_escapes_the_project_never_matches() {
        let d = project("escape");
        rule(&d, ".openleash/rules/all.md", "scratch\neverywhere");
        let p = d.to_string_lossy().into_owned();
        let _g = trust(&p);

        // Relative escapes and an absolute path outside the project.
        for path in [
            "../../etc/passwd",
            "..\\..\\windows\\system32\\config",
            "C:/Windows/x",
            "C:\\Windows\\x",
            "/etc/passwd",
            "",
        ] {
            assert!(
                names(&p, path).is_empty(),
                "a project-scoped rule must not match an escaping path {path:?}"
            );
        }
        // An absolute path *inside* the project still works.
        let inside = d.join("src").join("a.ts");
        assert_eq!(
            names(&p, &inside.to_string_lossy()),
            vec![".openleash/rules/all.md"],
            "an absolute path under the root is fine"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_rendered_section_is_byte_identical_across_calls() {
        let d = project("determinism");
        // Written in an order unrelated to their names, to prove the section is
        // sorted rather than read-order dependent.
        rule(
            &d,
            ".openleash/rules/zeta.md",
            "---\npaths: src/**\n---\nzeta rule",
        );
        rule(
            &d,
            ".claude/rules/alpha.md",
            "---\nglobs: [src/**, lib/**]\n---\nalpha rule",
        );
        let p = d.to_string_lossy().into_owned();
        let _g = trust(&p);

        let a = path_rules_section(&p, "src/a/b.ts");
        let b = path_rules_section(&p, "src/a/b.ts");
        assert_eq!(a, b, "the section must be byte-identical across calls");
        assert!(!a.is_empty());
        // Both rules apply; the `.claude` one sorts first.
        let alpha = a.find("alpha.md").expect("alpha present");
        let zeta = a.find("zeta.md").expect("zeta present");
        assert!(alpha < zeta, "rules must render in name order:\n{a}");
        assert!(a.contains("Path-scoped rules"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_untrusted_project_yields_no_rules() {
        let d = project("untrusted");
        rule(&d, ".openleash/rules/all.md", "SHOULD-NOT-APPEAR");
        let p = d.to_string_lossy().into_owned();
        let _g = no_trust();

        assert!(
            rules_for(&p, "README.md").is_empty(),
            "an untrusted folder must contribute nothing"
        );
        assert_eq!(
            path_rules_section(&p, "README.md"),
            "",
            "the rendered section must be empty for an untrusted folder"
        );
        // The raw inventory still sees it, for the trust manifest to show.
        assert_eq!(discover(&p).0.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
