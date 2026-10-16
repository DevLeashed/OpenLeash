//! Dispatch tests for remote-SSH tasks: the real `Agent::exec_inner` path,
//! against an in-memory SFTP server (and a local `sh` playing sshd for
//! commands). No ssh, no network.

// The `test_home` guard spans each test to serialize `OPENLEASH_HOME`.
#![allow(clippy::await_holding_lock)]

use super::*;
use crate::agent::ssh_session::tests::{
    has_sh, host_id, install_fs, install_local_sh, sample_fs, sh_path, Fs, Node,
};
use crate::agent::tests::{harness, task};

struct Fx {
    h: Arc<Harness>,
    fs: Fs,
    host: String,
    home: std::path::PathBuf,
    _home: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// A task whose project is `/proj` on an in-memory SFTP host.
fn fixture(perm: &str, plan: bool) -> Fx {
    let home = std::env::temp_dir().join(format!("ol-ssh-disp-{}", uuid::Uuid::new_v4()));
    let _home = store::test_home(&home);
    let host = host_id();
    let fs = sample_fs();
    install_fs(&host, fs.clone());
    let mut t = task("ssh", "unused/model");
    t.project = host::ssh_uri(&host, "/proj");
    t.cwd = t.project.clone();
    t.perm = perm.into();
    t.plan = plan;
    let (h, _) = harness(store::Settings::default(), vec![t]);
    Fx {
        h,
        fs,
        host,
        home,
        _home,
    }
}

fn agent(fx: &Fx) -> Agent {
    Agent {
        h: fx.h.clone(),
        task_id: "ssh".into(),
        sub: None,
        cancel: CancellationToken::new(),
    }
}

fn file(fs: &Fs, p: &str) -> Option<Vec<u8>> {
    match fs.lock().unwrap().get(p) {
        Some(Node::File(d, _)) => Some(d.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn ssh_write_inside_the_root_lands_on_the_remote_host() {
    let fx = fixture("auto", false);
    let a = agent(&fx);
    let out = a
        .exec_inner(
            "write_file",
            &json!({"path":"src/new.rs","content":"fn x() {}\n"}),
            None,
            "c",
        )
        .await
        .unwrap();
    assert!(out.starts_with("Created /proj/src/new.rs"), "{out}");
    assert_eq!(file(&fx.fs, "/proj/src/new.rs").unwrap(), b"fn x() {}\n");
}

/// `proj/link_out -> /etc`: lexically inside the root, physically not. The
/// write is refused on the resolved path, and nothing lands on the host.
#[tokio::test]
async fn ssh_write_through_a_remote_symlink_out_of_the_root_is_refused() {
    let fx = fixture("auto", false);
    let a = agent(&fx);
    for (path, landed) in [
        ("link_out/new.txt", "/etc/new.txt"),
        ("link_out/passwd", "/etc/passwd"),
        ("dangle", "/outside/new.txt"),
        ("linkdir/up/x", "/deep/a/x/x"),
    ] {
        let e = a
            .exec_inner(
                "write_file",
                &json!({"path":path,"content":"pwned"}),
                None,
                "c",
            )
            .await
            .unwrap_err();
        assert!(e.contains("outside this project"), "{path}: {e}");
        assert!(e.contains(landed), "{path}: {e}");
        assert!(
            file(&fx.fs, landed).as_deref() != Some(&b"pwned".to_vec()[..]),
            "{path} wrote {landed}"
        );
    }
    assert_eq!(file(&fx.fs, "/etc/passwd").unwrap(), b"root:x:0:0\n");
    assert_eq!(file(&fx.fs, "/proj/link_out/new.txt"), None);
}

/// `..` and another host's URI never reach outside the project either.
#[tokio::test]
async fn ssh_traversal_and_other_hosts_never_reach_outside_the_root() {
    let fx = fixture("auto", false);
    let a = agent(&fx);
    let own = format!("ssh://{}/etc/evil", fx.host);
    for path in [
        "../etc/evil",
        "src/../../outside/evil",
        "/etc/evil",
        own.as_str(),
        "src/../../../..",
    ] {
        let e = a
            .exec_inner("write_file", &json!({"path":path,"content":"x"}), None, "c")
            .await
            .unwrap_err();
        assert!(e.contains("outside this project"), "{path}: {e}");
    }
    assert!(file(&fx.fs, "/etc/evil").is_none() && file(&fx.fs, "/outside/evil").is_none());

    // Another host's URI is refused outright.
    let e = agent(&fx)
        .exec_inner(
            "write_file",
            &json!({"path":"ssh://otherhost/proj/x","content":"x"}),
            None,
            "c",
        )
        .await
        .unwrap_err();
    assert!(e.contains("different SSH host"), "{e}");
}

#[tokio::test]
async fn ssh_edit_requires_a_read_first_and_then_works() {
    let fx = fixture("auto", false);
    let a = agent(&fx);
    let edit = json!({"path":"a.txt","old_string":"world","new_string":"there"});
    let e = a
        .exec_inner("edit_file", &edit, None, "c")
        .await
        .unwrap_err();
    assert!(e.contains("You must read /proj/a.txt"), "{e}");
    let e = a
        .exec_inner(
            "write_file",
            &json!({"path":"a.txt","content":"x"}),
            None,
            "c",
        )
        .await
        .unwrap_err();
    assert!(e.contains("must read"), "{e}");
    assert_eq!(file(&fx.fs, "/proj/a.txt").unwrap(), b"hello\nworld\n");
    let read = a
        .exec_inner("read_file", &json!({"path":"a.txt"}), None, "c")
        .await
        .unwrap();
    assert!(read.contains("     2→world"), "{read}");
    let out = a.exec_inner("edit_file", &edit, None, "c").await.unwrap();
    assert!(out.starts_with("Updated /proj/a.txt"), "{out}");
    assert_eq!(file(&fx.fs, "/proj/a.txt").unwrap(), b"hello\nthere\n");
    // The record is refreshed by the write: a second edit needs no re-read.
    let edit2 = json!({"path":"/proj/a.txt","old_string":"hello","new_string":"hi"});
    a.exec_inner("edit_file", &edit2, None, "c").await.unwrap();
    assert_eq!(file(&fx.fs, "/proj/a.txt").unwrap(), b"hi\nthere\n");
    // A missing file can't be edited, only created.
    let e = a
        .exec_inner(
            "edit_file",
            &json!({"path":"nope.txt","old_string":"a","new_string":"b"}),
            None,
            "c",
        )
        .await
        .unwrap_err();
    assert!(e.contains("does not exist"), "{e}");
    // Reading through a symlink and editing the target share one record.
    a.exec_inner("read_file", &json!({"path":"inner/main.rs"}), None, "c")
        .await
        .unwrap();
    a.exec_inner(
        "edit_file",
        &json!({"path":"src/main.rs","old_string":"main","new_string":"start"}),
        None,
        "c",
    )
    .await
    .unwrap();
    assert_eq!(
        file(&fx.fs, "/proj/src/main.rs").unwrap(),
        b"fn start() {}\n"
    );
}

/// The test server reports one mtime for every file, like an SFTP server
/// reporting whole seconds for two writes within the same second.
#[tokio::test]
async fn ssh_staleness_is_caught_by_content_hash_within_the_same_second() {
    let fx = fixture("auto", false);
    let a = agent(&fx);
    a.exec_inner("read_file", &json!({"path":"a.txt"}), None, "c")
        .await
        .unwrap();
    let changed = b"hello\nworld\nchanged by a command\n".to_vec();
    fx.fs
        .lock()
        .unwrap()
        .insert("/proj/a.txt".into(), Node::File(changed.clone(), 0o600));
    let e = a
        .exec_inner(
            "edit_file",
            &json!({"path":"a.txt","old_string":"hello","new_string":"bye"}),
            None,
            "c",
        )
        .await
        .unwrap_err();
    assert!(e.contains("was modified since you last read it"), "{e}");
    let e = a
        .exec_inner(
            "write_file",
            &json!({"path":"a.txt","content":"clobber"}),
            None,
            "c",
        )
        .await
        .unwrap_err();
    assert!(e.contains("modified since"), "{e}");
    assert_eq!(file(&fx.fs, "/proj/a.txt").unwrap(), changed);
}

#[tokio::test]
async fn ssh_plan_mode_and_read_only_subagents_still_block_writes() {
    let fx = fixture("turbo", true);
    let a = agent(&fx);
    let e = a
        .exec_inner(
            "write_file",
            &json!({"path":"plan.txt","content":"x"}),
            None,
            "c",
        )
        .await
        .unwrap_err();
    assert!(e.contains("Plan mode is active"), "{e}");
    let e = a
        .exec_inner("bash", &json!({"command":"touch x"}), None, "c")
        .await
        .unwrap_err();
    assert!(e.contains("Plan mode is active"), "{e}");
    assert!(file(&fx.fs, "/proj/plan.txt").is_none());
    fx.h.update_task("ssh", |t| t.plan = false).await;
    let sub = Agent {
        sub: Some(SubCtx {
            id: "w".into(),
            def: AgentDef {
                id: "explore".into(),
                tools: "read_only".into(),
                ..Default::default()
            },
        }),
        ..agent(&fx)
    };
    for (name, input) in [
        ("write_file", json!({"path":"ro.txt","content":"x"})),
        (
            "edit_file",
            json!({"path":"a.txt","old_string":"hello","new_string":"x"}),
        ),
        (
            "multi_edit",
            json!({"path":"a.txt","edits":[{"old_string":"hello","new_string":"x"}]}),
        ),
        ("bash", json!({"command":"rm -rf /proj"})),
        ("bash", json!({"command":"ls", "run_in_background": true})),
    ] {
        let e = sub.exec_inner(name, &input, None, "c").await.unwrap_err();
        assert!(e.contains("explore agent"), "{name}: {e}");
    }
    assert!(file(&fx.fs, "/proj/ro.txt").is_none());
    assert_eq!(file(&fx.fs, "/proj/a.txt").unwrap(), b"hello\nworld\n");
    sub.exec_inner("read_file", &json!({"path":"a.txt"}), None, "c")
        .await
        .unwrap();
}

#[tokio::test]
async fn ssh_local_only_features_fail_closed() {
    let fx = fixture("auto", false);
    let a = agent(&fx);
    let e = a
        .exec_inner("artifict_list", &json!({}), None, "c")
        .await
        .unwrap_err();
    assert!(e.contains("not yet supported for SSH projects"), "{e}");
    let cwd = host::ssh_uri(&fx.host, "/proj");
    assert!(browser_target("index.html", &cwd).is_err());
    for r in [
        checkpoint::capture_latest_turn("ssh", &cwd),
        checkpoint::ensure("ssh", &cwd),
        git::create_worktree(&cwd, "HEAD", "x").map(|_| ()),
        crate::artificts::list(&cwd).map(|_| ()),
    ] {
        assert!(r
            .unwrap_err()
            .contains("not yet supported for SSH projects"));
    }
    let e = a
        .exec_inner("view_image", &json!({"path":"a.txt"}), None, "c")
        .await
        .unwrap_err();
    assert!(e.contains("viewable image"), "{e}");
}

/// Commands go through the real job seam, with a local `sh` standing in for
/// sshd. The cwd and the command arrive byte-exact: nothing in them is
/// re-parsed by the shell that starts the job.
#[tokio::test]
async fn ssh_bash_quotes_cwd_and_commands() {
    if !has_sh() {
        eprintln!("skipping: no sh");
        return;
    }
    let fx = fixture("turbo", false);
    let dir = fx
        .home
        .join("we'ird $(touch PWNED) `touch PWNED2`; dir && x");
    std::fs::create_dir_all(&dir).unwrap();
    let rcwd = sh_path(&dir);
    assert!(!rcwd.is_empty());
    let host = host_id();
    install_local_sh(&host);
    let uri = host::ssh_uri(&host, &rcwd);
    fx.h.update_task("ssh", |t| {
        t.cwd = uri.clone();
        t.project = uri.clone();
    })
    .await;
    let a = agent(&fx);
    let cmd = "printf 'A%s|\\n' \"$(pwd)\" && echo B; echo C | tr C D; echo `echo E`\necho F";
    let out = a
        .exec_inner("bash", &json!({ "command": cmd }), None, "c")
        .await
        .unwrap();
    assert!(out.contains(&format!("A{rcwd}|")), "{out}");
    for want in ["B", "D", "E", "F"] {
        assert!(out.lines().any(|l| l == want), "{want:?} in {out:?}");
    }
    assert!(!dir.join("PWNED").exists() && !fx.home.join("PWNED").exists());
    assert!(!dir.join("PWNED2").exists() && !fx.home.join("PWNED2").exists());
    let e = a
        .exec_inner("bash", &json!({"command":"exit 3"}), None, "c")
        .await
        .unwrap_err();
    assert!(e.contains("[exit code 3]"), "{e}");
    // Background jobs start on the host, can be read, and end when killed.
    let out = a
        .exec_inner(
            "bash",
            &json!({"command":"echo started; sleep 30", "run_in_background": true}),
            None,
            "c",
        )
        .await
        .unwrap();
    assert!(out.contains("started"), "{out}");
    let id = out
        .split_whitespace()
        .nth(3)
        .unwrap()
        .trim_end_matches('.')
        .to_string();
    assert!(fx.h.bg.kill(&id));
    let job = fx.h.bg.get(&id).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), job.done.cancelled())
        .await
        .expect("the remote job ends when killed");
}
