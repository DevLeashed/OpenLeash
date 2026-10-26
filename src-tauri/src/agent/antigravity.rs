//! Antigravity CLI (`agy`) as a model backend.
//!
//! # Why there is no login here
//!
//! Google's terms do not allow a third-party client to drive Antigravity with
//! your account: the accounts that were suspended were suspended for pulling the
//! Antigravity OAuth token out of the vendor's files and replaying it against
//! Google's endpoints. So this module never reads, stores, refreshes or sends a
//! credential. `agy` owns its own login (system keyring, its own sign-in flow),
//! and OpenLeash only runs the official binary and reads its documented headless
//! output. Nothing here can be mistaken for a second client of Google's API,
//! because nothing here speaks to Google at all — the CLI does.
//!
//! # What that costs
//!
//! `agy` is an agent, not a model. It runs its own tool loop with its own tools
//! (`run_command`, `write_to_file`, browser control, subagents…) and does not
//! take a tool schema from us. So a turn routed here is a *delegation*: the CLI
//! decides how to do the work, and OpenLeash's approval layer, plan mode,
//! read-before-write and checkpoints do not reach inside it. That is a trust
//! boundary, documented in `docs/security-model.md`, and the reason the tool
//! steps the CLI takes are streamed back as `StreamEvent::Step` — visibly its
//! own, never dressed up as an OpenLeash tool call.
//!
//! Headless mode soft-denies every tool that would need approval, which would
//! make a delegate that cannot run a test suite. The posture chosen for this
//! provider is therefore full access, passed explicitly as
//! `--dangerously-skip-permissions`; see [`args`].

use super::host;
use super::providers::{self, ChatRequest, Fail, StreamEvent, Target, Turn, TurnUsage};
use super::store;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio_util::sync::CancellationToken;

/// The official CLI's executable name.
pub const BIN: &str = "agy";
/// Where the CLI keeps its own permission rules. Shown in the UI, never written:
/// it is another tool's configuration, and it is the user's to edit.
pub const CLI_SETTINGS: &str = "~/.gemini/antigravity-cli/settings.json";
/// The headless mode documentation this backend is written against.
pub const DOCS: &str = "https://antigravity.google/docs/cli/headless";

// ───────────────────────────── install discovery ─────────────────────────────

fn bin_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["agy.exe", "agy.cmd", "agy"]
    } else {
        &["agy"]
    }
}

/// Absolute path to the CLI, if it is installed.
///
/// PATH first, then the installer's own destinations. The extra directories are
/// not defensive noise: a windowed app on macOS does not inherit the shell's
/// PATH, and neither does a launcher on Linux, so `agy` can be installed and
/// working in a terminal while `which` finds nothing from in here.
pub fn locate() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".antigravity").join("bin"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(&local).join("agy").join("bin"));
        dirs.push(
            PathBuf::from(&local)
                .join("Programs")
                .join("agy")
                .join("bin"),
        );
    }
    if let Some(roaming) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(roaming).join("agy").join("bin"));
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/bin"));

    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        if !seen.insert(dir.clone()) {
            continue;
        }
        for name in bin_names() {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// How the UI reports the install. Not a guess: `authed` is only true once the
/// CLI has answered with a model list, which it needs a working login for.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub found: bool,
    pub path: String,
    pub version: String,
    pub authed: bool,
    pub models: usize,
    pub error: String,
    pub settings_path: String,
    pub docs: String,
}

async fn output(bin: &Path, args: &[&str]) -> Result<String, String> {
    let run = tokio::process::Command::new(bin).args(args).output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), run)
        .await
        .map_err(|_| format!("`{BIN} {}` did not answer within 60s", args.join(" ")))?
        .map_err(|e| format!("could not run {}: {e}", bin.display()))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "`{BIN} {}` failed: {}",
            args.join(" "),
            first_lines(&err, 4)
        ));
    }
    Ok(text)
}

fn first_lines(s: &str, n: usize) -> String {
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The model slugs this install *and* this plan actually offer.
pub async fn models() -> Result<Vec<providers::ModelInfo>, String> {
    let bin = locate().ok_or_else(|| format!("{BIN} is not installed or not on PATH"))?;
    let out = output(&bin, &["models"]).await?;
    let list = providers::parse_agy_models(&out);
    if list.is_empty() {
        return Err(format!(
            "`{BIN} models` listed no models — sign in by running `{BIN}` once in a terminal"
        ));
    }
    Ok(list)
}

/// Install + login state, and a live model refresh.
///
/// The fetched list is pushed into the pool-model slot, which replaces the
/// built-in guesses for this provider: the CLI knows which slugs the account can
/// reach, and a hardcoded list would rot.
pub async fn status() -> Status {
    let mut s = Status {
        settings_path: CLI_SETTINGS.to_string(),
        docs: DOCS.to_string(),
        ..Default::default()
    };
    let Some(bin) = locate() else {
        s.error = format!("{BIN} is not installed or not on PATH");
        return s;
    };
    s.found = true;
    s.path = bin.display().to_string();
    match output(&bin, &["--version"]).await {
        Ok(v) => s.version = first_lines(&v, 1),
        Err(e) => s.error = e,
    }
    match models().await {
        Ok(list) => {
            s.models = list.len();
            s.authed = true;
            providers::set_pool_models("antigravity", list);
        }
        Err(e) => {
            if s.error.is_empty() {
                s.error = e;
            }
        }
    }
    s
}

// ───────────────────────────── conversation memory ───────────────────────────

/// `cache_key` → the CLI's `conversation_id`.
///
/// A CLI turn is one process with one prompt, so continuity across turns comes
/// from `--conversation <id>` rather than from re-sending history (the CLI takes
/// no message list — only text on stdin or `-p`). The mapping is a session
/// handle, like a cookie: losing it costs a delegate its memory, nothing else,
/// which is why it is a small file and not a migration.
fn sessions_path() -> PathBuf {
    store::data_dir().join("antigravity-sessions.json")
}

pub fn load_sessions(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_sessions(path: &Path, map: &HashMap<String, String>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(path, s);
    }
}

static SESSIONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, String>> {
    SESSIONS.get_or_init(|| Mutex::new(load_sessions(&sessions_path())))
}

pub fn conversation(key: &str) -> Option<String> {
    sessions()
        .lock()
        .ok()
        .and_then(|m| m.get(key).cloned())
        .filter(|c| !c.is_empty())
}

fn remember(key: &str, id: &str) {
    if id.is_empty() {
        return;
    }
    let Ok(mut map) = sessions().lock() else {
        return;
    };
    if map.get(key).map(String::as_str) == Some(id) {
        return;
    }
    map.insert(key.to_string(), id.to_string());
    save_sessions(&sessions_path(), &map);
}

/// Drop a chat's delegate memory (used by tests and by nothing else today).
pub fn forget(key: &str) {
    if let Ok(mut map) = sessions().lock() {
        if map.remove(key).is_some() {
            save_sessions(&sessions_path(), &map);
        }
    }
}

// ───────────────────────────────── the turn ──────────────────────────────────

/// The command line for one turn. Pure, so the flags that matter for security
/// can be asserted in a test instead of being buried in a spawn.
///
/// `full_access` is a property of the *request*, not of the provider: a
/// delegated turn that names a workspace gets the CLI's tools approved, while an
/// internal one-shot (a chat title, a summary, a commit message) names no
/// workspace and is run read-only in a scratch directory — there is nothing for
/// it to edit, and it has no business editing anything.
pub fn args(prompt: &str, model: &str, conv: Option<&str>, full_access: bool) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-p".into(),
        prompt.into(),
        "--output-format".into(),
        "stream-json".into(),
        "--model".into(),
        model.into(),
        // A ceiling for a hung run. Cancelling the chat still kills the process.
        "--print-timeout".into(),
        "30m".into(),
    ];
    if full_access {
        // The whole provider is a delegate: headless mode otherwise soft-denies
        // every shell command, leaving an agent that can edit but not verify.
        // This is the posture the provider is documented and gated as; the
        // narrower alternative is `--mode plan`, which is what a request without
        // a workspace gets below.
        a.push("--dangerously-skip-permissions".into());
    } else {
        a.push("--mode".into());
        a.push("plan".into());
    }
    if let Some(c) = conv.filter(|c| !c.is_empty()) {
        a.push("--conversation".into());
        a.push(c.to_string());
    }
    a
}

/// Conversation handles are versioned and scoped to the task, workspace, and
/// model. This prevents stale context and avoids resuming a CLI session under a
/// different workspace or model.
fn session_key(cache_key: &str, cwd: &Path, model: &str) -> String {
    format!("v2:{cache_key}:{}:{model}", cwd.to_string_lossy())
}

/// The text of the newest user message. This is the whole prompt: the delegate
/// keeps its own history, so replaying ours would give it two.
pub fn last_user_text(req: &ChatRequest) -> String {
    for msg in req.messages.iter().rev() {
        if msg.role != "user" {
            continue;
        }
        let parts: Vec<String> = msg
            .content
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .filter(|t| !t.trim().is_empty())
            .map(str::to_string)
            .collect();
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    String::new()
}

fn add_usage(dst: &mut TurnUsage, v: &Value) {
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    dst.input += n("input_tokens");
    dst.output += n("output_tokens");
    dst.cache_read += n("cache_read_tokens");
    // Part of the output tokens, the same way OpenAI-style reasoning is counted.
    dst.reasoning += n("thinking_tokens");
}

fn usage_of(v: &Value) -> TurnUsage {
    let mut u = TurnUsage::default();
    if v.is_object() {
        add_usage(&mut u, v);
    }
    u
}

fn step_event(su: &Value) -> Option<StreamEvent> {
    let info = &su["tool_info"];
    let name = info["name"].as_str().or_else(|| su["tool_name"].as_str())?;
    let error = info.get("error").is_some_and(|e| !e.is_null());
    let mut output = info["output"].as_str().unwrap_or("").to_string();
    if error && output.is_empty() {
        output = info["error"]["message"].as_str().unwrap_or("").to_string();
    }
    Some(StreamEvent::Step {
        name: name.to_string(),
        input: if info["parameters"].is_null() {
            Value::Null
        } else {
            info["parameters"].clone()
        },
        output,
        error,
    })
}

/// Turns one NDJSON line of `--output-format stream-json` into stream events.
///
/// Split out from the spawn so the protocol can be tested without a Google
/// login: every mapping below (text deltas, tool steps, usage, terminal status)
/// is a decision worth a test that does not cost a token.
#[derive(Default)]
struct Assembler {
    text: String,
    usage: TurnUsage,
    conv: Option<String>,
    error: Option<String>,
    result_usage: Option<TurnUsage>,
    finished: bool,
}

impl Assembler {
    fn line(&mut self, line: &str) -> Vec<StreamEvent> {
        let line = line.trim();
        if line.is_empty() {
            return vec![];
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            // Unrecognised noise on stdout is data we do not understand, not a
            // reason to lose the turn: the CLI resolves slash commands to text
            // in some paths, and a newer build may add events.
            return vec![];
        };
        let mut out = vec![];
        match v["event"].as_str().unwrap_or("") {
            "init" => {
                // `conversation_id` sits at the top level of the event (the
                // `init` object carries cwd/tools/permission_mode), but accept
                // it from either place so a build that moves it is not a silent
                // memory loss.
                if let Some(c) = v["conversation_id"]
                    .as_str()
                    .or_else(|| v["init"]["conversation_id"].as_str())
                {
                    self.conv = Some(c.to_string());
                }
            }
            "step_update" => {
                let su = &v["step_update"];
                if let Some(c) = su["conversation_id"].as_str() {
                    self.conv = Some(c.to_string());
                }
                let done = su["state"].as_str() != Some("ACTIVE");
                match su["step_type"].as_str().unwrap_or("") {
                    "agent_response" => {
                        if let Some(t) = su["text_delta"].as_str() {
                            if !t.is_empty() {
                                self.text.push_str(t);
                                out.push(StreamEvent::Text(t.to_string()));
                            }
                        }
                    }
                    // Only completed steps: a running one has no result yet, and
                    // the CLI emits its own ACTIVE/DONE pair which would double
                    // every command in the transcript.
                    "tool" if done => {
                        if let Some(e) = step_event(su) {
                            out.push(e);
                        }
                    }
                    _ => {}
                }
                add_usage(&mut self.usage, &su["usage"]);
            }
            "result" => {
                let r = &v["result"];
                if let Some(c) = r["conversation_id"].as_str() {
                    self.conv = Some(c.to_string());
                }
                if r["usage"].is_object() {
                    self.result_usage = Some(usage_of(&r["usage"]));
                }
                if r["status"].as_str() != Some("SUCCESS") {
                    let msg = r["error"]
                        .as_str()
                        .filter(|e| !e.trim().is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            format!(
                                "run ended with status {}",
                                r["status"].as_str().unwrap_or("?")
                            )
                        });
                    self.error = Some(msg);
                }
                // A short answer arrives only here, with no deltas before it.
                if self.text.is_empty() {
                    if let Some(t) = r["response"].as_str() {
                        if !t.is_empty() {
                            self.text.push_str(t);
                            out.push(StreamEvent::Text(t.to_string()));
                        }
                    }
                }
                self.finished = true;
            }
            _ => {}
        }
        out
    }
}

/// Quota and auth read differently to a user, and differently to the router:
/// an exhausted plan should not be retried as if it were a glitch.
fn classify_cli(msg: &str) -> Fail {
    let l = msg.to_lowercase();
    if l.contains("quota")
        || l.contains("exhaust")
        || l.contains("rate limit")
        || l.contains("usage limit")
    {
        Fail::Exhausted {
            msg: msg.to_string(),
            until: 0,
            hard: true,
        }
    } else if l.contains("authentication")
        || l.contains("sign in")
        || l.contains("not authenticated")
        || l.contains("login")
    {
        Fail::Auth(msg.to_string())
    } else {
        Fail::Bad(msg.to_string())
    }
}

/// Run one delegated turn through the vendor's own CLI.
pub async fn run(
    t: &Target,
    req: &ChatRequest,
    on: &mut (dyn FnMut(StreamEvent) + Send),
    cancel: &CancellationToken,
) -> Result<Turn, Fail> {
    let bin = locate().ok_or_else(|| {
        Fail::Auth(format!(
            "The Antigravity CLI (`{BIN}`) is not installed or not on PATH. Install it, run `{BIN}` \
             once to sign in, then try again. OpenLeash never sees your Google credentials — the \
             CLI owns that login."
        ))
    })?;
    if host::is_ssh(&req.cwd) {
        return Err(Fail::Bad(
            "Antigravity runs on this machine, but this chat's project is remote (ssh://…). \
             Use a model provider for remote work, or open a local copy of the project."
                .into(),
        ));
    }
    let prompt = last_user_text(req);
    if prompt.trim().is_empty() {
        return Err(Fail::Bad(
            "Antigravity: the turn had no user text to send.".into(),
        ));
    }
    // A request that names no workspace is one of OpenLeash's own one-shots, not
    // a delegated turn. It gets a scratch directory and the CLI's read-only mode
    // rather than inheriting the app's own working directory with full access.
    let (cwd, full_access) = if req.cwd.is_empty() {
        let scratch = std::env::temp_dir().join("openleash-agy-scratch");
        let _ = std::fs::create_dir_all(&scratch);
        (scratch, false)
    } else {
        (PathBuf::from(&req.cwd), true)
    };
    // `agy -p` accepts user text, not a separate system message. Do not inject
    // OpenLeash's harness instructions as user content: the delegate would treat
    // them as something the user said. The CLI reads project instructions itself.
    let session_key = session_key(&req.cache_key, &cwd, &t.model);
    let prior = conversation(&session_key);

    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(args(&prompt, &t.model, prior.as_deref(), full_access))
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| Fail::Bad(format!("could not start {BIN}: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Fail::Bad(format!("{BIN}: no stdout pipe")))?;

    // Drained on its own task: the CLI writes progress and permission notices to
    // stderr, and a full pipe would block it mid-run with the chat waiting.
    let err_task = child.stderr.take().map(|s| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = s.take(64 * 1024).read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).to_string()
        })
    });

    let mut a = Assembler::default();
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(Fail::Cancel);
            }
            n = lines.next_line() => n,
        };
        match next {
            Ok(Some(line)) => {
                for e in a.line(&line) {
                    on(e);
                }
                if a.finished {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                let _ = child.kill().await;
                return Err(Fail::Bad(format!("{BIN} stream: {e}")));
            }
        }
    }

    // `-p` exits on its own after the result event; a build that lingers must not
    // hold the turn open.
    let code = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait())
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|s| s.code());
    let err_text = match err_task {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    };

    if let Some(c) = &a.conv {
        remember(&session_key, c);
    }
    if let Some(e) = a.error {
        let msg = if err_text.trim().is_empty() {
            e
        } else {
            format!("{e} — {}", first_lines(&err_text, 3))
        };
        return Err(classify_cli(&msg));
    }
    if let Some(code) = code.filter(|c| *c != 0) {
        return Err(classify_cli(&format!(
            "{BIN} exited {code} without a result: {}",
            first_lines(&err_text, 3)
        )));
    }

    // Per-step usage is the honest number for this turn. The result envelope is
    // cumulative over the whole CLI conversation, so using it on a resumed turn
    // would bill this chat for every earlier one — only trust it when the
    // conversation started here.
    let usage = if a.usage.context() > 0 {
        a.usage
    } else if prior.is_none() {
        a.result_usage.unwrap_or_default()
    } else {
        TurnUsage::default()
    };
    let text = a.text.trim_end().to_string();
    if text.is_empty() {
        return Err(Fail::Bad(
            "Antigravity finished without a reply. Its tool steps are above; the run left no text."
                .into(),
        ));
    }
    Ok(Turn {
        content: vec![json!({"type": "text", "text": text})],
        stop_reason: "end_turn".into(),
        usage,
        model: t.model.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(line: &str) -> (Assembler, Vec<StreamEvent>) {
        let mut a = Assembler::default();
        let e = a.line(line);
        (a, e)
    }

    #[test]
    fn streamed_answer_and_usage_are_collected() {
        let (a, e) = events(
            r#"{"event":"init","conversation_id":"c1","init":{"cwd":"/tmp/x","permission_mode":"request-review"}}"#,
        );
        assert_eq!(a.conv.as_deref(), Some("c1"));
        assert!(e.is_empty());

        let (_, e) = events(
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"po"}}"#,
        );
        assert!(matches!(&e[..], [StreamEvent::Text(t)] if t == "po"));

        let (_, e) = events(
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"ng","usage":{"input_tokens":10,"output_tokens":2,"thinking_tokens":1,"cache_read_tokens":3}}}"#,
        );
        assert!(matches!(&e[..], [StreamEvent::Text(t)] if t == "ng"));

        let mut a = Assembler::default();
        for line in [
            r#"{"event":"step_update","step_update":{"step_type":"agent_response","state":"ACTIVE","text_delta":"po"}}"#,
            r#"{"event":"step_update","step_update":{"step_type":"agent_response","state":"DONE","text_delta":"ng","usage":{"input_tokens":10,"output_tokens":2,"thinking_tokens":1,"cache_read_tokens":3}}}"#,
            r#"{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"pong"}}"#,
        ] {
            a.line(line);
        }
        assert_eq!(a.text, "pong");
        assert!(a.finished);
        assert_eq!(a.usage.input, 10);
        assert_eq!(a.usage.output, 2);
        assert_eq!(a.usage.cache_read, 3);
        assert_eq!(a.usage.reasoning, 1);
        assert!(a.error.is_none());
    }

    #[test]
    fn short_answer_arrives_only_on_the_result_event() {
        let mut a = Assembler::default();
        let e = a.line(
            r#"{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"pong\n"}}"#,
        );
        assert!(matches!(&e[..], [StreamEvent::Text(t)] if t == "pong\n"));
        assert_eq!(a.text, "pong\n");
    }

    #[test]
    fn completed_tool_steps_become_their_own_events() {
        let running = Assembler::default();
        let mut a = running;
        let e = a.line(
            r#"{"event":"step_update","step_update":{"step_type":"tool","state":"ACTIVE","tool_name":"run_command"}}"#,
        );
        assert!(e.is_empty(), "a running step has no result to show yet");

        let (_, e) = events(
            r#"{"event":"step_update","step_update":{"step_type":"tool","state":"DONE","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"},"output":"hi\n"}}}"#,
        );
        match &e[..] {
            [StreamEvent::Step {
                name,
                input,
                output,
                error,
            }] => {
                assert_eq!(name, "run_command");
                assert_eq!(input["CommandLine"], "echo hi");
                assert_eq!(output, "hi\n");
                assert!(!error);
            }
            other => panic!("expected one step event, got {}", other.len()),
        }
    }

    #[test]
    fn failed_tool_step_keeps_its_error() {
        let (_, e) = events(
            r#"{"event":"step_update","step_update":{"step_type":"tool","state":"DONE","tool_info":{"name":"run_command","error":{"type":"x","message":"command not found"}}}}"#,
        );
        assert!(
            matches!(&e[..], [StreamEvent::Step { error: true, output, .. }] if output == "command not found")
        );
    }

    #[test]
    fn error_status_surfaces_rather_than_being_swallowed() {
        let mut a = Assembler::default();
        a.line(
            r#"{"event":"result","result":{"status":"ERROR","response":"","error":"invalid model selection"}}"#,
        );
        assert_eq!(a.error.as_deref(), Some("invalid model selection"));
        assert!(classify_cli(a.error.as_deref().unwrap()).is_bad());
    }

    #[test]
    fn quota_and_login_failures_are_classified() {
        assert!(matches!(
            classify_cli("usage limit reached for this plan"),
            Fail::Exhausted { hard: true, .. }
        ));
        assert!(matches!(
            classify_cli("authentication required"),
            Fail::Auth(_)
        ));
        assert!(matches!(classify_cli("something odd"), Fail::Bad(_)));
    }

    /// The provider's posture is a security property, so it is asserted here
    /// rather than left to a reviewer to spot in a spawn call.
    #[test]
    fn full_access_is_explicit_and_resume_is_opt_in() {
        let fresh = args("hi", "gemini-3.8-flash-high", None, true);
        assert!(fresh.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(!fresh.iter().any(|a| a == "--mode"));
        assert!(!fresh.iter().any(|a| a == "--conversation"));
        assert!(fresh
            .windows(2)
            .any(|w| w == ["--model", "gemini-3.8-flash-high"]));
        assert!(fresh.windows(2).any(|w| w[0] == "-p" && w[1] == "hi"));

        let resumed = args("hi", "m", Some("conv-1"), true);
        assert!(resumed
            .windows(2)
            .any(|w| w == ["--conversation", "conv-1"]));
    }

    /// A request with no workspace must never reach full access: it would edit
    /// whichever directory the app happened to be started from.
    #[test]
    fn a_request_without_a_workspace_is_read_only() {
        let one_shot = args("summarise this", "m", None, false);
        assert!(one_shot.windows(2).any(|w| w == ["--mode", "plan"]));
        assert!(!one_shot
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));
    }

    #[test]
    fn model_list_parsing_skips_the_progress_line() {
        let out = "Fetching available models...\n\
                   gemini-3.8-flash-high\tGemini 3.8 Flash (High)\n\
                   claude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)\n";
        let v = providers::parse_agy_models(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "antigravity/gemini-3.8-flash-high");
        assert_eq!(v[0].name, "Gemini 3.8 Flash (High)");
        assert_eq!(v[0].provider, "antigravity");
        assert!(v.iter().all(|m| !m.effort), "effort lives in the slug");
    }

    #[test]
    fn only_the_newest_user_message_is_sent() {
        let req = ChatRequest {
            system: "ignored".into(),
            messages: vec![
                super::super::Message {
                    role: "user".into(),
                    content: vec![json!({"type":"text","text":"first"})],
                    model: String::new(),
                },
                super::super::Message {
                    role: "assistant".into(),
                    content: vec![json!({"type":"text","text":"reply"})],
                    model: String::new(),
                },
                super::super::Message {
                    role: "user".into(),
                    content: vec![
                        json!({"type":"image","source":{}}),
                        json!({"type":"text","text":"second"}),
                    ],
                    model: String::new(),
                },
            ],
            tools: vec![],
            effort: 0,
            max_tokens: 0,
            cache_key: "ol-t1".into(),
            cwd: String::new(),
        };
        assert_eq!(last_user_text(&req), "second");
        assert!(!last_user_text(&req).contains("OpenLeash is driving"));
    }

    #[test]
    fn cli_conversation_is_scoped_to_task_workspace_and_model_version() {
        let cwd = Path::new("C:/work/project");
        assert_eq!(
            session_key("ol-task", cwd, "gemini-flash"),
            "v2:ol-task:C:/work/project:gemini-flash"
        );
        assert_ne!(
            session_key("ol-task", cwd, "gemini-flash"),
            session_key("ol-task", Path::new("C:/work/other"), "gemini-flash")
        );
        assert_ne!(
            session_key("ol-task", cwd, "gemini-flash"),
            session_key("ol-task", cwd, "claude-opus")
        );
    }

    #[test]
    fn a_real_stream_from_the_cli_lands_the_way_the_probe_showed() {
        // Captured verbatim from `agy -p "Reply with exactly: pong"
        // --output-format stream-json` on agy 1.4.7 (tools list shortened), so
        // the parser is pinned to the protocol the installed CLI actually emits
        // rather than to a shape invented from the docs.
        let lines = [
            r#"{"event":"init","conversation_id":"c3f6d6ee","init":{"cwd":"C:\\tmp","tools":["run_command","write_to_file"],"permission_mode":"request-review"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c3f6d6ee","step_index":0,"state":"DONE","step_type":"user_input"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c3f6d6ee","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"p"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c3f6d6ee","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"ong"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c3f6d6ee","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"\n","duration_seconds":1.35,"usage":{"input_tokens":7936,"output_tokens":15,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":7951}}}"#,
            r#"{"event":"result","result":{"conversation_id":"c3f6d6ee","status":"SUCCESS","response":"pong\n","duration_seconds":1.39,"num_turns":1,"usage":{"input_tokens":7936,"output_tokens":15,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":7951}}}"#,
        ];
        let mut a = Assembler::default();
        let mut streamed = String::new();
        for l in lines {
            for e in a.line(l) {
                if let StreamEvent::Text(t) = e {
                    streamed.push_str(&t);
                }
            }
        }
        assert_eq!(streamed, "pong\n");
        assert_eq!(a.text, "pong\n");
        assert_eq!(a.conv.as_deref(), Some("c3f6d6ee"));
        assert!(a.finished && a.error.is_none());
        // Per-step usage, not the cumulative envelope.
        assert_eq!(a.usage.input, 7936);
        assert_eq!(a.usage.output, 15);
        assert_eq!(a.result_usage.unwrap().input, 7936);
    }

    #[test]
    fn conversation_handles_survive_a_restart() {
        let dir = std::env::temp_dir().join("ol-agy-sessions-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("antigravity-sessions.json");
        assert!(load_sessions(&path).is_empty());
        let mut map = HashMap::new();
        map.insert("ol-t1".to_string(), "conv-1".to_string());
        save_sessions(&path, &map);
        assert_eq!(
            load_sessions(&path).get("ol-t1").map(String::as_str),
            Some("conv-1")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_discovery_never_invents_a_path() {
        if let Some(p) = locate() {
            assert!(p.is_file(), "locate() returned a path that is not a file");
        }
    }

    /// A helper so the tests above read as intent, not menu.
    trait IsBad {
        fn is_bad(&self) -> bool;
    }
    impl IsBad for Fail {
        fn is_bad(&self) -> bool {
            matches!(self, Fail::Bad(_))
        }
    }
}
