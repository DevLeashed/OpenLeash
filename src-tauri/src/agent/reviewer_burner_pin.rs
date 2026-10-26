//! Request-boundary regressions: a spare second account is never billed.
#![allow(clippy::await_holding_lock)]
use super::{
    accounts::{self, AcctUsage, Window},
    burner_policy::BurnPolicy,
    providers::ChatRequest,
    router::{self, Who},
    store::{self, Account, Settings},
    tests::{err, harness, openai_ok, task, Mock},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn burner_request_never_attempts_the_next_account() {
    let dir = std::env::temp_dir().join(format!("ol-burner-pin-{}", std::process::id()));
    let _home = store::test_home(&dir);
    let mock = Mock::start().await;
    let previous = std::env::var_os("OPENLEASH_CODEX_BASE");
    std::env::set_var("OPENLEASH_CODEX_BASE", &mock.base);
    let mut s = Settings::default();
    s.burner.enabled = true;
    s.accounts = ["a1", "a2"]
        .iter()
        .enumerate()
        .map(|(i, id)| Account {
            id: id.to_string(),
            kind: "codex".into(),
            enabled: true,
            access_token: format!("dummy-{id}"),
            priority: 10 - i as i32,
            ..Default::default()
        })
        .collect();
    let mut t = task("t1", "codex/gpt-5");
    t.burner = Some(BurnPolicy {
        account_id: "a1".into(),
        model: t.model.clone(),
        window: "5h".into(),
        resets_at: accounts::now() + 600,
        spend_cap: 50.0,
    });
    let (h, _) = harness(s, vec![t]);
    for id in ["a1", "a2"] {
        h.accts.set_usage(
            id,
            AcctUsage {
                fetched: accounts::now(),
                windows: vec![Window {
                    label: "5h".into(),
                    used: 10.0,
                    resets_at: accounts::now() + 600,
                }],
                ..Default::default()
            },
        );
    }
    mock.push(err(429, json!({"error":{"message":"usage limit reached"}})));
    mock.push(openai_ok("must not bill second account"));
    let cancel = CancellationToken::new();
    let who = Who {
        task_id: "t1",
        sub: None,
        side: true,
        turn: false,
        usage_id: "side",
    };
    let req = ChatRequest {
        system: "test".into(),
        messages: vec![],
        tools: vec![],
        effort: 2,
        max_tokens: 32,
        cache_key: "test".into(),
        cwd: String::new(),
    };
    let _ = router::request(&h, who, "codex/gpt-5", &req, &mut |_| {}, &cancel).await;
    let requests = mock.reqs();
    assert!(
        !requests.iter().any(|r| r.headers.contains("dummy-a2")),
        "second account was attempted: {:?}",
        requests
    );
    assert_eq!(
        requests.len(),
        1,
        "no fallback or retry once pinned account becomes unavailable"
    );
    match previous {
        Some(v) => std::env::set_var("OPENLEASH_CODEX_BASE", v),
        None => std::env::remove_var("OPENLEASH_CODEX_BASE"),
    };
}
