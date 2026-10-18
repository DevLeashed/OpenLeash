//! Code intelligence through the project's own language servers: the `lsp` tool.
//!
//! One server process per (server id, project root), started on first use,
//! shared by every call that lands on it (calls run in parallel, so requests are
//! multiplexed over one JSON-RPC connection) and reaped when idle.
//!
//! Security notes, because `lsp` can sit behind an always-allow rule:
//! - The server command line is built in, never configurable. A user- or
//!   repo-supplied command would be an execution primitive.
//! - The executable is resolved by scanning `PATH` ourselves, skipping relative
//!   entries and every entry inside the project. A repository that ships its own
//!   `rust-analyzer` (or a `node_modules/.bin/typescript-language-server`) must
//!   not get run just because the model asked for a definition.
//! - rust-analyzer is told not to run build scripts, proc macros or `cargo check`.
//! - Edits a server proposes (`workspace/applyEdit`) are refused, and `didSave`
//!   is never sent.
//!
//! This does not make a language server harmless on an untrusted checkout: the
//! servers themselves read project configuration (`.cargo/config.toml`,
//! `rust-toolchain.toml`, a workspace TypeScript, a Python venv) that can point
//! them at project-controlled programs. Gate the tool on project trust.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio_util::sync::CancellationToken;

/// Largest source file we will open in a server.
const MAX_FILE: u64 = 2 * 1024 * 1024;
/// Output caps: entries listed, characters returned, characters per source line.
const MAX_ENTRIES: usize = 200;
const MAX_CHARS: usize = 30_000;
const LINE_CAP: usize = 160;
const HOVER_CAP: usize = 4_000;
const WS_SYMBOLS_CAP: usize = 100;
/// Documents kept open per server before the least recently used is closed.
const MAX_OPEN_DOCS: usize = 64;
/// Framing limits: a header block or body beyond these is a broken stream.
const MAX_HEADER: usize = 8 * 1024;
const MAX_BODY: usize = 64 * 1024 * 1024;
const CANCELLED: &str = "Interrupted by user.";

// ───────── tool surface ─────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Definition,
    TypeDefinition,
    Implementation,
    References,
    Hover,
    DocumentSymbols,
    WorkspaceSymbols,
    Diagnostics,
    IncomingCalls,
    OutgoingCalls,
}

const OPS: [(&str, Op); 10] = [
    ("definition", Op::Definition),
    ("type_definition", Op::TypeDefinition),
    ("implementation", Op::Implementation),
    ("references", Op::References),
    ("hover", Op::Hover),
    ("document_symbols", Op::DocumentSymbols),
    ("workspace_symbols", Op::WorkspaceSymbols),
    ("diagnostics", Op::Diagnostics),
    ("incoming_calls", Op::IncomingCalls),
    ("outgoing_calls", Op::OutgoingCalls),
];

impl Op {
    pub fn name(self) -> &'static str {
        OPS.iter()
            .find(|(_, o)| *o == self)
            .map(|(n, _)| *n)
            .unwrap_or("")
    }

    fn parse(s: &str) -> Option<Op> {
        OPS.iter().find(|(n, _)| *n == s).map(|(_, o)| *o)
    }

    /// Operations that act on one position in a file (need `line`).
    pub fn needs_position(self) -> bool {
        !matches!(
            self,
            Op::DocumentSymbols | Op::WorkspaceSymbols | Op::Diagnostics
        )
    }
}

fn op_list() -> String {
    OPS.iter()
        .map(|(n, _)| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The tool's JSON schema, in the same shape as `tools::schemas` entries.
pub fn schema() -> Value {
    let ops: Vec<&str> = OPS.iter().map(|(n, _)| *n).collect();
    json!({
        "name": "lsp",
        "description": "Code intelligence from the project's language server (rust-analyzer, typescript-language-server, pyright/basedpyright/pylsp, gopls or clangd — the server must be installed on PATH; copies inside the project are never run). Use it for precise, semantic navigation where text search guesses: where X is defined (`definition`), what type something is (`hover`, `type_definition`), who implements a trait/interface (`implementation`), every real use of a symbol (`references`), who calls X / what X calls (`incoming_calls`/`outgoing_calls`), one file's outline (`document_symbols`), finding a symbol by name across the project (`workspace_symbols`), and compiler errors and warnings for one file (`diagnostics`). Don't use it for plain text search, string literals, comments, docs or config files — use grep. Position operations take `path` + 1-based `line` + `symbol` (the identifier text on that line; more reliable than counting a `character` column). Results are 1-based `path:line:col` followed by the source line. The first call in a project starts the server and may wait for it to index (up to ~45 s); later calls are fast.",
        "input_schema": {"type":"object","properties":{
            "operation":{"type":"string","enum":ops},
            "path":{"type":"string","description":"File to query: absolute, or relative to the working directory. Required for every operation except workspace_symbols"},
            "line":{"type":"integer","description":"1-based line number (position operations)"},
            "character":{"type":"integer","description":"1-based column on that line. Prefer `symbol`; if neither is given, the first non-blank character is used"},
            "symbol":{"type":"string","description":"The identifier on that line to query, e.g. `parse_config`; matched as a whole word, first occurrence"},
            "query":{"type":"string","description":"Symbol name or fragment to search for (workspace_symbols)"},
            "language":{"type":"string","enum":["rust","typescript","python","go","c"],"description":"Which language server to ask for workspace_symbols without a `path` (default: guessed from the project's files)"}},
            "required":["operation"]}
    })
}

#[derive(Clone, Debug)]
pub struct LspCall {
    pub op: Op,
    pub path: Option<PathBuf>,
    pub line: Option<u32>,
    pub character: Option<u32>,
    pub symbol: Option<String>,
    pub query: Option<String>,
    pub language: Option<String>,
}

fn str_field(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// A 1-based integer field. Accepts `12`, `12.0` and `"12"` — models are loose
/// about types — but never 0 or a negative number.
fn uint_field(input: &Value, key: &str) -> Result<Option<u32>, String> {
    let v = match input.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(v) => v,
    };
    let n: Option<i64> = match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        Value::String(s) if s.trim().is_empty() => return Ok(None),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    };
    match n {
        Some(n) if n >= 1 && n <= u32::MAX as i64 => Ok(Some(n as u32)),
        Some(_) => Err(format!(
            "`{key}` is 1-based: the first {} is 1.",
            if key == "line" { "line" } else { "column" }
        )),
        None => Err(format!("`{key}` must be a positive integer.")),
    }
}

/// Validate a tool call. `path` is resolved against `cwd` the same way the file
/// tools resolve it.
pub fn parse_call(input: &Value, cwd: &str) -> Result<LspCall, String> {
    let op_s = input
        .get("operation")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if op_s.is_empty() {
        return Err(format!("`operation` is required: one of {}.", op_list()));
    }
    let op = Op::parse(op_s)
        .ok_or_else(|| format!("Unknown operation `{op_s}`. Use one of {}.", op_list()))?;
    let path = str_field(input, "path").map(|p| super::tools::resolve(cwd, &p));
    let line = uint_field(input, "line")?;
    let character = uint_field(input, "character")?;
    let symbol = match input.get("symbol").and_then(Value::as_str) {
        Some(s) if s.contains(['\n', '\r']) => {
            return Err(
                "`symbol` must be a single identifier from that line, not several lines.".into(),
            )
        }
        Some(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        None => None,
    };
    let query = str_field(input, "query");
    let language = match str_field(input, "language") {
        Some(l) => Some(
            canonical_language(&l)
                .ok_or_else(|| {
                    format!("Unknown language `{l}`. Use rust, typescript, python, go or c.")
                })?
                .to_string(),
        ),
        None => None,
    };
    if op == Op::WorkspaceSymbols {
        if query.is_none() {
            return Err(
                "`query` is required for `workspace_symbols`: the symbol name or part of it."
                    .into(),
            );
        }
    } else if path.is_none() {
        return Err(format!("`path` is required for `{}`.", op.name()));
    }
    if op.needs_position() && line.is_none() {
        return Err(format!(
            "`line` is required for `{}` (1-based), together with `symbol` (the identifier on that line) or `character`.",
            op.name()
        ));
    }
    Ok(LspCall {
        op,
        path,
        line,
        character,
        symbol,
        query,
        language,
    })
}

// ───────── server table ─────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    Rust,
    Ts,
    Python,
    Go,
    C,
}

fn canonical_language(s: &str) -> Option<&'static str> {
    Some(match s.to_ascii_lowercase().as_str() {
        "rust" | "rs" => "rust",
        "typescript" | "ts" | "tsx" | "javascript" | "js" | "jsx" | "node" => "typescript",
        "python" | "py" => "python",
        "go" | "golang" => "go",
        "c" | "cpp" | "c++" | "cxx" | "objc" => "c",
        _ => return None,
    })
}

fn lang_named(s: &str) -> Option<Lang> {
    Some(match canonical_language(s)? {
        "rust" => Lang::Rust,
        "typescript" => Lang::Ts,
        "python" => Lang::Python,
        "go" => Lang::Go,
        _ => Lang::C,
    })
}

/// Language and LSP `languageId` for a file extension (lowercase, no dot).
fn lang_of_ext(ext: &str) -> Option<(Lang, &'static str)> {
    Some(match ext {
        "rs" => (Lang::Rust, "rust"),
        "ts" | "mts" | "cts" => (Lang::Ts, "typescript"),
        "tsx" => (Lang::Ts, "typescriptreact"),
        "js" | "mjs" | "cjs" => (Lang::Ts, "javascript"),
        "jsx" => (Lang::Ts, "javascriptreact"),
        "py" | "pyi" => (Lang::Python, "python"),
        "go" => (Lang::Go, "go"),
        "c" => (Lang::C, "c"),
        "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => (Lang::C, "cpp"),
        _ => return None,
    })
}

fn default_language_id(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => "rust",
        Lang::Ts => "typescript",
        Lang::Python => "python",
        Lang::Go => "go",
        Lang::C => "cpp",
    }
}

struct Spec {
    exe: &'static str,
    args: &'static [&'static str],
    install: &'static str,
}

fn specs(lang: Lang) -> &'static [Spec] {
    match lang {
        Lang::Rust => &[Spec {
            exe: "rust-analyzer",
            args: &[],
            install: "rustup component add rust-analyzer",
        }],
        Lang::Ts => &[Spec {
            exe: "typescript-language-server",
            args: &["--stdio"],
            install: "npm i -g typescript-language-server typescript",
        }],
        Lang::Python => &[
            Spec {
                exe: "pyright-langserver",
                args: &["--stdio"],
                install: "npm i -g pyright",
            },
            Spec {
                exe: "basedpyright-langserver",
                args: &["--stdio"],
                install: "pip install basedpyright",
            },
            Spec {
                exe: "pylsp",
                args: &[],
                install: "pip install python-lsp-server",
            },
        ],
        Lang::Go => &[Spec {
            exe: "gopls",
            args: &[],
            install: "go install golang.org/x/tools/gopls@latest",
        }],
        Lang::C => &[Spec {
            exe: "clangd",
            args: &[],
            install: "install clangd from LLVM (https://clangd.llvm.org/installation)",
        }],
    }
}

fn root_markers(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &["Cargo.toml"],
        Lang::Ts => &["tsconfig.json", "jsconfig.json", "package.json"],
        Lang::Python => &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
        Lang::Go => &["go.work", "go.mod"],
        Lang::C => &["compile_commands.json", "CMakeLists.txt", ".clangd"],
    }
}

/// Which server answers a call, where it is rooted, and how to start it.
///
/// `language_id` is the languageId of *this call's* file (`typescriptreact` for
/// a `.tsx`, …), not a property of the server: one typescript-language-server
/// serves `.ts` and `.tsx` alike, and the pick is recomputed for every call.
#[derive(Clone, Debug)]
pub struct ServerPick {
    pub id: &'static str,
    pub root: PathBuf,
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub language_id: &'static str,
}

fn is_wsl_unc(s: &str) -> bool {
    let s = s.replace('/', "\\").to_ascii_lowercase();
    let s = s
        .strip_prefix(r"\\?\unc\")
        .map(|r| format!(r"\\{r}"))
        .unwrap_or(s);
    s.starts_with(r"\\wsl.localhost\") || s.starts_with(r"\\wsl$\")
}

/// Choose the language server for a call, using the process `PATH`.
pub fn pick_server(cwd: &Path, call: &LspCall) -> Result<ServerPick, String> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    pick_server_with(cwd, call, &path_var)
}

fn pick_server_with(cwd: &Path, call: &LspCall, path_var: &OsStr) -> Result<ServerPick, String> {
    let cwd_s = cwd.to_string_lossy();
    let remote = |s: &str| super::host::is_ssh(s) || is_wsl_unc(s);
    if remote(&cwd_s)
        || call
            .path
            .as_ref()
            .is_some_and(|p| remote(&p.to_string_lossy()))
    {
        return Err("The lsp tool is not supported for remote (SSH) or WSL projects yet; use grep and read_file instead.".into());
    }
    let cwd_n = norm(cwd);
    let (lang, language_id, root) = match &call.path {
        Some(p) => {
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let (lang, lid) = lang_of_ext(&ext).ok_or_else(|| {
                format!(
                    "No language server is built in for {} files. Supported: Rust (.rs), TypeScript/JavaScript (.ts/.tsx/.js/.jsx/.mjs/.cjs), Python (.py), Go (.go) and C/C++ (.c/.h/.cpp/...). Use grep for anything else.",
                    if ext.is_empty() { "extensionless".to_string() } else { format!("`.{ext}`") }
                )
            })?;
            let dir = p.parent().map(norm).unwrap_or_else(|| cwd_n.clone());
            (lang, lid, find_root(lang, &dir, &cwd_n))
        }
        None => {
            let lang = match call.language.as_deref() {
                Some(l) => lang_named(l).ok_or_else(|| format!("Unknown language `{l}`."))?,
                None => guess_lang(&cwd_n).ok_or_else(|| {
                    "Couldn't tell which language server to ask. Pass `language` (rust, typescript, python, go or c) or a `path` in the project.".to_string()
                })?,
            };
            (
                lang,
                default_language_id(lang),
                workspace_root(lang, &cwd_n),
            )
        }
    };
    let options = specs(lang);
    for spec in options {
        if let Some(exe) = find_exe_in(spec.exe, path_var, &cwd_n) {
            return Ok(ServerPick {
                id: spec.exe,
                root,
                exe,
                args: spec.args.iter().map(|a| a.to_string()).collect(),
                language_id,
            });
        }
    }
    let how: Vec<String> = options
        .iter()
        .map(|s| format!("`{}` ({})", s.exe, s.install))
        .collect();
    Err(format!(
        "No language server found on PATH for this file. Looked for {}. Copies inside the project (e.g. node_modules/.bin) are deliberately ignored. Install one and retry, or use grep/read_file instead.",
        how.join(", or ")
    ))
}

fn has_marker(dir: &Path, lang: Lang) -> bool {
    root_markers(lang).iter().any(|m| dir.join(m).exists())
}

/// Directories from `dir` up to and including `cwd`, nearest first. Empty when
/// `dir` is not inside `cwd`: root search never climbs above the project.
fn ancestors_within(dir: &Path, cwd: &Path) -> Vec<PathBuf> {
    if !is_within(dir, cwd) {
        return Vec::new();
    }
    dir.ancestors()
        .take_while(|a| is_within(a, cwd))
        .map(Path::to_path_buf)
        .collect()
}

fn find_root(lang: Lang, dir: &Path, cwd: &Path) -> PathBuf {
    let chain = ancestors_within(dir, cwd);
    let hit = if lang == Lang::Rust {
        // The topmost Cargo.toml is the workspace root: rooting rust-analyzer at
        // a member crate would start a second server per crate.
        chain.iter().rev().find(|d| has_marker(d, lang))
    } else {
        chain.iter().find(|d| has_marker(d, lang))
    };
    hit.cloned().unwrap_or_else(|| workspace_root(lang, cwd))
}

/// Immediate subdirectories worth probing for project markers, sorted.
fn probe_subdirs(cwd: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(cwd)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            !n.starts_with('.')
                && !matches!(n.as_str(), "node_modules" | "target" | "dist" | "build")
        })
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// Root for a call without a file inside a project root: the project itself
/// when it has a marker, else the first immediate subdirectory that does (an
/// app with its Rust crate in `src-tauri/`), else the project.
fn workspace_root(lang: Lang, cwd: &Path) -> PathBuf {
    if has_marker(cwd, lang) {
        return cwd.to_path_buf();
    }
    probe_subdirs(cwd)
        .into_iter()
        .find(|d| has_marker(d, lang))
        .map(|d| norm(&d))
        .unwrap_or_else(|| cwd.to_path_buf())
}

fn guess_lang(cwd: &Path) -> Option<Lang> {
    let order = [Lang::Rust, Lang::Go, Lang::Ts, Lang::Python, Lang::C];
    if let Some(l) = order.iter().find(|l| has_marker(cwd, **l)) {
        return Some(*l);
    }
    let subs = probe_subdirs(cwd);
    order
        .iter()
        .find(|l| subs.iter().any(|d| has_marker(d, **l)))
        .copied()
}

// ───────── executable lookup ─────────

fn exe_candidates(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ]
    } else {
        vec![name.to_string()]
    }
}

fn is_executable(p: &Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Find `name` on an explicit `PATH` value, never inside `project`.
///
/// Relative entries (`.`, `node_modules/.bin`) are skipped because they resolve
/// against whatever the current directory is; absolute entries inside the
/// project are skipped because the project is untrusted input. The resolved
/// file is checked again after following links, so `~/bin/rust-analyzer ->
/// <project>/evil` is refused too. The path returned is the PATH spelling, not
/// the link target: rustup proxies dispatch on their own file name.
pub(crate) fn find_exe_in(name: &str, path_var: &OsStr, project: &Path) -> Option<PathBuf> {
    let project = norm(project);
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        if is_within(&norm(&dir), &project) {
            continue;
        }
        for cand in exe_candidates(name) {
            let p = dir.join(&cand);
            if is_executable(&p) && !is_within(&norm(&p), &project) {
                return Some(p);
            }
        }
    }
    None
}

// ───────── paths, URIs, columns ─────────

/// Drop Windows' verbatim prefix (`\\?\C:\x` → `C:\x`, `\\?\UNC\h\s` → `\\h\s`).
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

/// Lexically resolve `.` and `..`, for paths that don't exist (yet).
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonical form used for every comparison and every URI we send.
fn norm(p: &Path) -> PathBuf {
    strip_verbatim(&std::fs::canonicalize(p).unwrap_or_else(|_| lexical(p)))
}

fn comp_eq(a: &Component, b: &Component) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase()
            == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// `p` relative to `base`, comparing component-wise (case-insensitively on
/// Windows). Both should already be normalized.
fn rel_to(p: &Path, base: &Path) -> Option<PathBuf> {
    let mut pc = p.components();
    for b in base.components() {
        match pc.next() {
            Some(c) if comp_eq(&c, &b) => {}
            _ => return None,
        }
    }
    Some(pc.as_path().to_path_buf())
}

fn is_within(p: &Path, base: &Path) -> bool {
    rel_to(p, base).is_some()
}

/// `file://` URI for an absolute path.
pub(crate) fn path_to_uri(p: &Path) -> String {
    let s = strip_verbatim(p).to_string_lossy().to_string();
    let enc = |seg: &str| urlencoding::encode(seg).into_owned();
    if cfg!(windows) {
        let s = s.replace('\\', "/");
        if let Some(unc) = s.strip_prefix("//") {
            let mut parts = unc.splitn(2, '/');
            let host = parts.next().unwrap_or("");
            let rest = parts.next().unwrap_or("");
            let path: Vec<String> = rest.split('/').map(enc).collect();
            return format!("file://{host}/{}", path.join("/"));
        }
        let segs: Vec<String> = s
            .split('/')
            .enumerate()
            .map(|(i, seg)| {
                if i == 0 && seg.len() == 2 && seg.ends_with(':') {
                    seg.to_ascii_uppercase()
                } else {
                    enc(seg)
                }
            })
            .collect();
        format!("file:///{}", segs.join("/"))
    } else {
        let segs: Vec<String> = s.split('/').map(enc).collect();
        format!("file://{}", segs.join("/"))
    }
}

/// Local path for a `file://` URI; `None` for other schemes.
pub(crate) fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("file://"))
        .map(|_| &uri[7..])?;
    let (auth, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = urlencoding::decode(path).ok()?.into_owned();
    let auth = urlencoding::decode(auth).ok()?.into_owned();
    let local = auth.is_empty() || auth.eq_ignore_ascii_case("localhost");
    if cfg!(windows) {
        if !local {
            return Some(PathBuf::from(format!(
                r"\\{auth}{}",
                path.replace('/', "\\")
            )));
        }
        let b = path.as_bytes();
        let p = if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
            format!("{}{}", (b[1] as char).to_ascii_uppercase(), &path[2..])
        } else {
            path
        };
        Some(PathBuf::from(p.replace('/', "\\")))
    } else if local {
        Some(PathBuf::from(path))
    } else {
        None
    }
}

/// Comparison key for a URI: two spellings of one file map to one key.
fn uri_key(uri: &str) -> String {
    match uri_to_path(uri) {
        Some(p) if cfg!(windows) => p.to_string_lossy().to_lowercase(),
        Some(p) => p.to_string_lossy().to_string(),
        None => uri.to_string(),
    }
}

/// 1-based character column → 0-based UTF-16 offset (clamped to the line end).
fn char_col_to_utf16(line: &str, col1: u32) -> u32 {
    line.chars()
        .take(col1.saturating_sub(1) as usize)
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// 0-based UTF-16 offset → 1-based character column. An offset in the middle
/// of a surrogate pair counts as that character.
fn utf16_to_char_col(line: &str, off: u32) -> u32 {
    let mut units = 0u32;
    let mut col = 1u32;
    for c in line.chars() {
        if units >= off {
            break;
        }
        units += c.len_utf16() as u32;
        if units > off {
            break;
        }
        col += 1;
    }
    col
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Byte offset of the first whole-word occurrence of `sym` in `line`.
/// A side of `sym` that is not an identifier character (`->`, `::new`) needs no
/// boundary there.
fn find_symbol(line: &str, sym: &str) -> Option<usize> {
    let first = sym.chars().next()?;
    let last = sym.chars().next_back()?;
    line.match_indices(sym).map(|(i, _)| i).find(|&i| {
        let before = line[..i].chars().next_back();
        let after = line[i + sym.len()..].chars().next();
        let left_ok = !is_ident(first) || !before.is_some_and(is_ident);
        let right_ok = !is_ident(last) || !after.is_some_and(is_ident);
        left_ok && right_ok
    })
}

/// The identifier touching a 1-based char column, for messages.
fn word_at(line: &str, col1: u32) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let i = (col1.saturating_sub(1) as usize).min(chars.len());
    let mut s = i;
    while s > 0 && is_ident(chars[s - 1]) {
        s -= 1;
    }
    let mut e = i;
    while e < chars.len() && is_ident(chars[e]) {
        e += 1;
    }
    (e > s).then(|| chars[s..e].iter().collect())
}

fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n').map(|l| l.trim_end_matches('\r')).collect()
}

/// A resolved position in the call's file.
#[derive(Debug)]
struct Pos {
    line0: u32,
    utf16: u32,
    col1: u32,
    /// What the user pointed at, for "no definition found for `x`" messages.
    subject: Option<String>,
}

fn resolve_position(call: &LspCall, text: &str, shown: &str) -> Result<Pos, String> {
    let line1 = call.line.unwrap_or(1);
    let lines = split_lines(text);
    let total = if text.ends_with('\n') {
        lines.len() - 1
    } else {
        lines.len()
    };
    let Some(lt) = lines
        .get(line1 as usize - 1)
        .filter(|_| line1 as usize <= total.max(1))
    else {
        return Err(format!(
            "Line {line1} is past the end of {shown} ({total} lines)."
        ));
    };
    if let Some(sym) = &call.symbol {
        let Some(b) = find_symbol(lt, sym) else {
            return Err(format!(
                "`{sym}` does not appear as a whole word on line {line1} of {shown}. That line is: {}",
                cap_chars(lt.trim(), LINE_CAP)
            ));
        };
        return Ok(Pos {
            line0: line1 - 1,
            utf16: lt[..b].encode_utf16().count() as u32,
            col1: lt[..b].chars().count() as u32 + 1,
            subject: Some(sym.clone()),
        });
    }
    let col1 = match call.character {
        Some(c) => c.min(lt.chars().count() as u32 + 1),
        None => lt.chars().position(|c| !c.is_whitespace()).unwrap_or(0) as u32 + 1,
    };
    Ok(Pos {
        line0: line1 - 1,
        utf16: char_col_to_utf16(lt, col1),
        col1,
        subject: word_at(lt, col1),
    })
}

fn cap_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

// ───────── framing ─────────

fn encode(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Take one complete message body off the front of `buf`, if one is there.
/// Headers are matched case-insensitively and unknown ones (Content-Type) ignored.
fn take_frame(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        if buf.len() > MAX_HEADER {
            return Err("language server sent an oversized header".into());
        }
        return Ok(None);
    };
    let head = std::str::from_utf8(&buf[..end])
        .map_err(|_| "language server sent a non-UTF-8 header".to_string())?;
    let mut len = None;
    for line in head.split("\r\n") {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = Some(
                    v.trim()
                        .parse::<usize>()
                        .map_err(|_| format!("bad Content-Length `{}`", v.trim()))?,
                );
            }
        }
    }
    let len = len.ok_or("language server sent a message without Content-Length")?;
    if len > MAX_BODY {
        return Err(format!("language server sent a {len}-byte message"));
    }
    let start = end + 4;
    if buf.len() < start + len {
        return Ok(None);
    }
    let body = buf[start..start + len].to_vec();
    buf.drain(..start + len);
    Ok(Some(body))
}

struct FrameReader<R> {
    r: R,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    fn new(r: R) -> Self {
        Self { r, buf: Vec::new() }
    }

    /// Next message; `Ok(None)` at end of stream. A body that isn't JSON is
    /// skipped — the framing is still intact, so the stream is usable.
    async fn next(&mut self) -> Result<Option<Value>, String> {
        let mut chunk = vec![0u8; 16 * 1024];
        loop {
            while let Some(body) = take_frame(&mut self.buf)? {
                if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                    return Ok(Some(v));
                }
            }
            let n = self.r.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok(None);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

// ───────── JSON-RPC client ─────────

type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;

#[derive(Debug)]
struct RpcErr {
    code: i64,
    message: String,
}

#[derive(Debug)]
enum ReqErr {
    Rpc(RpcErr),
    Timeout,
    Cancelled,
    Dead,
}

struct Progress {
    active: HashSet<String>,
    idle_since: Instant,
    seen: bool,
}

/// State the reader task updates and request callers read.
struct Shared {
    pending: StdMutex<HashMap<i64, oneshot::Sender<Result<Value, RpcErr>>>>,
    dead: AtomicBool,
    progress: StdMutex<Progress>,
    progress_bell: Notify,
    /// Latest published diagnostics per `uri_key`: (sequence, when, diagnostics).
    diags: StdMutex<HashMap<String, (u64, Instant, Value)>>,
    diag_seq: AtomicU64,
    diag_bell: Notify,
    /// Answers for `workspace/configuration`, by dotted section.
    config: Value,
    folders: Value,
}

impl Shared {
    fn die(&self) {
        self.dead.store(true, Ordering::SeqCst);
        // Dropping the senders wakes every waiter with "server exited".
        self.pending.lock().unwrap().clear();
        self.progress_bell.notify_waiters();
        self.diag_bell.notify_waiters();
    }

    fn progress_event(&self, token: String, busy: bool) {
        let mut p = self.progress.lock().unwrap();
        p.seen = true;
        if busy {
            p.active.insert(token);
        } else if p.active.remove(&token) && p.active.is_empty() {
            p.idle_since = Instant::now();
        }
        drop(p);
        self.progress_bell.notify_waiters();
    }
}

fn lookup_section(config: &Value, section: &str) -> Value {
    let mut v = config;
    for part in section.split('.') {
        match v.get(part) {
            Some(x) => v = x,
            None => return Value::Null,
        }
    }
    v.clone()
}

/// Answer a request the server sent us.
fn server_request(shared: &Shared, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "workspace/configuration" => Ok(Value::Array(
            params["items"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|it| match it["section"].as_str() {
                            Some(s) => lookup_section(&shared.config, s),
                            None => Value::Null,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )),
        "workspace/workspaceFolders" => Ok(shared.folders.clone()),
        "workspace/applyEdit" => Ok(json!({
            "applied": false,
            "failureReason": "this client never applies edits from a language server"
        })),
        "window/showDocument" => Ok(json!({"success": false})),
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "window/showMessageRequest"
        | "workspace/semanticTokens/refresh"
        | "workspace/inlayHint/refresh"
        | "workspace/inlineValue/refresh"
        | "workspace/codeLens/refresh"
        | "workspace/diagnostic/refresh"
        | "workspace/foldingRange/refresh" => Ok(Value::Null),
        _ => Err((-32601, format!("Method not found: {method}"))),
    }
}

fn notification(shared: &Shared, method: &str, params: &Value) {
    match method {
        "$/progress" => {
            let token = match &params["token"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            match params["value"]["kind"].as_str() {
                Some("begin") => shared.progress_event(token, true),
                Some("end") => shared.progress_event(token, false),
                _ => {}
            }
        }
        // rust-analyzer's own readiness signal (opted into in `initialize`).
        "experimental/serverStatus" => {
            let quiet = params["quiescent"].as_bool().unwrap_or(true);
            shared.progress_event("\u{0}serverStatus".into(), !quiet);
        }
        "textDocument/publishDiagnostics" => {
            if let Some(uri) = params["uri"].as_str() {
                let seq = shared.diag_seq.fetch_add(1, Ordering::SeqCst) + 1;
                let d = params.get("diagnostics").cloned().unwrap_or(json!([]));
                shared
                    .diags
                    .lock()
                    .unwrap()
                    .insert(uri_key(uri), (seq, Instant::now(), d));
                shared.diag_bell.notify_waiters();
            }
        }
        _ => {}
    }
}

fn handle_message(shared: &Shared, out: &mpsc::UnboundedSender<Vec<u8>>, msg: Value) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").filter(|v| !v.is_null());
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match (method, id) {
        (None, Some(id)) => {
            let Some(n) = id.as_i64() else { return };
            let Some(tx) = shared.pending.lock().unwrap().remove(&n) else {
                return;
            };
            let r = match msg.get("error").filter(|e| !e.is_null()) {
                Some(e) => Err(RpcErr {
                    code: e["code"].as_i64().unwrap_or(0),
                    message: e["message"].as_str().unwrap_or("").to_string(),
                }),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(r);
        }
        (Some(m), Some(id)) => {
            let reply = match server_request(shared, m, &params) {
                Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
                Err((code, message)) => {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
                }
            };
            let _ = out.send(encode(&reply));
        }
        (Some(m), None) => notification(shared, m, &params),
        _ => {}
    }
}

/// One JSON-RPC connection. Writes go through a channel to a writer task so a
/// frame is never interleaved with another; a reader task routes replies by id.
struct Client {
    out: mpsc::UnboundedSender<Vec<u8>>,
    next_id: AtomicI64,
    shared: Arc<Shared>,
}

impl Client {
    fn start(read: BoxRead, write: BoxWrite, config: Value, folders: Value) -> Client {
        let shared = Arc::new(Shared {
            pending: Default::default(),
            dead: AtomicBool::new(false),
            progress: StdMutex::new(Progress {
                active: HashSet::new(),
                idle_since: Instant::now(),
                seen: false,
            }),
            progress_bell: Notify::new(),
            diags: Default::default(),
            diag_seq: AtomicU64::new(0),
            diag_bell: Notify::new(),
            config,
            folders,
        });
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let ws = shared.clone();
        tokio::spawn(async move {
            let mut w = write;
            while let Some(b) = rx.recv().await {
                if w.write_all(&b).await.is_err() || w.flush().await.is_err() {
                    ws.die();
                    break;
                }
            }
        });
        let rs = shared.clone();
        let rtx = tx.clone();
        tokio::spawn(async move {
            let mut fr = FrameReader::new(read);
            while let Ok(Some(msg)) = fr.next().await {
                handle_message(&rs, &rtx, msg);
            }
            rs.die();
        });
        Client {
            out: tx,
            next_id: AtomicI64::new(1),
            shared,
        }
    }

    fn dead(&self) -> bool {
        self.shared.dead.load(Ordering::SeqCst)
    }

    fn notify(&self, method: &str, params: Value) {
        let mut m = json!({"jsonrpc":"2.0","method":method});
        if !params.is_null() {
            m["params"] = params;
        }
        let _ = self.out.send(encode(&m));
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> Result<Value, ReqErr> {
        if self.dead() {
            return Err(ReqErr::Dead);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(id, tx);
        // Re-checked after inserting: a death in between already drained the
        // map, and our sender would otherwise wait out the whole timeout.
        if self.dead() {
            self.shared.pending.lock().unwrap().remove(&id);
            return Err(ReqErr::Dead);
        }
        let mut m = json!({"jsonrpc":"2.0","id":id,"method":method});
        if !params.is_null() {
            m["params"] = params;
        }
        if self.out.send(encode(&m)).is_err() {
            self.shared.pending.lock().unwrap().remove(&id);
            return Err(ReqErr::Dead);
        }
        tokio::select! {
            r = rx => match r {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(ReqErr::Rpc(e)),
                Err(_) => Err(ReqErr::Dead),
            },
            _ = tokio::time::sleep(timeout) => {
                self.abandon(id);
                Err(ReqErr::Timeout)
            }
            _ = cancel.cancelled() => {
                self.abandon(id);
                Err(ReqErr::Cancelled)
            }
        }
    }

    /// Stop waiting for `id` and tell the server it can drop the work.
    fn abandon(&self, id: i64) {
        self.shared.pending.lock().unwrap().remove(&id);
        self.notify("$/cancelRequest", json!({"id": id}));
    }

    fn progress(&self) -> (usize, Instant, bool) {
        let p = self.shared.progress.lock().unwrap();
        (p.active.len(), p.idle_since, p.seen)
    }

    fn diag(&self, key: &str) -> Option<(u64, Instant, Value)> {
        self.shared.diags.lock().unwrap().get(key).cloned()
    }
}

// ───────── server process ─────────

/// A transport plus, for a real process, the child that owns it.
struct Launched {
    read: BoxRead,
    write: BoxWrite,
    child: Option<tokio::process::Child>,
    stderr: Arc<StdMutex<String>>,
}

/// Keep the last few KB of a server's stderr for error messages, and keep the
/// pipe drained so a chatty server never blocks on it.
fn drain_stderr(mut err: impl AsyncRead + Unpin + Send + 'static, tail: Arc<StdMutex<String>>) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        while let Ok(n) = err.read(&mut buf).await {
            if n == 0 {
                break;
            }
            let mut t = tail.lock().unwrap();
            t.push_str(&String::from_utf8_lossy(&buf[..n]));
            if t.len() > 4096 {
                let cut = t.len() - 2048;
                let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(0);
                t.drain(..cut);
            }
        }
    });
}

fn spawn_server(pick: &ServerPick) -> Result<Launched, String> {
    let mut c = tokio::process::Command::new(&pick.exe);
    // A `.cmd` shim (npm's global installs on Windows) is run through cmd.exe
    // by std itself, with its arguments escaped; ours are fixed literals anyway.
    //
    // The working directory is the server's own folder, *not* the project:
    // servers find the project through rootUri. In the project, the shim's bare
    // `node` would be looked up in the current directory first by cmd.exe, so a
    // repo shipping `node.cmd` would run; a rustup proxy would also obey the
    // project's `rust-toolchain.toml` (which can name a custom toolchain path)
    // when picking the rust-analyzer binary itself.
    let neutral = pick
        .exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    c.args(&pick.args)
        .current_dir(neutral)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // Same hazard one level down: commands the server (or its shim) runs
        // by bare name must not be resolved from whatever directory it is in.
        c.env("NoDefaultCurrentDirectoryInExePath", "1");
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = c
        .spawn()
        .map_err(|e| format!("Couldn't start {} ({}): {e}", pick.id, pick.exe.display()))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        super::shell::kill_tree(&mut child);
        return Err(format!("Couldn't open pipes to {}.", pick.id));
    };
    let stderr: Arc<StdMutex<String>> = Default::default();
    if let Some(err) = child.stderr.take() {
        drain_stderr(err, stderr.clone());
    }
    Ok(Launched {
        read: Box::new(stdout),
        write: Box::new(stdin),
        child: Some(child),
        stderr,
    })
}

/// Settings that keep rust-analyzer from executing project code: no build
/// scripts, no proc macros, no `cargo check`. Sent as initializationOptions and
/// again for every `workspace/configuration` pull of the `rust-analyzer` section.
fn rust_analyzer_settings() -> Value {
    json!({
        "cargo": {"buildScripts": {"enable": false}},
        "procMacro": {"enable": false, "attributes": {"enable": false}},
        "checkOnSave": false,
        "cachePriming": {"enable": false},
        // We never send didChangeWatchedFiles, so let the server watch the disk
        // itself — otherwise edits the agent makes are invisible to it.
        "files": {"watcher": "server"}
    })
}

/// typescript-language-server prefers the project's own `node_modules/typescript`
/// — project code. Point it at the TypeScript installed next to the global
/// server instead, when there is one.
fn global_tsserver(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let cands = [
        dir.join("node_modules/typescript/lib/tsserver.js"),
        dir.join("../lib/node_modules/typescript/lib/tsserver.js"),
    ];
    cands.into_iter().find(|p| p.is_file()).map(|p| norm(&p))
}

fn init_options(pick: &ServerPick) -> Value {
    match pick.id {
        "rust-analyzer" => rust_analyzer_settings(),
        "typescript-language-server" => match global_tsserver(&pick.exe) {
            Some(p) => json!({"tsserver": {"path": p.to_string_lossy()}}),
            None => json!({}),
        },
        _ => json!({}),
    }
}

fn client_config(pick: &ServerPick) -> Value {
    match pick.id {
        "rust-analyzer" => json!({"rust-analyzer": rust_analyzer_settings()}),
        _ => json!({}),
    }
}

fn initialize_params(pick: &ServerPick) -> Value {
    let kinds: Vec<u32> = (1..=26).collect();
    let uri = path_to_uri(&pick.root);
    let name = pick
        .root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".into());
    json!({
        "processId": std::process::id(),
        "clientInfo": {"name": "OpenLeash"},
        "rootUri": uri,
        "rootPath": pick.root.to_string_lossy(),
        "workspaceFolders": [{"uri": uri, "name": name}],
        "initializationOptions": init_options(pick),
        "capabilities": {
            "general": {"positionEncodings": ["utf-16"]},
            "window": {"workDoneProgress": true},
            "workspace": {
                "configuration": true,
                "workspaceFolders": true,
                "applyEdit": false,
                "symbol": {"symbolKind": {"valueSet": kinds}}
            },
            "textDocument": {
                "synchronization": {"dynamicRegistration": false, "didSave": false, "willSave": false},
                "hover": {"contentFormat": ["markdown", "plaintext"]},
                "definition": {"linkSupport": true},
                "typeDefinition": {"linkSupport": true},
                "implementation": {"linkSupport": true},
                "references": {},
                "documentSymbol": {
                    "hierarchicalDocumentSymbolSupport": true,
                    "symbolKind": {"valueSet": kinds}
                },
                "publishDiagnostics": {"relatedInformation": false, "versionSupport": true},
                "diagnostic": {"dynamicRegistration": false},
                "callHierarchy": {"dynamicRegistration": false}
            },
            "experimental": {"serverStatusNotification": true}
        }
    })
}

struct DocState {
    path: PathBuf,
    version: i32,
    hash: String,
    /// `diag_seq` when we last sent this text: diagnostics newer than this
    /// describe it.
    mark: u64,
    stamp: Option<(SystemTime, u64)>,
    used: u64,
}

fn file_stamp(p: &Path) -> Option<(SystemTime, u64)> {
    let md = std::fs::metadata(p).ok()?;
    Some((md.modified().ok()?, md.len()))
}

struct Server {
    id: &'static str,
    client: Client,
    caps: Value,
    child: StdMutex<Option<tokio::process::Child>>,
    stderr: Arc<StdMutex<String>>,
    docs: Mutex<HashMap<String, DocState>>,
    doc_tick: AtomicU64,
    last_used: StdMutex<Instant>,
    inflight: AtomicUsize,
    warmed: AtomicBool,
    started: Instant,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            super::shell::kill_tree(&mut c);
        }
    }
}

impl Server {
    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    fn stderr_tail(&self) -> String {
        let t = self.stderr.lock().unwrap();
        let t = t.trim();
        if t.is_empty() {
            String::new()
        } else {
            format!(" Its last output: {}", cap_chars(t, 600))
        }
    }

    fn describe(&self, method: &str, e: ReqErr, timeout: Duration) -> String {
        match e {
            ReqErr::Cancelled => CANCELLED.into(),
            ReqErr::Timeout => format!(
                "{} didn't answer `{method}` within {} s.",
                self.id,
                timeout.as_secs()
            ),
            ReqErr::Dead => format!(
                "{} exited unexpectedly.{} The next lsp call restarts it.",
                self.id,
                self.stderr_tail()
            ),
            ReqErr::Rpc(e) if e.code == -32601 => {
                format!("{} doesn't support `{method}`.", self.id)
            }
            ReqErr::Rpc(e) => format!(
                "{} returned an error for `{method}`: {} (code {}).",
                self.id, e.message, e.code
            ),
        }
    }

    /// Bring the server's copy of `uri` up to date with `text`, and every other
    /// open document up to date with the disk. Returns the diagnostics mark.
    async fn sync(&self, path: &Path, uri: &str, text: &str, language_id: &str) -> u64 {
        let mut docs = self.docs.lock().await;
        let tick = self.doc_tick.fetch_add(1, Ordering::SeqCst);
        self.refresh_others(&mut docs, uri);
        let hash = super::tools::content_hash(text.as_bytes());
        let mark = self.client.shared.diag_seq.load(Ordering::SeqCst);
        match docs.get_mut(uri) {
            Some(d) if d.hash == hash => {
                d.used = tick;
                d.stamp = file_stamp(path);
                d.mark
            }
            Some(d) => {
                d.version += 1;
                d.hash = hash;
                d.mark = mark;
                d.used = tick;
                d.stamp = file_stamp(path);
                self.client.notify(
                    "textDocument/didChange",
                    json!({"textDocument": {"uri": uri, "version": d.version},
                           "contentChanges": [{"text": text}]}),
                );
                mark
            }
            None => {
                if docs.len() >= MAX_OPEN_DOCS {
                    if let Some(old) = docs
                        .iter()
                        .min_by_key(|(_, d)| d.used)
                        .map(|(u, _)| u.clone())
                    {
                        docs.remove(&old);
                        self.client.notify(
                            "textDocument/didClose",
                            json!({"textDocument": {"uri": old}}),
                        );
                    }
                }
                self.client.notify(
                    "textDocument/didOpen",
                    json!({"textDocument": {"uri": uri, "languageId": language_id, "version": 1, "text": text}}),
                );
                docs.insert(
                    uri.to_string(),
                    DocState {
                        path: path.to_path_buf(),
                        version: 1,
                        hash,
                        mark,
                        stamp: file_stamp(path),
                        used: tick,
                    },
                );
                mark
            }
        }
    }

    /// An open document is the client's to keep current: the server stops
    /// reading it from disk. So when the agent edits a file opened by an
    /// earlier call, push the new text (or close it if it's gone).
    fn refresh_others(&self, docs: &mut HashMap<String, DocState>, except: &str) {
        let mut gone = Vec::new();
        for (uri, d) in docs.iter_mut().filter(|(u, _)| u.as_str() != except) {
            let stamp = file_stamp(&d.path);
            if stamp == d.stamp {
                continue;
            }
            d.stamp = stamp;
            let text = match stamp {
                Some((_, len)) if len <= MAX_FILE => std::fs::read(&d.path)
                    .ok()
                    .map(|b| String::from_utf8_lossy(&b).into_owned()),
                _ => None,
            };
            let Some(text) = text else {
                gone.push(uri.clone());
                continue;
            };
            let hash = super::tools::content_hash(text.as_bytes());
            if hash != d.hash {
                d.version += 1;
                d.hash = hash;
                d.mark = self.client.shared.diag_seq.load(Ordering::SeqCst);
                self.client.notify(
                    "textDocument/didChange",
                    json!({"textDocument": {"uri": uri, "version": d.version},
                           "contentChanges": [{"text": text}]}),
                );
            }
        }
        for uri in gone {
            docs.remove(&uri);
            self.client.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": uri}}),
            );
        }
    }

    /// Polite shutdown, then make sure the whole process tree is gone.
    async fn shutdown(&self) {
        if !self.client.dead() {
            let c = CancellationToken::new();
            let _ = self
                .client
                .request("shutdown", Value::Null, &c, Duration::from_secs(2))
                .await;
            self.client.notify("exit", Value::Null);
        }
        let child = self.child.lock().unwrap().take();
        if let Some(mut ch) = child {
            // Only kill if it didn't exit: once waited on, its pid may belong
            // to someone else (tokio then reports no id, so kill_tree is inert).
            if tokio::time::timeout(Duration::from_millis(1500), ch.wait())
                .await
                .is_err()
            {
                super::shell::kill_tree(&mut ch);
            }
        }
    }
}

/// Keeps a server marked busy (not reapable) for the duration of a call.
struct Busy<'a>(&'a Server);

impl<'a> Busy<'a> {
    fn new(s: &'a Server) -> Self {
        s.inflight.fetch_add(1, Ordering::SeqCst);
        s.touch();
        Busy(s)
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.touch();
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

// ───────── manager ─────────

#[derive(Clone, Copy, Debug)]
struct Timing {
    request: Duration,
    init: Duration,
    /// Longest wait for indexing on a server's first call, and on later ones.
    first_index: Duration,
    later_index: Duration,
    /// How long progress must have been quiet before we call the server idle.
    settle: Duration,
    /// On first use, how long to give a server to *start* reporting progress.
    warmup: Duration,
    diag_wait: Duration,
    diag_settle: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            request: Duration::from_secs(60),
            init: Duration::from_secs(60),
            first_index: Duration::from_secs(45),
            later_index: Duration::from_secs(10),
            settle: Duration::from_millis(300),
            warmup: Duration::from_millis(1000),
            diag_wait: Duration::from_secs(15),
            diag_settle: Duration::from_secs(1),
        }
    }
}

#[derive(Default)]
struct Slot {
    srv: Mutex<Option<Arc<Server>>>,
}

type Key = (String, PathBuf);
type Launcher<'a> = &'a (dyn Fn(&ServerPick) -> Result<Launched, String> + Send + Sync);

/// Running language servers, keyed by (server id, root).
#[derive(Default)]
pub struct LspManager {
    slots: Mutex<HashMap<Key, Arc<Slot>>>,
    timing: Timing,
}

impl LspManager {
    /// Answer one tool call, starting the server if needed.
    pub async fn run(
        &self,
        cwd: &Path,
        call: &LspCall,
        pick: &ServerPick,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        self.run_with(cwd, call, pick, cancel, &spawn_server).await
    }

    /// Shut down servers nobody has used for `idle`.
    pub async fn reap_idle(&self, idle: Duration) {
        let mut victims = Vec::new();
        {
            let mut map = self.slots.lock().await;
            let keys: Vec<Key> = map.keys().cloned().collect();
            for k in keys {
                let Some(slot) = map.get(&k).cloned() else {
                    continue;
                };
                // Locked = a server is starting in it right now; leave it be.
                let Ok(mut g) = slot.srv.try_lock() else {
                    continue;
                };
                let reap = match g.as_ref() {
                    None => true,
                    Some(s) => {
                        s.client.dead()
                            || (s.inflight.load(Ordering::SeqCst) == 0
                                && s.last_used.lock().unwrap().elapsed() >= idle)
                    }
                };
                if reap {
                    if let Some(s) = g.take() {
                        victims.push(s);
                    }
                    drop(g);
                    map.remove(&k);
                }
            }
        }
        for s in victims {
            s.shutdown().await;
        }
    }

    pub async fn shutdown_all(&self) {
        let slots: Vec<Arc<Slot>> = self.slots.lock().await.drain().map(|(_, s)| s).collect();
        let mut victims = Vec::new();
        for slot in slots {
            // A server still starting is left to its caller; once that call
            // ends nothing holds it and dropping it kills the process tree.
            if let Ok(mut g) = slot.srv.try_lock() {
                if let Some(s) = g.take() {
                    victims.push(s);
                }
            }
        }
        futures_util::future::join_all(victims.iter().map(|s| s.shutdown())).await;
    }

    pub async fn running(&self) -> Vec<(String, PathBuf)> {
        let map = self.slots.lock().await;
        let mut v: Vec<(String, PathBuf)> = map
            .iter()
            .filter(|(_, slot)| {
                slot.srv
                    .try_lock()
                    .is_ok_and(|g| g.as_ref().is_some_and(|s| !s.client.dead()))
            })
            .map(|((id, root), _)| (id.clone(), root.clone()))
            .collect();
        v.sort();
        v
    }

    /// The live server for `pick`, starting one if there is none. Concurrent
    /// first calls queue on the slot's lock, so only one process is started.
    async fn server(
        &self,
        pick: &ServerPick,
        cancel: &CancellationToken,
        launch: Launcher<'_>,
    ) -> Result<Arc<Server>, String> {
        let key: Key = (pick.id.to_string(), pick.root.clone());
        loop {
            let slot = self
                .slots
                .lock()
                .await
                .entry(key.clone())
                .or_default()
                .clone();
            let mut g = tokio::select! {
                g = slot.srv.lock() => g,
                _ = cancel.cancelled() => return Err(CANCELLED.into()),
            };
            // Reaped while we waited for the lock: start over on the live slot,
            // or a server started here would be invisible to reaping.
            let current = self.slots.lock().await.get(&key).cloned();
            if !current.is_some_and(|c| Arc::ptr_eq(&c, &slot)) {
                continue;
            }
            if let Some(s) = g.as_ref() {
                if !s.client.dead() {
                    return Ok(s.clone());
                }
            }
            if let Some(old) = g.take() {
                old.shutdown().await;
            }
            let srv = self.start(pick, launch(pick)?, cancel).await?;
            *g = Some(srv.clone());
            return Ok(srv);
        }
    }

    async fn start(
        &self,
        pick: &ServerPick,
        l: Launched,
        cancel: &CancellationToken,
    ) -> Result<Arc<Server>, String> {
        let folders = json!([{
            "uri": path_to_uri(&pick.root),
            "name": pick.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        }]);
        let client = Client::start(l.read, l.write, client_config(pick), folders);
        let srv = Server {
            id: pick.id,
            client,
            caps: Value::Null,
            child: StdMutex::new(l.child),
            stderr: l.stderr,
            docs: Mutex::new(HashMap::new()),
            doc_tick: AtomicU64::new(0),
            last_used: StdMutex::new(Instant::now()),
            inflight: AtomicUsize::new(0),
            warmed: AtomicBool::new(false),
            started: Instant::now(),
        };
        let res = srv
            .client
            .request(
                "initialize",
                initialize_params(pick),
                cancel,
                self.timing.init,
            )
            .await;
        let init = match res {
            Ok(v) => v,
            Err(e) => {
                let msg = match e {
                    ReqErr::Dead => format!(
                        "{} ({}) exited during startup.{}{}",
                        pick.id,
                        pick.exe.display(),
                        srv.stderr_tail(),
                        if pick.id == "rust-analyzer" {
                            " If that is the rustup proxy, run `rustup component add rust-analyzer`."
                        } else {
                            ""
                        }
                    ),
                    other => srv.describe("initialize", other, self.timing.init),
                };
                // Dropping `srv` kills the process tree.
                return Err(msg);
            }
        };
        let mut srv = srv;
        srv.caps = init.get("capabilities").cloned().unwrap_or(Value::Null);
        srv.client.notify("initialized", json!({}));
        Ok(Arc::new(srv))
    }

    /// Wait until the server has stopped reporting work. `Ok(false)` = gave up
    /// waiting (results may be incomplete).
    async fn ready(&self, srv: &Server, cancel: &CancellationToken) -> Result<bool, String> {
        let first = !srv.warmed.load(Ordering::SeqCst);
        let max = if first {
            self.timing.first_index
        } else {
            self.timing.later_index
        };
        let deadline = Instant::now() + max;
        loop {
            let bell = srv.client.shared.progress_bell.notified();
            let (active, idle_since, seen) = srv.client.progress();
            let now = Instant::now();
            // A fresh server may not have begun reporting yet: give it a moment
            // so "no progress" isn't mistaken for "done".
            let warm = !first || seen || now.duration_since(srv.started) >= self.timing.warmup;
            if srv.client.dead()
                || (active == 0 && now.duration_since(idle_since) >= self.timing.settle && warm)
            {
                srv.warmed.store(true, Ordering::SeqCst);
                return Ok(true);
            }
            if now >= deadline {
                srv.warmed.store(true, Ordering::SeqCst);
                return Ok(false);
            }
            let nap = Duration::from_millis(50).min(deadline - now);
            tokio::select! {
                _ = bell => {}
                _ = tokio::time::sleep(nap) => {}
                _ = cancel.cancelled() => return Err(CANCELLED.into()),
            }
        }
    }

    /// A request, retried briefly when the server says the content moved
    /// under it (rust-analyzer does this while it is still loading).
    async fn call(
        &self,
        srv: &Server,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        let mut attempt = 0;
        loop {
            match srv
                .client
                .request(method, params.clone(), cancel, self.timing.request)
                .await
            {
                Ok(v) => return Ok(v),
                Err(ReqErr::Rpc(e)) if (e.code == -32801 || e.code == -32802) && attempt < 2 => {
                    attempt += 1;
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                        _ = cancel.cancelled() => return Err(CANCELLED.into()),
                    }
                }
                Err(e) => return Err(srv.describe(method, e, self.timing.request)),
            }
        }
    }

    async fn run_with(
        &self,
        cwd: &Path,
        call: &LspCall,
        pick: &ServerPick,
        cancel: &CancellationToken,
        launch: Launcher<'_>,
    ) -> Result<String, String> {
        if cancel.is_cancelled() {
            return Err(CANCELLED.into());
        }
        let mut src = Src::new(cwd);
        // Validate everything about the file before touching a server.
        let file = match &call.path {
            Some(p) => {
                super::tools::local_guard(p)?;
                let shown = src.rel(p);
                let md = std::fs::metadata(p).map_err(|_| format!("{shown} does not exist."))?;
                if md.is_dir() {
                    return Err(format!("{shown} is a directory; pass a source file."));
                }
                if md.len() > MAX_FILE {
                    return Err(format!(
                        "{shown} is {} KB; the lsp tool only opens files up to {} KB.",
                        md.len() / 1024,
                        MAX_FILE / 1024
                    ));
                }
                let bytes = std::fs::read(p).map_err(|e| format!("Couldn't read {shown}: {e}"))?;
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let pos = if call.op.needs_position() {
                    Some(resolve_position(call, &text, &shown)?)
                } else {
                    None
                };
                Some((norm(p), shown, text, pos))
            }
            None if call.op == Op::WorkspaceSymbols => None,
            None => return Err(format!("`path` is required for `{}`.", call.op.name())),
        };
        let srv = self.server(pick, cancel, launch).await?;
        let _busy = Busy::new(&srv);

        let Some((path, shown, text, pos)) = file else {
            let ready = self.ready(&srv, cancel).await?;
            let q = call.query.clone().unwrap_or_default();
            let res = self
                .call(&srv, "workspace/symbol", json!({"query": q}), cancel)
                .await?;
            return Ok(format_workspace_symbols(&res, &q, &mut src, ready));
        };
        let uri = path_to_uri(&path);
        let mark = srv.sync(&path, &uri, &text, pick.language_id).await;
        let ready = self.ready(&srv, cancel).await?;
        let doc = json!({"uri": uri});

        if call.op == Op::DocumentSymbols {
            let res = self
                .call(
                    &srv,
                    "textDocument/documentSymbol",
                    json!({"textDocument": doc}),
                    cancel,
                )
                .await?;
            return Ok(format_document_symbols(&res, &shown, ready));
        }
        if call.op == Op::Diagnostics {
            return self
                .diagnostics(&srv, &uri, mark, &shown, &mut src, ready, cancel)
                .await;
        }

        let pos = pos.ok_or("internal error: no position")?;
        let at =
            json!({"textDocument": doc, "position": {"line": pos.line0, "character": pos.utf16}});
        let subject = match &pos.subject {
            Some(s) => format!("`{s}` at {shown}:{}", pos.line0 + 1),
            None => format!("{shown}:{}:{}", pos.line0 + 1, pos.col1),
        };
        let (method, what) = match call.op {
            Op::Definition => ("textDocument/definition", "definition"),
            Op::TypeDefinition => ("textDocument/typeDefinition", "type definition"),
            Op::Implementation => ("textDocument/implementation", "implementations"),
            Op::References => ("textDocument/references", "references"),
            Op::Hover => ("textDocument/hover", "hover information"),
            Op::IncomingCalls | Op::OutgoingCalls => {
                return self
                    .calls(&srv, call.op, at, &subject, &mut src, ready, cancel)
                    .await
            }
            _ => unreachable!("handled above"),
        };
        let mut params = at;
        if call.op == Op::References {
            params["context"] = json!({"includeDeclaration": true});
        }
        let res = self.call(&srv, method, params, cancel).await?;
        let empty = || {
            format!(
                "No {what} found for {subject}.{}",
                if ready {
                    ""
                } else {
                    " The server is still indexing; try again shortly."
                }
            )
        };
        let note = indexing_note(ready);
        if call.op == Op::Hover {
            let text = hover_text(&res["contents"]);
            if res.is_null() || text.trim().is_empty() {
                return Ok(empty());
            }
            return Ok(format!(
                "{shown}:{}:{}\n{}{}",
                pos.line0 + 1,
                pos.col1,
                cap_chars(text.trim(), HOVER_CAP),
                note
            ));
        }
        let locs = parse_locations(&res);
        if locs.is_empty() {
            return Ok(empty());
        }
        if call.op == Op::References {
            return Ok(format_references(&locs, &subject, &mut src, &note));
        }
        let entries: Vec<String> = locs.iter().map(|l| src.show(l)).collect();
        let header = if entries.len() == 1 {
            String::new()
        } else {
            format!("{} results for {subject}:", entries.len())
        };
        Ok(finish(header, entries, &note))
    }

    #[allow(clippy::too_many_arguments)]
    async fn diagnostics(
        &self,
        srv: &Server,
        uri: &str,
        mark: u64,
        shown: &str,
        src: &mut Src,
        ready: bool,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let pull = srv
            .caps
            .get("diagnosticProvider")
            .is_some_and(|v| !v.is_null() && v != &json!(false));
        let mut note = String::new();
        let items = if pull {
            let res = self
                .call(
                    srv,
                    "textDocument/diagnostic",
                    json!({"textDocument": {"uri": uri}}),
                    cancel,
                )
                .await?;
            res.get("items").cloned().unwrap_or(json!([]))
        } else {
            let key = uri_key(uri);
            let deadline = Instant::now() + self.timing.diag_wait;
            let fresh = loop {
                let bell = srv.client.shared.diag_bell.notified();
                if srv.client.diag(&key).is_some_and(|(seq, _, _)| seq > mark) {
                    break true;
                }
                let now = Instant::now();
                if now >= deadline || srv.client.dead() {
                    break false;
                }
                tokio::select! {
                    _ = bell => {}
                    _ = tokio::time::sleep(Duration::from_millis(50).min(deadline - now)) => {}
                    _ = cancel.cancelled() => return Err(CANCELLED.into()),
                }
            };
            if fresh {
                // Servers often publish twice (fast syntax pass, then semantic):
                // give a second publish a moment to land.
                let age = srv
                    .client
                    .diag(&key)
                    .map(|(_, at, _)| at.elapsed())
                    .unwrap_or_default();
                if let Some(rest) = self.timing.diag_settle.checked_sub(age) {
                    tokio::select! {
                        _ = tokio::time::sleep(rest) => {}
                        _ = cancel.cancelled() => return Err(CANCELLED.into()),
                    }
                }
            } else {
                note = format!(
                    "\n(no fresh diagnostics were published within {} s; the server may still be analyzing)",
                    self.timing.diag_wait.as_secs()
                );
            }
            srv.client
                .diag(&key)
                .map(|(_, _, d)| d)
                .unwrap_or(json!([]))
        };
        if srv.id == "rust-analyzer" {
            note.push_str("\n(rust-analyzer: native diagnostics only; `cargo check` is not run — use bash for full compiler output)");
        }
        note.push_str(&indexing_note(ready));
        Ok(format_diagnostics(&items, uri, shown, src, &note))
    }

    #[allow(clippy::too_many_arguments)]
    async fn calls(
        &self,
        srv: &Server,
        op: Op,
        at: Value,
        subject: &str,
        src: &mut Src,
        ready: bool,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let prep = self
            .call(srv, "textDocument/prepareCallHierarchy", at, cancel)
            .await?;
        let items: Vec<Value> = prep.as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            return Ok(format!(
                "No function or method at {subject} to build a call hierarchy for.{}",
                if ready {
                    ""
                } else {
                    " The server is still indexing; try again shortly."
                }
            ));
        }
        let incoming = op == Op::IncomingCalls;
        let method = if incoming {
            "callHierarchy/incomingCalls"
        } else {
            "callHierarchy/outgoingCalls"
        };
        let mut sections = Vec::new();
        for item in items.iter().take(3) {
            let name = item["name"].as_str().unwrap_or("?").to_string();
            let res = self
                .call(srv, method, json!({"item": item}), cancel)
                .await?;
            let calls = res.as_array().cloned().unwrap_or_default();
            let mut entries = Vec::new();
            for c in &calls {
                let entry = if incoming {
                    let from = &c["from"];
                    let ranges = c["fromRanges"].as_array().cloned().unwrap_or_default();
                    let start = ranges
                        .first()
                        .map(|r| &r["start"])
                        .unwrap_or(&from["selectionRange"]["start"]);
                    let more = if ranges.len() > 1 {
                        format!("  (+{} more calls)", ranges.len() - 1)
                    } else {
                        String::new()
                    };
                    loc_from(from["uri"].as_str(), start).map(|l| {
                        format!(
                            "{}  {}{more}",
                            from["name"].as_str().unwrap_or("?"),
                            src.show(&l)
                        )
                    })
                } else {
                    let to = &c["to"];
                    loc_from(to["uri"].as_str(), &to["selectionRange"]["start"]).map(|l| {
                        format!("{}  {}", to["name"].as_str().unwrap_or("?"), src.show(&l))
                    })
                };
                if let Some(e) = entry {
                    entries.push(e);
                }
            }
            let header = if incoming {
                format!("Callers of `{name}` ({}):", entries.len())
            } else {
                format!("Calls made by `{name}` ({}):", entries.len())
            };
            if entries.is_empty() {
                sections.push(format!(
                    "{} `{name}`.",
                    if incoming {
                        "No callers found for"
                    } else {
                        "No outgoing calls found in"
                    }
                ));
            } else {
                sections.push(finish(header, entries, ""));
            }
        }
        Ok(format!("{}{}", sections.join("\n\n"), indexing_note(ready)))
    }
}

// ───────── output formatting ─────────

fn indexing_note(ready: bool) -> String {
    if ready {
        String::new()
    } else {
        "\n(server still indexing; results may be incomplete)".into()
    }
}

/// Join a header and entries, capping both count and size.
fn finish(header: String, entries: Vec<String>, note: &str) -> String {
    let total = entries.len();
    let mut out = header;
    let mut shown = 0;
    for e in entries {
        if shown >= MAX_ENTRIES || out.len() + e.len() + 1 > MAX_CHARS {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&e);
        shown += 1;
    }
    if shown < total {
        out.push_str(&format!("\n… {} more", total - shown));
    }
    out.push_str(note);
    out
}

/// A position in some file: 0-based line, UTF-16 column.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Loc {
    path: PathBuf,
    line: u32,
    ch: u32,
}

fn loc_from(uri: Option<&str>, pos: &Value) -> Option<Loc> {
    Some(Loc {
        path: uri_to_path(uri?)?,
        line: pos["line"].as_u64()? as u32,
        ch: pos["character"].as_u64().unwrap_or(0) as u32,
    })
}

/// Location | Location[] | LocationLink[] | null → locations, deduplicated.
fn parse_locations(v: &Value) -> Vec<Loc> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![v],
        _ => vec![],
    };
    let mut out: Vec<Loc> = Vec::new();
    for it in items {
        let (uri, range) = if it.get("targetUri").is_some() {
            let r = if it["targetSelectionRange"].is_object() {
                &it["targetSelectionRange"]
            } else {
                &it["targetRange"]
            };
            (it["targetUri"].as_str(), r)
        } else {
            (it["uri"].as_str(), &it["range"])
        };
        if let Some(l) = loc_from(uri, &range["start"]) {
            if !out.contains(&l) {
                out.push(l);
            }
        }
    }
    out
}

/// Reads and caches source lines for display, and shortens paths.
struct Src {
    cwd: PathBuf,
    files: HashMap<PathBuf, Option<Vec<String>>>,
}

impl Src {
    fn new(cwd: &Path) -> Self {
        Src {
            cwd: norm(cwd),
            files: HashMap::new(),
        }
    }

    /// Relative to the project when inside it (with `/`), else absolute.
    fn rel(&self, p: &Path) -> String {
        let n = norm(p);
        match rel_to(&n, &self.cwd) {
            Some(r) if !r.as_os_str().is_empty() => r
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("/"),
            _ => n.to_string_lossy().to_string(),
        }
    }

    fn line(&mut self, p: &Path, line0: u32) -> Option<String> {
        let lines = self.files.entry(p.to_path_buf()).or_insert_with(|| {
            let md = std::fs::metadata(p).ok()?;
            if md.len() > 4 * MAX_FILE {
                return None;
            }
            let b = std::fs::read(p).ok()?;
            Some(
                split_lines(&String::from_utf8_lossy(&b))
                    .into_iter()
                    .map(String::from)
                    .collect(),
            )
        });
        lines.as_ref()?.get(line0 as usize).cloned()
    }

    /// `path:line:col` (1-based, column in characters).
    fn pos(&mut self, l: &Loc) -> String {
        let col = self
            .line(&l.path, l.line)
            .map(|t| utf16_to_char_col(&t, l.ch))
            .unwrap_or(l.ch + 1);
        format!("{}:{}:{}", self.rel(&l.path), l.line + 1, col)
    }

    /// `path:line:col  source line`.
    fn show(&mut self, l: &Loc) -> String {
        let pos = self.pos(l);
        match self.line(&l.path, l.line) {
            Some(t) if !t.trim().is_empty() => format!("{pos}  {}", cap_chars(t.trim(), LINE_CAP)),
            _ => pos,
        }
    }
}

fn format_references(locs: &[Loc], subject: &str, src: &mut Src, note: &str) -> String {
    let mut sorted = locs.to_vec();
    sorted.sort();
    let mut groups: Vec<(PathBuf, Vec<Loc>)> = Vec::new();
    for l in sorted {
        match groups.last_mut() {
            Some((p, v)) if *p == l.path => v.push(l),
            _ => groups.push((l.path.clone(), vec![l])),
        }
    }
    let header = format!(
        "{} reference{} to {subject} in {} file{}:",
        locs.len(),
        if locs.len() == 1 { "" } else { "s" },
        groups.len(),
        if groups.len() == 1 { "" } else { "s" }
    );
    let mut entries = Vec::new();
    for (p, ls) in &groups {
        entries.push(format!("{} ({})", src.rel(p), ls.len()));
        for l in ls {
            let full = src.show(l);
            // Within a group the path is already in the heading.
            let rel = src.rel(p);
            let short = full.strip_prefix(&format!("{rel}:")).unwrap_or(&full);
            entries.push(format!("  {short}"));
        }
    }
    finish(header, entries, note)
}

fn marked_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            let value = o.get("value").and_then(Value::as_str).unwrap_or("");
            match o.get("language").and_then(Value::as_str) {
                Some(lang) => format!("```{lang}\n{value}\n```"),
                None => value.to_string(),
            }
        }
        _ => String::new(),
    }
}

/// Hover contents (MarkupContent | MarkedString | MarkedString[]) as text.
fn hover_text(v: &Value) -> String {
    match v {
        Value::Array(a) => a
            .iter()
            .map(marked_string)
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => marked_string(other),
    }
}

fn symbol_kind(k: u64) -> &'static str {
    const KINDS: [&str; 26] = [
        "file",
        "module",
        "namespace",
        "package",
        "class",
        "method",
        "property",
        "field",
        "constructor",
        "enum",
        "interface",
        "function",
        "variable",
        "constant",
        "string",
        "number",
        "boolean",
        "array",
        "object",
        "key",
        "null",
        "enum_member",
        "struct",
        "event",
        "operator",
        "type_param",
    ];
    k.checked_sub(1)
        .and_then(|i| KINDS.get(i as usize))
        .copied()
        .unwrap_or("symbol")
}

fn document_symbol_tree(items: &[Value], depth: usize, out: &mut Vec<String>) {
    for it in items {
        let line = it["selectionRange"]["start"]["line"]
            .as_u64()
            .or_else(|| it["range"]["start"]["line"].as_u64())
            .or_else(|| it["location"]["range"]["start"]["line"].as_u64())
            .unwrap_or(0);
        let container = it["containerName"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(|c| format!(" (in {c})"))
            .unwrap_or_default();
        out.push(format!(
            "{}{} {}{container}  :{}",
            "  ".repeat(depth),
            symbol_kind(it["kind"].as_u64().unwrap_or(0)),
            it["name"].as_str().unwrap_or("?"),
            line + 1
        ));
        if let Some(children) = it["children"].as_array() {
            document_symbol_tree(children, depth + 1, out);
        }
    }
}

fn format_document_symbols(res: &Value, shown: &str, ready: bool) -> String {
    let items = res.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return format!(
            "No symbols found in {shown}.{}",
            if ready {
                ""
            } else {
                " The server is still indexing; try again shortly."
            }
        );
    }
    let mut entries = Vec::new();
    document_symbol_tree(&items, 0, &mut entries);
    finish(
        format!("Symbols in {shown}:"),
        entries,
        &indexing_note(ready),
    )
}

fn format_workspace_symbols(res: &Value, query: &str, src: &mut Src, ready: bool) -> String {
    let items = res.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return format!(
            "No symbols matching `{query}`.{}",
            if ready {
                ""
            } else {
                " The server is still indexing; try again shortly."
            }
        );
    }
    let total = items.len();
    let mut entries: Vec<String> = Vec::new();
    for it in items.iter().take(WS_SYMBOLS_CAP) {
        let uri = it["location"]["uri"].as_str();
        let place = match loc_from(uri, &it["location"]["range"]["start"]) {
            Some(l) => format!("{}:{}", src.rel(&l.path), l.line + 1),
            None => uri
                .and_then(uri_to_path)
                .map(|p| src.rel(&p))
                .unwrap_or_default(),
        };
        let container = it["containerName"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(|c| format!(" (in {c})"))
            .unwrap_or_default();
        entries.push(format!(
            "{} {}{container}  {place}",
            symbol_kind(it["kind"].as_u64().unwrap_or(0)),
            it["name"].as_str().unwrap_or("?")
        ));
    }
    let mut note = String::new();
    if total > WS_SYMBOLS_CAP {
        note.push_str(&format!(
            "\n… {} more; narrow the query",
            total - WS_SYMBOLS_CAP
        ));
    }
    note.push_str(&indexing_note(ready));
    finish(
        format!("{total} symbols matching `{query}`:"),
        entries,
        &note,
    )
}

fn format_diagnostics(items: &Value, uri: &str, shown: &str, src: &mut Src, note: &str) -> String {
    let mut list: Vec<&Value> = items
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if list.is_empty() {
        return format!("No diagnostics for {shown}.{note}");
    }
    let sev = |d: &Value| d["severity"].as_u64().unwrap_or(1);
    let line = |d: &Value| d["range"]["start"]["line"].as_u64().unwrap_or(0);
    list.sort_by_key(|d| (sev(d), line(d)));
    let errors = list.iter().filter(|d| sev(d) == 1).count();
    let warnings = list.iter().filter(|d| sev(d) == 2).count();
    let path = uri_to_path(uri).unwrap_or_default();
    let entries: Vec<String> = list
        .iter()
        .map(|d| {
            let l = Loc {
                path: path.clone(),
                line: line(d) as u32,
                ch: d["range"]["start"]["character"].as_u64().unwrap_or(0) as u32,
            };
            let severity = match sev(d) {
                1 => "error",
                2 => "warning",
                3 => "info",
                _ => "hint",
            };
            let code = match &d["code"] {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            };
            let tag = [d["source"].as_str().unwrap_or(""), code.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            let tag = if tag.is_empty() {
                String::new()
            } else {
                format!(" [{tag}]")
            };
            let msg = d["message"]
                .as_str()
                .unwrap_or("")
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            format!("{} {severity}{tag} {}", src.pos(&l), cap_chars(&msg, 600))
        })
        .collect();
    finish(
        format!(
            "{shown}: {} diagnostic{} ({errors} error{}, {warnings} warning{}):",
            list.len(),
            if list.len() == 1 { "" } else { "s" },
            if errors == 1 { "" } else { "s" },
            if warnings == 1 { "" } else { "s" }
        ),
        entries,
        note,
    )
}

/// The one manager every chat shares. Process-wide rather than a `Harness`
/// field on purpose: a language server indexes a *folder*, not a chat, so two
/// chats in the same project should share one rust-analyzer instead of paying
/// for two indexes, and the servers belong to this process whatever builds the
/// harness (desktop, headless, ACP, tests).
pub fn global() -> &'static LspManager {
    static MANAGER: std::sync::OnceLock<LspManager> = std::sync::OnceLock::new();
    MANAGER.get_or_init(LspManager::default)
}

/// How long a server may sit unused before it is shut down. Indexing a large
/// workspace takes minutes, so this is generous; the memory a server holds is
/// the cost of keeping it.
pub const IDLE: Duration = Duration::from_secs(15 * 60);

/// The reaper loop, spawned once at app start.
pub async fn reaper() {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        global().reap_idle(IDLE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{ReadHalf, WriteHalf};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("openleash-lsp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        norm(&d)
    }

    fn write(p: &Path, s: &str) {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(p, s).unwrap();
    }

    fn frame(s: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{s}", s.len()).into_bytes()
    }

    // ── framing ──

    #[test]
    fn framing_round_trip_and_partial_frames() {
        let msg = json!({"jsonrpc":"2.0","id":1,"result":{"x":"héllo"}});
        let bytes = encode(&msg);
        // Feed it one byte at a time: nothing until the last byte arrives.
        let mut buf = Vec::new();
        for (i, b) in bytes.iter().enumerate() {
            buf.push(*b);
            let got = take_frame(&mut buf).unwrap();
            if i + 1 < bytes.len() {
                assert!(got.is_none(), "frame completed early at byte {i}");
            } else {
                let v: Value = serde_json::from_slice(&got.unwrap()).unwrap();
                assert_eq!(v, msg);
            }
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn framing_handles_several_messages_case_and_extra_headers() {
        let a = r#"{"id":1}"#;
        let b = r#"{"id":2}"#;
        let mut buf = frame(a);
        buf.extend(
            format!(
                "content-type: application/vscode-jsonrpc; charset=utf-8\r\nCONTENT-LENGTH:  {}\r\n\r\n{b}",
                b.len()
            )
            .into_bytes(),
        );
        buf.extend(b"Content-Length: 50\r\n\r\n{\"partial".to_vec());
        assert_eq!(take_frame(&mut buf).unwrap().unwrap(), a.as_bytes());
        assert_eq!(take_frame(&mut buf).unwrap().unwrap(), b.as_bytes());
        assert!(take_frame(&mut buf).unwrap().is_none());
        assert!(buf.starts_with(b"Content-Length: 50"));
        let mut bad = b"Content-Type: x\r\n\r\n{}".to_vec();
        assert!(take_frame(&mut bad).is_err());
    }

    #[tokio::test]
    async fn frame_reader_over_split_reads() {
        let (mut a, b) = tokio::io::duplex(64);
        let mut fr = FrameReader::new(b);
        let mut all = frame(r#"{"n":1}"#);
        all.extend(frame(r#"{"n":2}"#));
        tokio::spawn(async move {
            for chunk in all.chunks(5) {
                a.write_all(chunk).await.unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        assert_eq!(fr.next().await.unwrap().unwrap()["n"], 1);
        assert_eq!(fr.next().await.unwrap().unwrap()["n"], 2);
        assert!(fr.next().await.unwrap().is_none());
    }

    // ── columns, symbols, URIs ──

    #[test]
    fn utf16_columns_both_ways() {
        let line = "let 😀x = \"é\";";
        // char columns: l=1 … 😀=5, x=6
        assert_eq!(char_col_to_utf16(line, 1), 0);
        assert_eq!(char_col_to_utf16(line, 5), 4);
        assert_eq!(
            char_col_to_utf16(line, 6),
            6,
            "the emoji is two UTF-16 units"
        );
        assert_eq!(utf16_to_char_col(line, 0), 1);
        assert_eq!(utf16_to_char_col(line, 4), 5);
        assert_eq!(utf16_to_char_col(line, 6), 6);
        // Inside the surrogate pair → the emoji's column.
        assert_eq!(utf16_to_char_col(line, 5), 5);
        for col in 1..=line.chars().count() as u32 + 1 {
            assert_eq!(utf16_to_char_col(line, char_col_to_utf16(line, col)), col);
        }
        // Past the end clamps.
        assert_eq!(char_col_to_utf16("ab", 99), 2);
    }

    #[test]
    fn symbol_matches_whole_words_only() {
        let line = "    let foobar = foo(foo_x) + foo;";
        let i = find_symbol(line, "foo").unwrap();
        assert_eq!(&line[i..i + 4], "foo(");
        assert_eq!(find_symbol("foobar(barfoo)", "foo"), None);
        assert_eq!(find_symbol("x.$foo + foo", "foo"), Some(9));
        assert_eq!(find_symbol("a->b", "->"), Some(1));
        assert_eq!(find_symbol("Vec::new()", "new"), Some(5));
        assert_eq!(find_symbol("é_foo foo", "foo"), Some(7));

        let call = LspCall {
            op: Op::Definition,
            path: None,
            line: Some(2),
            character: None,
            symbol: Some("target".into()),
            query: None,
            language: None,
        };
        let text = "fn main() {\n    let 😀 = target_x(target);\n}\n";
        let p = resolve_position(&call, text, "a.rs").unwrap();
        assert_eq!(p.line0, 1);
        assert_eq!(p.col1, 22);
        // 21 characters precede it, one of them a surrogate pair.
        assert_eq!(p.utf16, 22, "the emoji counts twice in UTF-16");
        let none = LspCall {
            symbol: None,
            ..call.clone()
        };
        let p = resolve_position(&none, text, "a.rs").unwrap();
        assert_eq!((p.col1, p.subject.as_deref()), (5, Some("let")));
        let missing = LspCall {
            symbol: Some("nope".into()),
            ..call.clone()
        };
        let e = resolve_position(&missing, text, "a.rs").unwrap_err();
        assert!(e.contains("`nope` does not appear"), "{e}");
        let past = LspCall {
            line: Some(9),
            ..call
        };
        assert!(resolve_position(&past, text, "a.rs")
            .unwrap_err()
            .contains("past the end"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_uris_round_trip() {
        let p = PathBuf::from(r"C:\My Projects\app\src\main.rs");
        let u = path_to_uri(&p);
        assert_eq!(u, "file:///C:/My%20Projects/app/src/main.rs");
        assert_eq!(uri_to_path(&u).unwrap(), p);
        assert_eq!(
            path_to_uri(Path::new(r"\\?\C:\x\y.rs")),
            "file:///C:/x/y.rs"
        );
        // VS Code's spelling: lowercase drive, encoded colon.
        assert_eq!(
            uri_to_path("file:///c%3A/x/y.rs").unwrap(),
            PathBuf::from(r"C:\x\y.rs")
        );
        assert_eq!(uri_key("file:///c%3A/X/y.rs"), uri_key("file:///C:/x/Y.rs"));
        let unc = PathBuf::from(r"\\server\share\a b.rs");
        let u = path_to_uri(&unc);
        assert_eq!(u, "file://server/share/a%20b.rs");
        assert_eq!(uri_to_path(&u).unwrap(), unc);
        assert!(uri_to_path("untitled:Untitled-1").is_none());
        // Comparisons ignore case on Windows.
        assert!(is_within(Path::new(r"c:\Proj\src"), Path::new(r"C:\proj")));
        assert!(!is_within(Path::new(r"C:\project2"), Path::new(r"C:\proj")));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_uris_round_trip() {
        let p = PathBuf::from("/home/me/my app/src/main #1.rs");
        let u = path_to_uri(&p);
        assert_eq!(u, "file:///home/me/my%20app/src/main%20%231.rs");
        assert_eq!(uri_to_path(&u).unwrap(), p);
        assert_eq!(
            uri_to_path("file://localhost/tmp/x.rs").unwrap(),
            PathBuf::from("/tmp/x.rs")
        );
        assert!(uri_to_path("file://otherhost/tmp/x.rs").is_none());
        assert!(is_within(Path::new("/proj/src"), Path::new("/proj")));
        assert!(!is_within(Path::new("/project2"), Path::new("/proj")));
    }

    // ── parse_call ──

    #[test]
    fn parse_call_validates_per_operation() {
        let cwd = if cfg!(windows) { r"C:\proj" } else { "/proj" };
        let e = |v: Value| parse_call(&v, cwd).unwrap_err();
        assert!(e(json!({})).contains("`operation` is required"));
        assert!(e(json!({"operation":"rename"})).contains("Unknown operation `rename`"));
        assert!(e(json!({"operation":"definition","line":3})).contains("`path` is required"));
        assert!(e(json!({"operation":"hover","path":"a.rs"})).contains("`line` is required"));
        assert!(e(json!({"operation":"definition","path":"a.rs","line":0})).contains("1-based"));
        assert!(e(json!({"operation":"definition","path":"a.rs","line":-2})).contains("1-based"));
        assert!(
            e(json!({"operation":"definition","path":"a.rs","line":"x"}))
                .contains("positive integer")
        );
        assert!(e(json!({"operation":"workspace_symbols"})).contains("`query` is required"));
        assert!(
            e(json!({"operation":"workspace_symbols","query":"X","language":"cobol"}))
                .contains("Unknown language")
        );
        assert!(
            e(json!({"operation":"hover","path":"a.rs","line":1,"symbol":"a\nb"}))
                .contains("single identifier")
        );

        let c = parse_call(
            &json!({"operation":"references","path":"src/a.rs","line":"12","symbol":" foo "}),
            cwd,
        )
        .unwrap();
        assert_eq!(c.op, Op::References);
        assert_eq!(c.line, Some(12));
        assert_eq!(c.symbol.as_deref(), Some("foo"));
        assert_eq!(c.path.unwrap(), Path::new(cwd).join("src/a.rs"));
        let c = parse_call(&json!({"operation":"diagnostics","path":"a.py"}), cwd).unwrap();
        assert_eq!((c.op, c.line), (Op::Diagnostics, None));
        let c = parse_call(
            &json!({"operation":"workspace_symbols","query":"Parser","language":"TS"}),
            cwd,
        )
        .unwrap();
        assert_eq!(c.language.as_deref(), Some("typescript"));
        assert!(c.path.is_none());
        let s = schema();
        assert_eq!(s["name"], "lsp");
        assert_eq!(s["input_schema"]["required"], json!(["operation"]));
        assert_eq!(
            s["input_schema"]["properties"]["operation"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            10
        );
    }

    // ── server table and PATH lookup ──

    fn fake_exe(dir: &Path, name: &str) -> PathBuf {
        let file = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        let p = dir.join(file);
        write(&p, "fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn call_for(op: Op, path: Option<PathBuf>) -> LspCall {
        LspCall {
            op,
            path,
            line: Some(1),
            character: None,
            symbol: None,
            query: Some("x".into()),
            language: None,
        }
    }

    #[test]
    fn path_lookup_never_resolves_inside_the_project() {
        let base = tmp("pathlookup");
        let project = base.join("project");
        let outside = base.join("tools");
        let in_bin = project.join("node_modules").join(".bin");
        let planted = fake_exe(&in_bin, "rust-analyzer");
        fake_exe(&project, "rust-analyzer");
        let real = fake_exe(&outside, "rust-analyzer");

        let path_var = std::env::join_paths([
            PathBuf::from("node_modules/.bin"),
            PathBuf::from("."),
            in_bin.clone(),
            project.clone(),
            outside.clone(),
        ])
        .unwrap();
        let got = find_exe_in("rust-analyzer", &path_var, &project).unwrap();
        assert_eq!(norm(&got), norm(&real));
        assert_ne!(norm(&got), norm(&planted));

        // Only project entries → nothing at all, never the planted copy.
        let only_project = std::env::join_paths([in_bin.clone(), project.clone()]).unwrap();
        assert!(find_exe_in("rust-analyzer", &only_project, &project).is_none());

        // Case differences don't smuggle a project dir past the check on Windows.
        if cfg!(windows) {
            let upper = PathBuf::from(in_bin.to_string_lossy().to_uppercase());
            let pv = std::env::join_paths([upper]).unwrap();
            assert!(find_exe_in("rust-analyzer", &pv, &project).is_none());
        }

        // pick_server reports what to install when nothing is found.
        write(&project.join("src/main.rs"), "fn main() {}\n");
        let call = call_for(Op::Hover, Some(project.join("src/main.rs")));
        let e = pick_server_with(&project, &call, &only_project).unwrap_err();
        assert!(e.contains("rustup component add rust-analyzer"), "{e}");
        let pick = pick_server_with(&project, &call, &path_var).unwrap();
        assert_eq!(pick.id, "rust-analyzer");
        assert_eq!(norm(&pick.exe), norm(&real));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn server_table_and_roots() {
        let base = tmp("roots");
        let tools = base.join("tools");
        for n in [
            "rust-analyzer",
            "typescript-language-server",
            "basedpyright-langserver",
            "gopls",
            "clangd",
        ] {
            fake_exe(&tools, n);
        }
        let pv = std::env::join_paths([tools.clone()]).unwrap();
        // A Cargo.toml *above* the project must never be picked as a root.
        write(&base.join("Cargo.toml"), "[workspace]\n");
        let proj = base.join("proj");
        write(
            &proj.join("Cargo.toml"),
            "[workspace]\nmembers=[\"crates/a\"]\n",
        );
        write(&proj.join("crates/a/Cargo.toml"), "[package]\n");
        write(&proj.join("crates/a/src/lib.rs"), "pub fn a() {}\n");
        write(&proj.join("web/package.json"), "{}\n");
        write(&proj.join("web/src/App.tsx"), "export {}\n");
        write(&proj.join("py/pyproject.toml"), "\n");
        write(&proj.join("py/pkg/m.py"), "x = 1\n");
        write(&proj.join("go/go.mod"), "module x\n");
        write(&proj.join("go/cmd/main.go"), "package main\n");
        write(&proj.join("native/x.cpp"), "int main(){}\n");
        write(&proj.join("README.md"), "hi\n");

        let pick =
            |rel: &str| pick_server_with(&proj, &call_for(Op::Hover, Some(proj.join(rel))), &pv);
        let rs = pick("crates/a/src/lib.rs").unwrap();
        assert_eq!(rs.id, "rust-analyzer");
        assert_eq!(
            rs.root, proj,
            "the Cargo workspace root, not the member, not above cwd"
        );
        assert_eq!(rs.language_id, "rust");
        let ts = pick("web/src/App.tsx").unwrap();
        assert_eq!(
            (ts.id, ts.language_id),
            ("typescript-language-server", "typescriptreact")
        );
        assert_eq!(ts.root, norm(&proj.join("web")));
        assert_eq!(ts.args, vec!["--stdio".to_string()]);
        let py = pick("py/pkg/m.py").unwrap();
        assert_eq!(
            py.id, "basedpyright-langserver",
            "falls back when pyright is absent"
        );
        assert_eq!(py.root, norm(&proj.join("py")));
        let go = pick("go/cmd/main.go").unwrap();
        assert_eq!((go.id, go.root.clone()), ("gopls", norm(&proj.join("go"))));
        let c = pick("native/x.cpp").unwrap();
        assert_eq!((c.id, c.root.clone()), ("clangd", proj.clone()));
        assert!(pick("README.md").unwrap_err().contains("`.md`"));

        // Without Cargo.toml at cwd, a nested crate is still found as the root —
        // but never the one above cwd.
        let app = base.join("app");
        write(&app.join("package.json"), "{}\n");
        write(&app.join("src-tauri/Cargo.toml"), "[package]\n");
        write(&app.join("src-tauri/src/main.rs"), "fn main() {}\n");
        let r = pick_server_with(
            &app,
            &call_for(Op::Hover, Some(app.join("src-tauri/src/main.rs"))),
            &pv,
        )
        .unwrap();
        assert_eq!(r.root, norm(&app.join("src-tauri")));
        let mut ws = call_for(Op::WorkspaceSymbols, None);
        let g = pick_server_with(&app, &ws, &pv).unwrap();
        assert_eq!(
            g.id, "typescript-language-server",
            "guessed from package.json at cwd"
        );
        assert_eq!(g.root, app);
        ws.language = Some("rust".into());
        let g = pick_server_with(&app, &ws, &pv).unwrap();
        assert_eq!(
            (g.id, g.root.clone()),
            ("rust-analyzer", norm(&app.join("src-tauri")))
        );

        // Remote projects are refused up front.
        let e = pick_server_with(Path::new("ssh://host/home/x"), &ws, &pv).unwrap_err();
        assert!(e.contains("not supported for remote"), "{e}");
        let e =
            pick_server_with(Path::new(r"\\wsl.localhost\Ubuntu\home\x"), &ws, &pv).unwrap_err();
        assert!(e.contains("WSL"), "{e}");
        assert!(is_wsl_unc(r"\\wsl$\Ubuntu\x"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_shims_get_their_arguments() {
        let d = tmp("cmdshim");
        let tools = d.join("tools");
        let proj = d.join("proj");
        // The shim calls a helper by bare name, the way npm's shims call `node`.
        let exe = tools.join("echoargs.cmd");
        write(
            &exe,
            "@echo off\r\necho ARGS:%*\r\necho CWD:%CD%\r\nplanted-helper\r\n",
        );
        // A planted helper in the project must not be what that resolves to.
        write(&proj.join("planted-helper.cmd"), "@echo PLANTED\r\n");
        let pick = ServerPick {
            id: "typescript-language-server",
            root: proj.clone(),
            exe,
            args: vec!["--stdio".into()],
            language_id: "typescript",
        };
        let mut l = spawn_server(&pick).unwrap();
        let mut out = String::new();
        l.read.read_to_string(&mut out).await.unwrap();
        assert!(out.contains("ARGS:--stdio"), "{out:?}");
        assert!(!out.contains("PLANTED"), "{out:?}");
        let cwd_line = out.lines().find_map(|l| l.strip_prefix("CWD:")).unwrap();
        assert_eq!(norm(Path::new(cwd_line.trim())), tools);
        if let Some(mut c) = l.child.take() {
            let _ = c.wait().await;
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    // ── end to end over an in-process fake server ──

    type FakeWrite = WriteHalf<tokio::io::DuplexStream>;

    /// The server side of a duplex pipe. A background task timestamps every
    /// message as it arrives, so a test can tell *when* the client sent it.
    struct Fake {
        rx: mpsc::UnboundedReceiver<(Instant, Value)>,
        w: FakeWrite,
    }

    impl Fake {
        async fn recv(&mut self) -> (Instant, Value) {
            tokio::time::timeout(Duration::from_secs(10), self.rx.recv())
                .await
                .expect("fake server waited too long")
                .expect("client hung up")
        }

        /// Next message with this method, answering nothing else on the way.
        async fn expect(&mut self, method: &str) -> (Instant, Value) {
            loop {
                let (t, m) = self.recv().await;
                if m["method"] == method {
                    return (t, m);
                }
            }
        }

        async fn send(&mut self, v: Value) {
            self.w.write_all(&encode(&v)).await.unwrap();
        }

        async fn reply(&mut self, req: &Value, result: Value) {
            self.send(json!({"jsonrpc":"2.0","id":req["id"],"result":result}))
                .await;
        }

        async fn handshake(&mut self, caps: Value) {
            let (_, init) = self.expect("initialize").await;
            assert_eq!(
                init["params"]["capabilities"]["general"]["positionEncodings"],
                json!(["utf-16"])
            );
            assert_eq!(
                init["params"]["capabilities"]["textDocument"]["documentSymbol"]
                    ["hierarchicalDocumentSymbolSupport"],
                true
            );
            assert!(init["params"]["rootUri"]
                .as_str()
                .unwrap()
                .starts_with("file://"));
            self.reply(&init, json!({"capabilities": caps})).await;
            self.expect("initialized").await;
        }
    }

    fn fake_pair() -> (Launched, Fake) {
        let (client_end, server_end) = tokio::io::duplex(1 << 20);
        let (cr, cw) = tokio::io::split(client_end);
        let (sr, sw): (ReadHalf<_>, FakeWrite) = tokio::io::split(server_end);
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut fr = FrameReader::new(sr);
            // Stops as soon as the `Fake` is dropped, so dropping it closes
            // the pipe and the client sees end-of-stream (a dead server).
            loop {
                tokio::select! {
                    m = fr.next() => match m {
                        Ok(Some(v)) => {
                            if tx.send((Instant::now(), v)).is_err() {
                                break;
                            }
                        }
                        _ => break,
                    },
                    _ = tx.closed() => break,
                }
            }
        });
        (
            Launched {
                read: Box::new(cr),
                write: Box::new(cw),
                child: None,
                stderr: Default::default(),
            },
            Fake { rx, w: sw },
        )
    }

    fn quick_manager() -> LspManager {
        LspManager {
            slots: Default::default(),
            timing: Timing {
                request: Duration::from_secs(10),
                init: Duration::from_secs(10),
                first_index: Duration::from_secs(5),
                later_index: Duration::from_secs(2),
                settle: Duration::from_millis(30),
                warmup: Duration::from_millis(200),
                diag_wait: Duration::from_secs(3),
                diag_settle: Duration::from_millis(50),
            },
        }
    }

    /// A launcher that hands out one transport and refuses a second start.
    fn once(l: Launched) -> impl Fn(&ServerPick) -> Result<Launched, String> + Send + Sync {
        let slot = StdMutex::new(Some(l));
        move |_| {
            slot.lock()
                .unwrap()
                .take()
                .ok_or_else(|| "a second server was started".to_string())
        }
    }

    fn rust_project(name: &str) -> (PathBuf, ServerPick) {
        let cwd = tmp(name);
        write(&cwd.join("Cargo.toml"), "[package]\n");
        write(
            &cwd.join("src/lib.rs"),
            "fn main() {\n    let x = target();\n    let y = other();\n}\nfn target() -> i32 { 1 }\nfn other() -> i32 { 2 }\n",
        );
        let pick = ServerPick {
            id: "rust-analyzer",
            root: cwd.clone(),
            exe: PathBuf::from("fake"),
            args: vec![],
            language_id: "rust",
        };
        (cwd, pick)
    }

    fn def_call(cwd: &Path, line: u32, symbol: &str) -> LspCall {
        LspCall {
            op: Op::Definition,
            path: Some(cwd.join("src/lib.rs")),
            line: Some(line),
            character: None,
            symbol: Some(symbol.into()),
            query: None,
            language: None,
        }
    }

    #[tokio::test]
    async fn definition_end_to_end_with_config_and_progress() {
        let (cwd, pick) = rust_project("e2e");
        let (launched, mut fake) = fake_pair();
        let (cfg_tx, cfg_rx) = oneshot::channel::<Value>();
        let server = tokio::spawn(async move {
            fake.handshake(json!({"definitionProvider": true, "textDocumentSync": 1}))
                .await;
            // Before doing any work the server asks for settings and reports
            // indexing; the client must answer and wait for it to finish.
            fake.send(json!({"jsonrpc":"2.0","id":"p1","method":"window/workDoneProgress/create","params":{"token":"idx"}})).await;
            fake.send(json!({"jsonrpc":"2.0","id":901,"method":"workspace/configuration","params":{"items":[{"section":"rust-analyzer"},{"section":"rust-analyzer.procMacro"},{"section":"other"}]}})).await;
            fake.send(json!({"jsonrpc":"2.0","method":"$/progress","params":{"token":"idx","value":{"kind":"begin","title":"Indexing"}}})).await;
            let mut uri = String::new();
            let mut cfg_tx = Some(cfg_tx);
            let mut got_create = false;
            // Collect the answers and the didOpen while "indexing".
            while uri.is_empty() || cfg_tx.is_some() || !got_create {
                let (_, m) = fake.recv().await;
                if m["id"] == 901 && m.get("result").is_some() {
                    let _ = cfg_tx.take().unwrap().send(m["result"].clone());
                } else if m["id"] == "p1" {
                    got_create = true;
                } else if m["method"] == "textDocument/didOpen" {
                    assert_eq!(m["params"]["textDocument"]["version"], 1);
                    assert_eq!(m["params"]["textDocument"]["languageId"], "rust");
                    uri = m["params"]["textDocument"]["uri"].as_str().unwrap().into();
                } else if m["method"] == "textDocument/definition" {
                    panic!("definition was requested while the server was still indexing");
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            let ended = Instant::now();
            fake.send(json!({"jsonrpc":"2.0","method":"$/progress","params":{"token":"idx","value":{"kind":"end"}}})).await;
            let (t, def) = fake.expect("textDocument/definition").await;
            assert!(t >= ended, "the client asked before indexing ended");
            assert_eq!(def["params"]["textDocument"]["uri"], uri.as_str());
            assert_eq!(def["params"]["position"], json!({"line":1,"character":12}));
            fake.reply(
                &def,
                json!({"uri": uri, "range": {"start":{"line":4,"character":3},"end":{"line":4,"character":9}}}),
            )
            .await;
            fake
        });
        let mgr = quick_manager();
        let launch = once(launched);
        let out = mgr
            .run_with(
                &cwd,
                &def_call(&cwd, 2, "target"),
                &pick,
                &CancellationToken::new(),
                &launch,
            )
            .await
            .unwrap();
        assert_eq!(out, "src/lib.rs:5:4  fn target() -> i32 { 1 }");
        let cfg = cfg_rx.await.unwrap();
        assert_eq!(cfg[0]["cargo"]["buildScripts"]["enable"], false);
        assert_eq!(cfg[0]["procMacro"]["enable"], false);
        assert_eq!(cfg[0]["checkOnSave"], false);
        assert_eq!(cfg[1]["enable"], false, "dotted sections resolve");
        assert!(cfg[2].is_null());
        assert_eq!(
            mgr.running().await,
            vec![("rust-analyzer".to_string(), cwd.clone())]
        );

        // Idle reaping sends shutdown + exit and forgets the server.
        let mut fake = server.await.unwrap();
        let reaper = tokio::spawn(async move {
            let (_, sd) = fake.expect("shutdown").await;
            fake.reply(&sd, Value::Null).await;
            fake.expect("exit").await;
        });
        mgr.reap_idle(Duration::from_secs(3600)).await;
        assert_eq!(mgr.running().await.len(), 1, "a recently used server stays");
        mgr.reap_idle(Duration::ZERO).await;
        assert!(mgr.running().await.is_empty());
        reaper.await.unwrap();
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[tokio::test]
    async fn concurrent_calls_share_one_server_and_get_their_own_answers() {
        let (cwd, pick) = rust_project("concurrent");
        let (launched, mut fake) = fake_pair();
        tokio::spawn(async move {
            fake.handshake(json!({"definitionProvider": true})).await;
            let mut reqs = Vec::new();
            let mut opens = 0;
            while reqs.len() < 2 {
                let (_, m) = fake.recv().await;
                if m["method"] == "textDocument/definition" {
                    reqs.push(m);
                } else if m["method"] == "textDocument/didOpen" {
                    opens += 1;
                }
            }
            assert_eq!(opens, 1, "one didOpen for one file");
            // Answer in reverse order: routing must be by id, not by arrival.
            for r in reqs.iter().rev() {
                let uri = r["params"]["textDocument"]["uri"].clone();
                let target = if r["params"]["position"]["line"] == 1 {
                    4
                } else {
                    5
                };
                fake.reply(
                    r,
                    json!([{"uri": uri, "range": {"start":{"line":target,"character":3},"end":{"line":target,"character":3}}}]),
                )
                .await;
            }
            // Keep the pipe open until the test is done.
            let _ = fake.rx.recv().await;
        });
        let mgr = quick_manager();
        let launch = once(launched);
        let cancel = CancellationToken::new();
        let a_call = def_call(&cwd, 2, "target");
        let b_call = def_call(&cwd, 3, "other");
        let (a, b) = tokio::join!(
            mgr.run_with(&cwd, &a_call, &pick, &cancel, &launch),
            mgr.run_with(&cwd, &b_call, &pick, &cancel, &launch),
        );
        assert_eq!(a.unwrap(), "src/lib.rs:5:4  fn target() -> i32 { 1 }");
        assert_eq!(b.unwrap(), "src/lib.rs:6:4  fn other() -> i32 { 2 }");
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[tokio::test]
    async fn cancellation_returns_promptly_and_tells_the_server() {
        let (cwd, pick) = rust_project("cancel");
        let (launched, mut fake) = fake_pair();
        let (done_tx, done_rx) = oneshot::channel::<(Value, Value)>();
        tokio::spawn(async move {
            fake.handshake(json!({"definitionProvider": true})).await;
            let (_, def) = fake.expect("textDocument/definition").await;
            // Never answer; wait to be told to drop it.
            let (_, c) = fake.expect("$/cancelRequest").await;
            let _ = done_tx.send((def["id"].clone(), c["params"]["id"].clone()));
            let _ = fake.rx.recv().await;
        });
        let mgr = quick_manager();
        let launch = once(launched);
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            c2.cancel();
        });
        let t = Instant::now();
        let r = mgr
            .run_with(&cwd, &def_call(&cwd, 2, "target"), &pick, &cancel, &launch)
            .await;
        assert_eq!(r.unwrap_err(), CANCELLED);
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        let (req_id, cancelled_id) = tokio::time::timeout(Duration::from_secs(5), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(req_id, cancelled_id);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[tokio::test]
    async fn diagnostics_from_publish_and_server_death_is_reported() {
        let (cwd, pick) = rust_project("diags");
        let (launched, mut fake) = fake_pair();
        tokio::spawn(async move {
            fake.handshake(json!({"textDocumentSync": 1})).await;
            let (_, open) = fake.expect("textDocument/didOpen").await;
            let uri = open["params"]["textDocument"]["uri"].clone();
            // A stale publish for another file must not satisfy the wait.
            fake.send(json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///elsewhere.rs","diagnostics":[]}})).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            fake.send(json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":uri,"version":1,"diagnostics":[
                {"range":{"start":{"line":2,"character":12},"end":{"line":2,"character":17}},"severity":2,"source":"rust-analyzer","code":"unused","message":"unused variable"},
                {"range":{"start":{"line":1,"character":12},"end":{"line":1,"character":18}},"severity":1,"source":"rustc","code":"E0425","message":"cannot find function\n  in this scope"}
            ]}})).await;
            // Then the server dies.
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(fake);
        });
        let mgr = quick_manager();
        let launch = once(launched);
        let call = LspCall {
            op: Op::Diagnostics,
            path: Some(cwd.join("src/lib.rs")),
            line: None,
            character: None,
            symbol: None,
            query: None,
            language: None,
        };
        let cancel = CancellationToken::new();
        let out = mgr
            .run_with(&cwd, &call, &pick, &cancel, &launch)
            .await
            .unwrap();
        assert!(
            out.starts_with("src/lib.rs: 2 diagnostics (1 error, 1 warning):"),
            "{out}"
        );
        assert!(
            out.contains("\nsrc/lib.rs:2:13 error [rustc E0425] cannot find function in this scope\nsrc/lib.rs:3:13 warning [rust-analyzer unused] unused variable"),
            "{out}"
        );
        assert!(out.contains("native diagnostics only"), "{out}");

        // After the server exits, it is no longer reported as running and the
        // next call tries to start a new one (our launcher refuses a second).
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(mgr.running().await.is_empty());
        let e = mgr
            .run_with(&cwd, &def_call(&cwd, 2, "target"), &pick, &cancel, &launch)
            .await
            .unwrap_err();
        assert!(e.contains("a second server was started"), "{e}");
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[tokio::test]
    async fn server_requests_and_unknown_methods() {
        let (launched, mut fake) = fake_pair();
        let client = Client::start(
            launched.read,
            launched.write,
            json!({"rust-analyzer": {"a": 1}}),
            json!([{"uri": "file:///x", "name": "x"}]),
        );
        fake.send(json!({"jsonrpc":"2.0","id":1,"method":"workspace/workspaceFolders"}))
            .await;
        fake.send(json!({"jsonrpc":"2.0","id":2,"method":"workspace/applyEdit","params":{}}))
            .await;
        fake.send(json!({"jsonrpc":"2.0","id":3,"method":"client/registerCapability","params":{}}))
            .await;
        fake.send(json!({"jsonrpc":"2.0","id":4,"method":"custom/thing"}))
            .await;
        let mut got = HashMap::new();
        while got.len() < 4 {
            let (_, m) = fake.recv().await;
            got.insert(m["id"].as_i64().unwrap(), m);
        }
        assert_eq!(got[&1]["result"][0]["name"], "x");
        assert_eq!(got[&2]["result"]["applied"], false);
        assert!(got[&3]["result"].is_null() && got[&3].get("error").is_none());
        assert_eq!(got[&4]["error"]["code"], -32601);
        // Progress tracking: begin/end by token, numeric tokens too.
        fake.send(json!({"jsonrpc":"2.0","method":"$/progress","params":{"token":7,"value":{"kind":"begin"}}})).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(client.progress().0, 1);
        fake.send(json!({"jsonrpc":"2.0","method":"$/progress","params":{"token":7,"value":{"kind":"end"}}})).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(client.progress().0, 0);
        // An error reply surfaces as an RPC error, and EOF fails pending calls.
        let cancel = CancellationToken::new();
        let req = client.request("x/y", Value::Null, &cancel, Duration::from_secs(5));
        let srv = async {
            let (_, m) = fake.recv().await;
            fake.send(
                json!({"jsonrpc":"2.0","id":m["id"],"error":{"code":-32603,"message":"boom"}}),
            )
            .await;
            fake
        };
        let (r, fake) = tokio::join!(req, srv);
        assert!(matches!(r, Err(ReqErr::Rpc(RpcErr { code: -32603, .. }))));
        let req = client.request("x/z", Value::Null, &cancel, Duration::from_secs(5));
        let srv = async move {
            let mut fake = fake;
            fake.recv().await;
            drop(fake);
        };
        let (r, _) = tokio::join!(req, srv);
        assert!(matches!(r, Err(ReqErr::Dead)), "{r:?}");
        assert!(client.dead());
    }

    #[tokio::test]
    async fn edited_files_are_resynced_with_full_text() {
        let (cwd, pick) = rust_project("resync");
        let other = cwd.join("src/other.rs");
        write(&other, "pub fn o() {}\n");
        let (launched, mut fake) = fake_pair();
        let (seen_tx, seen_rx) = oneshot::channel::<Vec<Value>>();
        tokio::spawn(async move {
            fake.handshake(json!({"hoverProvider": true})).await;
            let mut log = Vec::new();
            let mut hovers = 0;
            while hovers < 3 {
                let (_, m) = fake.recv().await;
                if m["method"] == "textDocument/hover" {
                    hovers += 1;
                    fake.reply(
                        &m,
                        json!({"contents": {"kind": "markdown", "value": "```rust\nfn target() -> i32\n```\nDocs."}}),
                    )
                    .await;
                } else if m.get("method").is_some() {
                    log.push(m);
                }
            }
            let _ = seen_tx.send(log);
            let _ = fake.rx.recv().await;
        });
        let mgr = quick_manager();
        let launch = once(launched);
        let cancel = CancellationToken::new();
        let hover = |path: PathBuf| LspCall {
            op: Op::Hover,
            path: Some(path),
            line: Some(1),
            character: None,
            symbol: None,
            query: None,
            language: None,
        };
        let lib = cwd.join("src/lib.rs");
        let out = mgr
            .run_with(&cwd, &hover(other.clone()), &pick, &cancel, &launch)
            .await
            .unwrap();
        assert_eq!(
            out,
            "src/other.rs:1:1\n```rust\nfn target() -> i32\n```\nDocs."
        );
        mgr.run_with(&cwd, &hover(lib.clone()), &pick, &cancel, &launch)
            .await
            .unwrap();
        // The agent edits a file opened by an earlier call; the next call on a
        // *different* file must still bring the server's copy up to date.
        write(&other, "pub fn o() {}\npub fn p() {}\n");
        mgr.run_with(&cwd, &hover(lib), &pick, &cancel, &launch)
            .await
            .unwrap();
        let log = seen_rx.await.unwrap();
        let methods: Vec<&str> = log.iter().filter_map(|m| m["method"].as_str()).collect();
        assert_eq!(
            methods,
            [
                "textDocument/didOpen",
                "textDocument/didOpen",
                "textDocument/didChange"
            ],
            "{log:?}"
        );
        let change = &log[2]["params"];
        assert_eq!(change["textDocument"]["version"], 2);
        assert!(change["textDocument"]["uri"]
            .as_str()
            .unwrap()
            .ends_with("/src/other.rs"));
        assert_eq!(
            change["contentChanges"],
            json!([{"text": "pub fn o() {}\npub fn p() {}\n"}])
        );
        assert!(!methods.contains(&"textDocument/didSave"));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn output_formatting() {
        let cwd = tmp("format");
        write(&cwd.join("a.rs"), "fn a() {\n    b();\n    b();\n}\n");
        write(&cwd.join("b.rs"), "pub fn b() {}\n");
        let mut src = Src::new(&cwd);
        let a = norm(&cwd.join("a.rs"));
        let b = norm(&cwd.join("b.rs"));
        let loc = |p: &Path, line, ch| Loc {
            path: p.to_path_buf(),
            line,
            ch,
        };
        // LocationLink and Location both parse; duplicates collapse.
        let parsed = parse_locations(&json!([
            {"targetUri": path_to_uri(&b), "targetRange": {"start":{"line":0,"character":0}},
             "targetSelectionRange": {"start":{"line":0,"character":7}}},
            {"uri": path_to_uri(&b), "range": {"start":{"line":0,"character":7}}},
        ]));
        assert_eq!(parsed, vec![loc(&b, 0, 7)]);
        assert!(parse_locations(&Value::Null).is_empty());

        let refs = vec![loc(&b, 0, 7), loc(&a, 2, 4), loc(&a, 1, 4)];
        let out = format_references(&refs, "`b` at b.rs:1", &mut src, "");
        assert_eq!(
            out,
            "3 references to `b` at b.rs:1 in 2 files:\na.rs (2)\n  2:5  b();\n  3:5  b();\nb.rs (1)\n  1:8  pub fn b() {}"
        );

        assert_eq!(
            hover_text(&json!([{"language":"rust","value":"fn b()"}, "Plain docs."])),
            "```rust\nfn b()\n```\n\nPlain docs."
        );

        let tree = json!([{"name":"Outer","kind":23,"range":{"start":{"line":0}},"selectionRange":{"start":{"line":0}},
            "children":[{"name":"go","kind":6,"range":{"start":{"line":3}},"selectionRange":{"start":{"line":4}}}]}]);
        assert_eq!(
            format_document_symbols(&tree, "a.rs", true),
            "Symbols in a.rs:\nstruct Outer  :1\n  method go  :5"
        );
        let flat = json!([{"name":"f","kind":12,"containerName":"m","location":{"uri":path_to_uri(&a),"range":{"start":{"line":9}}}}]);
        assert_eq!(
            format_document_symbols(&flat, "a.rs", false),
            "Symbols in a.rs:\nfunction f (in m)  :10\n(server still indexing; results may be incomplete)"
        );
        assert!(format_document_symbols(&json!([]), "a.rs", false).contains("still indexing"));

        let ws = json!([{"name":"b","kind":12,"location":{"uri":path_to_uri(&b),"range":{"start":{"line":0,"character":7}}}},
                        {"name":"B","kind":5,"location":{"uri":path_to_uri(&b)}}]);
        assert_eq!(
            format_workspace_symbols(&ws, "b", &mut src, true),
            "2 symbols matching `b`:\nfunction b  b.rs:1\nclass B  b.rs"
        );

        // Caps: at most MAX_ENTRIES entries, then a count of the rest.
        let many: Vec<String> = (0..MAX_ENTRIES + 5).map(|i| format!("e{i}")).collect();
        let out = finish("h".into(), many, "");
        assert!(out.ends_with("\n… 5 more"), "{}", &out[out.len() - 20..]);
        let huge: Vec<String> = (0..50).map(|_| "x".repeat(1000)).collect();
        assert!(finish(String::new(), huge, "").len() < MAX_CHARS + 20);

        // Locations outside the project stay absolute.
        let outside = tmp("format-outside");
        write(&outside.join("z.rs"), "z\n");
        let z = norm(&outside.join("z.rs"));
        assert_eq!(
            src.show(&loc(&z, 0, 0)),
            format!("{}:1:1  z", z.to_string_lossy())
        );
        let _ = std::fs::remove_dir_all(&cwd);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The runner spawns tool calls onto the runtime, so `run` must be `Send`.
    #[test]
    fn run_future_is_send() {
        fn assert_send<T: Send>(_: &T) {}
        let mgr = LspManager::default();
        let call = call_for(Op::Hover, None);
        let pick = ServerPick {
            id: "rust-analyzer",
            root: PathBuf::from("."),
            exe: PathBuf::from("x"),
            args: vec![],
            language_id: "rust",
        };
        let cancel = CancellationToken::new();
        let fut = mgr.run(Path::new("."), &call, &pick, &cancel);
        assert_send(&fut);
        assert_send(&mgr.reap_idle(Duration::ZERO));
        assert_send(&mgr.shutdown_all());
        assert_send(&mgr.running());
    }
}
