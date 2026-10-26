//! Remote-SSH projects: the pure, synchronous guards around them — permission
//! containment for `ssh://` paths, per-host trust, the local-op guard that
//! keeps an `ssh://` string away from the local filesystem and shell, and the
//! quoting of remote git commands. The dispatch-level tests (writes through
//! remote symlinks, read-before-write, staleness, plan mode, read-only
//! subagents, command quoting) are in `runner_remote_tests.rs`.

use super::permissions::{self, Ctx, Decision};
use super::store::AllowRule;
use super::trust::{self, TrustKind, TrustState};
use super::{git, host, shell, tools};
use serde_json::json;
use std::path::Path;

fn auto(cwd: &str) -> Ctx<'_> {
    Ctx {
        perm: "auto",
        plan: false,
        cwd,
        project: cwd,
        allow: &[],
    }
}

fn write(path: &str, ctx: &Ctx) -> Decision {
    permissions::check(
        "write_file",
        &json!({"path": path, "content": "x"}),
        Some(path),
        ctx,
    )
}

/// POSIX paths are case-sensitive and a host is part of the path: an
/// `ssh://` write is inside the root only on the same host, at or below the
/// root, with the same case. (The local check folds case on Windows, which
/// made `ssh://h/proj` cover `ssh://h/PROJ/x`.)
#[test]
fn ssh_write_containment_is_per_host_and_case_sensitive() {
    let root = "ssh://h1/home/me/proj";
    let ctx = auto(root);
    for inside in [
        "ssh://h1/home/me/proj/a.rs",
        "ssh://h1/home/me/proj/src/deep/x.rs",
        "SSH://h1/home/me/proj/a.rs",
    ] {
        assert!(matches!(write(inside, &ctx), Decision::Allow), "{inside}");
    }
    for outside in [
        "ssh://h1/home/me/PROJ/a.rs",
        "ssh://h1/home/me/Proj/a.rs",
        "ssh://h2/home/me/proj/a.rs",
        "ssh://h1/home/me/proj2/a.rs",
        "ssh://h1/home/me/proj/../other/a.rs",
        "ssh://h1/etc/passwd",
        "/home/me/proj/a.rs",
        "home/me/proj/a.rs",
        "ssh:/h1/home/me/proj/a.rs",
    ] {
        match write(outside, &ctx) {
            Decision::Ask { reason, .. } => {
                assert!(reason.contains("outside"), "{outside}: {reason}")
            }
            _ => panic!("{outside}: expected an outside-the-project ask"),
        }
    }
    // A local root never contains a remote path, either way round.
    assert!(!permissions::inside_public("ssh://h1/x/y", "ssh:"));
    assert!(!permissions::inside_public(
        "ssh://h1/home/me/proj/a",
        "/home/me/proj"
    ));
    assert!(!permissions::inside_public("/home/me/proj/a", root));
    assert_eq!(
        permissions::short("ssh://h1/home/me/proj/src/a.rs", root),
        "src/a.rs"
    );
    assert_eq!(
        permissions::short("ssh://h1/home/me/PROJ/a.rs", root),
        "ssh://h1/home/me/PROJ/a.rs"
    );
}

/// Plan mode and the Disabled rung refuse remote writes like local ones.
#[test]
fn ssh_plan_mode_and_disabled_rung_refuse_writes() {
    let root = "ssh://h1/p";
    let plan = Ctx {
        plan: true,
        ..auto(root)
    };
    assert!(matches!(write("ssh://h1/p/a", &plan), Decision::Deny(m) if m.contains("Plan mode")));
    let off = Ctx {
        perm: "disabled",
        ..auto(root)
    };
    assert!(matches!(write("ssh://h1/p/a", &off), Decision::Deny(_)));
}

#[test]
fn ssh_file_regex_restriction_uses_the_remote_relative_path() {
    let root = "ssh://h1/p";
    let deny = |p: &str| {
        permissions::file_restriction_denial("docs", r"^docs/.*\.md$", "", "write_file", p, root)
    };
    assert!(deny("ssh://h1/p/docs/a.md").is_none());
    for bad in [
        "ssh://h1/p/src/a.md",
        "ssh://h1/P/docs/a.md",
        "ssh://h2/p/docs/a.md",
        "ssh://h1/p/../p2/docs/a.md",
        "ssh://h1/p",
    ] {
        assert!(deny(bad).is_some(), "{bad}");
    }
}

/// The local file tools and the local shell refuse an `ssh://` path instead
/// of treating it as a (relative) local one.
#[test]
fn local_ops_refuse_ssh_paths() {
    let cwd = "ssh://h1/home/me/proj";
    let p = tools::resolve(cwd, "src/main.rs");
    for e in [
        tools::read_file(&p, None, None).unwrap_err(),
        tools::view_image(&p).err().unwrap(),
        tools::glob(Path::new(cwd), "*.rs").unwrap_err(),
        tools::grep(Path::new(cwd), &json!({"pattern":"x"})).unwrap_err(),
        tools::local_guard(Path::new("SSH://h1/x")).unwrap_err(),
    ] {
        assert!(e.contains("remote (SSH) path"), "{e}");
    }
    assert!(tools::local_guard(Path::new("src/main.rs")).is_ok());
    assert!(tools::local_guard(Path::new("C:/code/sshx/a.rs")).is_ok());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let e = rt
        .block_on(shell::run(
            "echo hi",
            cwd,
            5_000,
            &tokio_util::sync::CancellationToken::new(),
        ))
        .err()
        .unwrap();
    assert!(e.contains("remote (SSH) folder"), "{e}");
    let bg = shell::BgManager::default();
    assert!(bg.spawn("t", "", "echo hi", cwd).is_err());
    for refusal in [
        host::refuse(cwd, "Checkpoints are"),
        git::populate_worktree(cwd, "C:/x").map(|_| ()),
    ] {
        assert!(refusal
            .unwrap_err()
            .contains("not yet supported for SSH projects"));
    }
    assert!(host::refuse("C:/code/app", "x").is_ok());
}

/// Trust is per host and case-sensitive for remote folders.
#[test]
fn ssh_trust_is_per_host_and_keeps_case() {
    let mut v = vec![];
    trust::upsert(
        &mut v,
        "ssh://hostA/home/me",
        TrustKind::Parent,
        TrustState::Trusted,
    );
    assert!(trust::trusted(&v, "ssh://hostA/home/me/app"));
    assert!(trust::trusted(&v, "SSH://hostA/home/me/app/"));
    assert!(!trust::trusted(&v, "ssh://hostB/home/me/app"));
    assert!(!trust::trusted(&v, "ssh://hosta/home/me/app"));
    assert!(!trust::trusted(&v, "ssh://hostA/home/Me/app"));
    assert!(!trust::trusted(&v, "/home/me/app"));
    assert!(!trust::trusted(&v, "ssh://hostA/home/me2/app"));
    // A local decision never covers a remote folder.
    let mut v = vec![];
    trust::upsert(&mut v, "/home/me", TrustKind::Parent, TrustState::Trusted);
    assert!(!trust::trusted(&v, "ssh://hostA/home/me/app"));
    assert_eq!(
        trust::norm_path("ssh://hostA/x//y/../Z/"),
        "ssh://hostA/x/Z"
    );
    assert_eq!(
        trust::parent_dir("ssh://hostA/home/me/app").as_deref(),
        Some("ssh://hostA/home/me")
    );
    assert_eq!(trust::parent_dir("ssh://hostA/home"), None);
}

/// Remote git arguments are one quoted word each, whatever they contain.
#[test]
fn remote_git_arguments_are_quoted() {
    let c = git::remote_git_command(&[
        "commit",
        "-m",
        "fix: it's $(rm -rf /) `id`; a && b | c\nnext",
    ]);
    assert_eq!(
        c,
        "git 'commit' '-m' 'fix: it'\\''s $(rm -rf /) `id`; a && b | c\nnext'"
    );
}

#[test]
fn deny_rules_still_apply_to_remote_reads() {
    let allow = [AllowRule {
        pattern: "!read_file *.pem".into(),
        project: String::new(),
    }];
    let ctx = Ctx {
        allow: &allow,
        ..auto("ssh://h1/p")
    };
    for (path, denied) in [
        ("ssh://h1/p/key.pem", true),
        ("ssh://h1/p/.env", true),
        ("ssh://h1/p/a.rs", false),
    ] {
        let d = permissions::check("read_file", &json!({"path": path}), Some(path), &ctx);
        assert_eq!(matches!(d, Decision::Deny(_)), denied, "{path}");
    }
}
