//! `http_request` is the one built-in tool that can *write* to the network:
//! POST to an API, fire a webhook, poke a local dev server. That makes it two
//! security problems at once.
//!
//! * A permission problem. A GET to a public host is a web read and runs like
//!   `web_fetch`; anything else changes something somewhere, so it asks below
//!   Full Access, plan mode refuses it, and an "always allow" must cover one
//!   method on one host — never `api.example.com.evil.com`, never a userinfo
//!   trick like `https://api.example.com@evil.com/`.
//! * An SSRF problem. The model picks the URL, so the cloud metadata service,
//!   link-local addresses and their many spellings (decimal, hex, octal,
//!   IPv4-mapped IPv6) are refused on every rung, a loopback/private target is
//!   only ever reached through an explicit approval below Full Access, and
//!   redirects and DNS answers are re-checked rather than trusted.
//!
//! The permission tests call the pure `permissions::check`; the network tests
//! use pure helpers plus a hand-rolled server bound to 127.0.0.1 (the same
//! pattern `trace.rs` uses). Nothing here touches the public internet.

#![cfg(test)]

use super::http_tool::{
    check_resolved, classify_url, execute, parse_url, redact_input, redirect_step, rule_key,
    subagent_refusal, Hop, Options, Target,
};
use super::permissions::{check, doom_loop_decision, is_read_only_tool, is_web_read_tool};
use super::permissions::{Ctx, Decision};
use super::store::AllowRule;
use serde_json::{json, Value};
use std::net::IpAddr;

const CWD: &str = if cfg!(windows) { r"C:\repo" } else { "/repo" };
const OTHER_PROJECT: &str = if cfg!(windows) { r"C:\other" } else { "/other" };

/// Every rung, under every spelling the permission ladder accepts.
const RUNGS: [&str; 6] = ["disabled", "ask", "reviewer", "auto", "turbo", "full"];

fn ctx<'a>(perm: &'a str, plan: bool, allow: &'a [AllowRule]) -> Ctx<'a> {
    Ctx {
        perm,
        plan,
        cwd: CWD,
        project: CWD,
        allow,
    }
}

fn rule(p: &str) -> AllowRule {
    AllowRule {
        pattern: p.into(),
        project: String::new(),
    }
}

fn req(method: &str, url: &str) -> Value {
    json!({"method": method, "url": url})
}

fn decide(perm: &str, plan: bool, allow: &[AllowRule], method: &str, url: &str) -> Decision {
    check(
        "http_request",
        &req(method, url),
        None,
        &ctx(perm, plan, allow),
    )
}

fn is_allow(d: &Decision) -> bool {
    matches!(d, Decision::Allow)
}
fn is_ask(d: &Decision) -> bool {
    matches!(d, Decision::Ask { .. })
}
fn is_deny(d: &Decision) -> bool {
    matches!(d, Decision::Deny(_))
}
fn show(d: &Decision) -> String {
    match d {
        Decision::Allow => "Allow".into(),
        Decision::Ask {
            title,
            rule,
            detail,
            ..
        } => format!("Ask({title}; rule={rule:?}; detail={detail})"),
        Decision::Deny(r) => format!("Deny({r})"),
    }
}

// ───────────────────────────── classification ─────────────────────────────

#[test]
fn it_is_neither_a_read_only_tool_nor_batched_as_a_web_read() {
    // `is_read_only_tool` short-circuits `check` to Allow on every rung, and
    // `is_web_read_tool` runs the call in a concurrent batch. A POST must get
    // neither.
    assert!(!is_read_only_tool("http_request"));
    assert!(!is_web_read_tool("http_request"));
}

// ───────────────────────────── permission matrix ─────────────────────────────

#[test]
fn a_get_or_head_to_a_public_host_is_a_web_read_on_every_rung() {
    for perm in RUNGS {
        for plan in [false, true] {
            for method in ["GET", "HEAD", "get"] {
                let d = decide(
                    perm,
                    plan,
                    &[],
                    method,
                    "https://api.example.com/v1/items?page=2",
                );
                assert!(is_allow(&d), "{perm} plan={plan} {method}: {}", show(&d));
            }
        }
    }
}

#[test]
fn a_post_to_a_public_host_asks_below_full_access() {
    for perm in ["ask", "reviewer", "auto", "allowlist"] {
        let d = decide(perm, false, &[], "POST", "https://api.example.com/v1/items");
        match &d {
            Decision::Ask { title, rule, .. } => {
                assert_eq!(title, "HTTP POST api.example.com");
                assert_eq!(
                    rule.as_deref(),
                    Some("http_request:POST https://api.example.com:443")
                );
            }
            other => panic!("{perm}: {}", show(other)),
        }
    }
    for method in ["PUT", "PATCH", "DELETE", "OPTIONS"] {
        let d = decide("ask", false, &[], method, "https://api.example.com/x");
        assert!(is_ask(&d), "{method}: {}", show(&d));
    }
    for perm in ["turbo", "full", ""] {
        let d = decide(perm, false, &[], "POST", "https://api.example.com/v1/items");
        assert!(
            is_allow(&d),
            "Full Access allows public writes: {}",
            show(&d)
        );
    }
    let d = decide(
        "disabled",
        false,
        &[],
        "POST",
        "https://api.example.com/v1/items",
    );
    assert!(
        is_deny(&d),
        "Disabled refuses instead of asking: {}",
        show(&d)
    );
}

#[test]
fn plan_mode_refuses_every_non_read_method_on_every_rung() {
    let allow = [rule("http_request:POST https://api.example.com:443")];
    for perm in RUNGS {
        for method in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            let d = decide(perm, true, &allow, method, "https://api.example.com/x");
            assert!(is_deny(&d), "{perm} {method}: {}", show(&d));
            if let Decision::Deny(r) = &d {
                assert!(r.contains("Plan mode"), "{r}");
            }
        }
    }
}

#[test]
fn an_allow_rule_covers_exactly_one_method_on_exactly_one_host() {
    let allow = [rule("http_request:POST https://api.example.com:443")];
    let d = |m: &str, u: &str| decide("ask", false, &allow, m, u);
    assert!(is_allow(&d("POST", "https://api.example.com/v1/items")));
    assert!(is_allow(&d("post", "https://API.Example.com/other?x=1")));
    // Same host, other method.
    assert!(is_ask(&d("PUT", "https://api.example.com/v1/items")));
    assert!(is_ask(&d("DELETE", "https://api.example.com/v1/items")));
    // Look-alike hosts.
    for url in [
        "https://api.example.com.evil.com/",
        "https://evil.com/api.example.com",
        "https://sub.api.example.com/",
        "https://example.com/",
        "https://evil.com/?h=api.example.com",
        "https://api.example.com:8443/",
    ] {
        let got = d("POST", url);
        assert!(is_ask(&got), "{url} must not ride the rule: {}", show(&got));
    }
    // Userinfo: the host is evil.com, and credentials in a URL are refused
    // outright rather than parsed generously.
    for url in [
        "https://api.example.com@evil.com/",
        "https://api.example.com:443@evil.com/",
        "https://user:pw@api.example.com/",
    ] {
        let got = d("POST", url);
        assert!(is_deny(&got), "{url}: {}", show(&got));
    }
    // A rule for another project does not apply here.
    let scoped = [AllowRule {
        pattern: "http_request:POST https://api.example.com:443".into(),
        project: OTHER_PROJECT.into(),
    }];
    let got = decide("ask", false, &scoped, "POST", "https://api.example.com/");
    assert!(is_ask(&got), "{}", show(&got));
}

#[test]
fn no_blanket_allow_rule_covers_writes() {
    for p in [
        "http_request",
        "http_request *",
        "http_request:*",
        "http_request:POST *",
        "http_request:* api.example.com",
        "http_request:POST api.example.*",
        "*",
    ] {
        let allow = [rule(p)];
        let d = decide("ask", false, &allow, "POST", "https://api.example.com/x");
        assert!(
            is_ask(&d),
            "`{p}` must not be a blanket grant: {}",
            show(&d)
        );
    }
}

#[test]
fn scheme_and_effective_port_are_part_of_allow_rules() {
    let https = parse_url("https://localhost:443/").unwrap();
    let http = parse_url("http://localhost:80/").unwrap();
    assert_ne!(
        rule_key("GET", &https),
        rule_key("GET", &http),
        "the same hostname at TLS :443 and plaintext :80 can be different services"
    );
}

#[test]
fn the_offered_rule_names_the_port_when_it_is_not_the_default() {
    let d = decide("ask", false, &[], "POST", "http://localhost:3000/api/login");
    match &d {
        Decision::Ask { title, rule, .. } => {
            assert_eq!(title, "HTTP POST localhost:3000");
            assert_eq!(
                rule.as_deref(),
                Some("http_request:POST http://localhost:3000")
            );
        }
        other => panic!("{}", show(other)),
    }
    let u = parse_url("https://api.example.com:443/x").unwrap();
    assert_eq!(
        rule_key("post", &u),
        "http_request:POST https://api.example.com:443"
    );
    let u = parse_url("http://api.example.com/x").unwrap();
    assert_eq!(
        rule_key("post", &u),
        "http_request:POST http://api.example.com:80"
    );
    let u = parse_url("http://[::1]:8080/").unwrap();
    assert_eq!(rule_key("GET", &u), "http_request:GET http://[::1]:8080");
}

#[test]
fn a_deny_rule_wins_over_allow_rules_reads_and_full_access() {
    let allow = [
        rule("http_request:POST https://api.example.com:443"),
        rule("!http_request:POST api.example.com"),
    ];
    for perm in RUNGS {
        let d = decide(perm, false, &allow, "POST", "https://api.example.com/x");
        assert!(is_deny(&d), "{perm}: {}", show(&d));
    }
    // The method-specific deny leaves GET alone.
    let d = decide("turbo", false, &allow, "GET", "https://api.example.com/x");
    assert!(is_allow(&d), "{}", show(&d));
    // A host-wide deny catches the read too, case-insensitively.
    let allow = [rule("!http_request:* API.example.com")];
    for m in ["GET", "HEAD", "POST"] {
        let d = decide("turbo", false, &allow, m, "https://api.example.com/x");
        assert!(is_deny(&d), "{m}: {}", show(&d));
    }
    let d = decide("turbo", false, &allow, "GET", "https://other.example.com/x");
    assert!(is_allow(&d), "{}", show(&d));
    // Method-wide and tool-wide denies.
    let allow = [rule("!http_request:DELETE *")];
    assert!(is_deny(&decide(
        "turbo",
        false,
        &allow,
        "DELETE",
        "https://a.example.org/"
    )));
    assert!(is_allow(&decide(
        "turbo",
        false,
        &allow,
        "POST",
        "https://a.example.org/"
    )));
    for p in ["!http_request", "!http_request *"] {
        let allow = [rule(p)];
        let d = decide("turbo", false, &allow, "GET", "https://api.example.com/");
        assert!(is_deny(&d), "{p}: {}", show(&d));
    }
}

#[test]
fn loopback_and_private_targets_ask_below_full_access_and_run_on_full_access() {
    for url in [
        "http://localhost:3000/api/health",
        "http://LOCALHOST:3000/",
        "http://app.localhost/",
        "http://127.0.0.1:8080/",
        "http://127.1.2.3/",
        "http://[::1]:9000/",
        "http://[::ffff:127.0.0.1]/",
        "http://0.0.0.0:3000/",
        "http://2130706433/", // 127.0.0.1 in decimal
        "http://10.0.0.5/",
        "http://172.16.3.4/",
        "http://192.168.1.1/",
        "http://100.64.0.1/",
        "http://[fd12:3456::1]/",
        "http://intranet/",
        "http://printer.local/",
        "http://wiki.corp.internal/",
    ] {
        for perm in ["ask", "reviewer", "auto"] {
            for method in ["GET", "HEAD", "POST"] {
                let d = decide(perm, false, &[], method, url);
                assert!(is_ask(&d), "{perm} {method} {url}: {}", show(&d));
            }
        }
        // Full Access is the "ask me nothing" mode: every target class runs.
        for perm in ["turbo", "full", ""] {
            for method in ["GET", "HEAD", "POST"] {
                let d = decide(perm, false, &[], method, url);
                assert!(is_allow(&d), "{perm} {method} {url}: {}", show(&d));
            }
        }
        // Plan mode: a local read still asks; a local write is refused.
        assert!(is_ask(&decide("ask", true, &[], "GET", url)), "{url}");
        assert!(is_deny(&decide("turbo", true, &[], "POST", url)), "{url}");
        // Disabled refuses rather than asks.
        assert!(
            is_deny(&decide("disabled", false, &[], "GET", url)),
            "{url}"
        );
    }
    // An explicit, per-host+port+method rule is how a dev server stops asking
    // on the rungs that ask.
    let allow = [rule("http_request:GET http://localhost:3000")];
    assert!(is_allow(&decide(
        "ask",
        false,
        &allow,
        "GET",
        "http://localhost:3000/x"
    )));
    assert!(is_ask(&decide(
        "ask",
        false,
        &allow,
        "GET",
        "http://localhost:3001/x"
    )));
    assert!(is_ask(&decide(
        "ask",
        false,
        &allow,
        "POST",
        "http://localhost:3000/x"
    )));
    assert!(is_ask(&decide(
        "ask",
        false,
        &allow,
        "GET",
        "http://127.0.0.1:3000/x"
    )));

    // A rule created for HTTPS :443 cannot authorize HTTP :80 on the same
    // hostname: these may be different local services.
    let secure = [rule("http_request:GET https://localhost:443")];
    assert!(is_allow(&decide(
        "ask",
        false,
        &secure,
        "GET",
        "https://localhost/"
    )));
    assert!(is_ask(&decide(
        "ask",
        false,
        &secure,
        "GET",
        "http://localhost/"
    )));
    let post = [rule("http_request:POST https://api.example.com:443")];
    assert!(is_allow(&decide(
        "ask",
        false,
        &post,
        "POST",
        "https://api.example.com/"
    )));
    assert!(is_ask(&decide(
        "ask",
        false,
        &post,
        "POST",
        "http://api.example.com/"
    )));
}

#[test]
fn the_reason_line_says_why_a_local_call_asks() {
    match decide("auto", false, &[], "GET", "http://localhost:3000/") {
        Decision::Ask { reason, .. } => {
            assert!(reason.to_lowercase().contains("this machine"), "{reason}")
        }
        d => panic!("{}", show(&d)),
    }
    match decide("auto", false, &[], "GET", "http://192.168.1.1/") {
        Decision::Ask { reason, .. } => {
            assert!(
                reason.to_lowercase().contains("private network"),
                "{reason}"
            )
        }
        d => panic!("{}", show(&d)),
    }
}

/// Every spelling of the cloud metadata service and of link-local space.
const METADATA: &[&str] = &[
    "http://169.254.169.254/latest/meta-data/",
    "http://169.254.170.2/v2/credentials", // ECS task credentials
    "http://[fd00:ec2::254]/latest/meta-data/",
    "http://metadata.google.internal/computeMetadata/v1/",
    "http://METADATA.GOOGLE.INTERNAL./",
    "http://metadata.goog/",
    "http://instance-data/latest/",
    "http://2852039166/",               // decimal
    "http://0xA9FEA9FE/",               // hex
    "http://0xa9.0xfe.0xa9.0xfe/",      // dotted hex
    "http://0251.0376.0251.0376/",      // octal
    "http://169.254.43518/",            // short form
    "http://[::ffff:169.254.169.254]/", // v4-mapped
    "http://[::ffff:a9fe:a9fe]/",       // v4-mapped, hex
    "http://[64:ff9b::a9fe:a9fe]/",     // NAT64
    "http://[2002:a9fe:a9fe::1]/",      // 6to4
    "http://[fe80::1]/",
    "http://100.100.100.200/latest/meta-data/", // Alibaba
    "http://192.0.0.192/",                      // Oracle
    "http://224.0.0.1/",                        // multicast
    "http://255.255.255.255/",                  // broadcast
];

#[test]
fn metadata_and_link_local_targets_are_refused_on_every_rung_whatever_the_rules() {
    let allow = [
        rule("http_request:GET 169.254.169.254"),
        rule("http_request:GET metadata.google.internal"),
        rule("http_request *"),
        rule("http_request"),
    ];
    for url in METADATA {
        for perm in RUNGS {
            for plan in [false, true] {
                for method in ["GET", "HEAD", "POST", "PUT"] {
                    let d = decide(perm, plan, &allow, method, url);
                    assert!(
                        is_deny(&d),
                        "{perm} plan={plan} {method} {url}: {}",
                        show(&d)
                    );
                }
            }
        }
    }
}

#[test]
fn classification_normalises_every_ip_spelling() {
    for url in METADATA {
        let u = parse_url(url).unwrap_or_else(|e| panic!("{url}: {e}"));
        assert_eq!(classify_url(&u), Target::Blocked, "{url}");
    }
    for (url, want) in [
        ("https://example.com/", Target::Public),
        ("http://93.184.216.34/", Target::Public),
        ("https://[2606:4700:4700::1111]/", Target::Public),
        ("http://localhost/", Target::Loopback),
        ("http://127.0.0.1/", Target::Loopback),
        ("http://0x7f000001/", Target::Loopback),
        ("http://017700000001/", Target::Loopback),
        ("http://[::1]/", Target::Loopback),
        ("http://[::]/", Target::Loopback),
        ("http://0.0.0.0/", Target::Loopback),
        ("http://10.1.2.3/", Target::Private),
        ("http://[::ffff:10.1.2.3]/", Target::Private),
        ("http://wiki/", Target::Private),
    ] {
        let u = parse_url(url).unwrap();
        assert_eq!(classify_url(&u), want, "{url}");
    }
}

#[test]
fn malformed_requests_are_refused_not_asked_about() {
    for input in [
        json!({"method": "TRACE", "url": "https://api.example.com/"}),
        json!({"method": "CONNECT", "url": "https://api.example.com/"}),
        json!({"method": "GET"}),
        json!({"method": "GET", "url": "ftp://example.com/x"}),
        json!({"method": "GET", "url": "file:///etc/passwd"}),
        json!({"method": "GET", "url": "example.com/x"}),
        json!({"method": "GET", "url": "http://"}),
    ] {
        for perm in ["ask", "turbo"] {
            let d = check("http_request", &input, None, &ctx(perm, false, &[]));
            assert!(is_deny(&d), "{perm} {input}: {}", show(&d));
        }
    }
    // A missing method defaults to GET.
    let d = check(
        "http_request",
        &json!({"url": "https://api.example.com/"}),
        None,
        &ctx("ask", false, &[]),
    );
    assert!(is_allow(&d), "{}", show(&d));
}

// ───────────────────────────── redaction ─────────────────────────────

fn secret_input() -> Value {
    json!({
        "method": "POST",
        "url": "https://api.example.com/v1/items?access_token=SECRET_Q1&page=2&api_key=SECRET_Q2",
        "headers": {
            "Authorization": "Bearer SECRET_H1",
            "cookie": "session=SECRET_H2",
            "X-Api-Key": "SECRET_H3",
            "X-Auth-Token": "SECRET_H4",
            "Content-Type": "application/json",
            "Accept": "application/json"
        },
        "json": {"name": "widget", "password": "SECRET_B1", "nested": {"client_secret": "SECRET_B2", "ok": 1}}
    })
}

fn assert_no_secret(s: &str, wher: &str) {
    for n in 1..=4 {
        for p in ["Q", "H", "B"] {
            let needle = format!("SECRET_{p}{n}");
            assert!(!s.contains(&needle), "{wher} leaked {needle}:\n{s}");
        }
    }
}

#[test]
fn the_approval_card_shows_the_request_with_secrets_redacted() {
    let d = check(
        "http_request",
        &secret_input(),
        None,
        &ctx("ask", false, &[]),
    );
    let Decision::Ask {
        title,
        detail,
        reason,
        rule,
    } = d
    else {
        panic!("POST must ask")
    };
    assert_eq!(title, "HTTP POST api.example.com");
    assert_eq!(
        rule.as_deref(),
        Some("http_request:POST https://api.example.com:443")
    );
    assert!(!reason.is_empty());
    assert_no_secret(&detail, "approval detail");
    assert_no_secret(&title, "approval title");
    assert!(
        detail.contains("POST https://api.example.com/v1/items"),
        "{detail}"
    );
    assert!(
        detail.contains("page=2"),
        "non-secret params stay: {detail}"
    );
    assert!(detail.contains("Bearer [redacted]"), "{detail}");
    assert!(detail.contains("application/json"), "{detail}");
    assert!(
        detail.contains("widget"),
        "non-secret body fields stay: {detail}"
    );
}

#[test]
fn the_transcript_copy_of_the_input_is_redacted() {
    let r = redact_input(&secret_input());
    let s = r.to_string();
    assert_no_secret(&s, "transcript input");
    assert_eq!(r["method"], "POST");
    assert_eq!(r["headers"]["Authorization"], "Bearer [redacted]");
    assert_eq!(r["headers"]["Accept"], "application/json");
    assert_eq!(r["json"]["name"], "widget");
    assert_eq!(r["json"]["nested"]["ok"], 1);
    // A string body: JSON and form encodings are redacted by key.
    let r = redact_input(
        &json!({"url": "https://a.example.org/", "body": "{\"token\":\"SECRET_B3\",\"q\":1}"}),
    );
    assert_no_secret(&r.to_string(), "json string body");
    let r = redact_input(
        &json!({"url": "https://a.example.org/", "body": "user=bob&password=SECRET_B4"}),
    );
    assert_no_secret(&r.to_string(), "form body");
    assert!(r["body"].as_str().unwrap().contains("user=bob"));
}

#[test]
fn the_repeated_call_card_is_redacted_too() {
    let d = doom_loop_decision("http_request", &secret_input(), 4).expect("asks at the threshold");
    match d {
        Decision::Ask { detail, .. } => assert_no_secret(&detail, "doom-loop detail"),
        d => panic!("{}", show(&d)),
    }
}

#[test]
fn the_reviewer_model_sees_a_redacted_request() {
    let p = super::review::user_prompt("http_request", &secret_input(), CWD, CWD);
    assert_no_secret(&p, "reviewer prompt");
    assert!(p.contains("api.example.com"));
}

#[tokio::test]
async fn the_timeline_row_stores_a_redacted_input() {
    use super::tests::{harness, task};
    use super::Item;
    let mut t = task("http-redact", "m/x");
    let item = Item::new("tool", "", json!({"name": "http_request"}));
    let iid = item.id.clone();
    t.items.push(item);
    let (h, _) = harness(Default::default(), vec![t]);
    let content = vec![
        json!({"type": "tool_use", "id": "tu1", "name": "http_request", "input": secret_input()}),
    ];
    super::runner::attach_inputs(
        &h,
        "http-redact",
        None,
        &content,
        &[("tu1".into(), iid.clone())],
    )
    .await;
    let task = h.task("http-redact").await.unwrap();
    let t = task.lock().await;
    let it = t.items.iter().find(|i| i.id == iid).unwrap();
    assert_no_secret(&it.data["input"].to_string(), "timeline input");
    assert_eq!(it.data["input"]["headers"]["Accept"], "application/json");
}

// ───────────────────────────── subagents ─────────────────────────────

#[test]
fn a_read_only_subagent_may_only_get_or_head() {
    for m in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE"] {
        let r = subagent_refusal("read_only", &req(m, "https://api.example.com/"));
        assert!(r.is_some(), "{m} must be refused for a read-only subagent");
    }
    for m in ["GET", "HEAD", "get"] {
        assert!(subagent_refusal("read_only", &req(m, "https://api.example.com/")).is_none());
    }
    assert!(subagent_refusal("read_only", &json!({"url": "https://a.example.org/"})).is_none());
    for policy in ["all", "no_shell", ""] {
        assert!(subagent_refusal(policy, &req("POST", "https://api.example.com/")).is_none());
    }
}

#[test]
fn the_schema_offers_reads_only_to_read_only_subagents_and_writes_to_the_rest() {
    let plugins = super::plugins::PluginsCfg::default();
    let methods = |sub: Option<&str>| -> Vec<String> {
        let v = super::tools::schemas(sub, &plugins);
        let t = v
            .iter()
            .find(|t| t["name"] == "http_request")
            .unwrap_or_else(|| panic!("{sub:?} has http_request"));
        t["input_schema"]["properties"]["method"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(methods(Some("read_only")), vec!["GET", "HEAD"]);
    for sub in [None, Some("all"), Some("no_shell")] {
        let m = methods(sub);
        for want in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            assert!(m.iter().any(|x| x == want), "{sub:?} lacks {want}");
        }
    }
    let main = super::tools::schemas(None, &plugins);
    let desc = main.iter().find(|t| t["name"] == "http_request").unwrap()["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(desc.contains("web_fetch"), "says when not to use it");
    assert!(desc.contains("approval"), "says non-GET needs approval");
}

// ───────────────────────────── DNS and redirects ─────────────────────────────

fn ips(v: &[&str]) -> Vec<IpAddr> {
    v.iter().map(|s| s.parse().unwrap()).collect()
}

#[test]
fn resolved_addresses_are_checked_against_what_was_approved() {
    // A public-looking name that resolves somewhere local is DNS rebinding or
    // an internal DNS entry: it was auto-approved as public, so it is refused.
    assert!(check_resolved(Target::Public, &ips(&["93.184.216.34"]), false).is_ok());
    assert!(check_resolved(Target::Public, &ips(&["127.0.0.1"]), false).is_err());
    assert!(check_resolved(Target::Public, &ips(&["93.184.216.34", "10.0.0.1"]), false).is_err());
    assert!(check_resolved(Target::Public, &ips(&["::ffff:192.168.0.1"]), false).is_err());
    assert!(check_resolved(Target::Public, &[], false).is_err());
    // Full Access approved every target class, so a local answer is allowed —
    // but only for the local classes, never for a blocked one.
    assert!(check_resolved(Target::Public, &ips(&["127.0.0.1"]), true).is_ok());
    assert!(check_resolved(Target::Public, &ips(&["10.0.0.1"]), true).is_ok());
    // An approved local target may resolve locally, never to metadata.
    assert!(check_resolved(Target::Loopback, &ips(&["127.0.0.1", "::1"]), false).is_ok());
    assert!(check_resolved(Target::Private, &ips(&["10.0.0.7"]), false).is_ok());
    for t in [Target::Public, Target::Loopback, Target::Private] {
        for allow_local in [false, true] {
            assert!(
                check_resolved(t, &ips(&["169.254.169.254"]), allow_local).is_err(),
                "{t:?} allow_local={allow_local}"
            );
            assert!(
                check_resolved(t, &ips(&["fd00:ec2::254"]), allow_local).is_err(),
                "{t:?} allow_local={allow_local}"
            );
        }
    }
}

fn hop(origin: &str, method: &str, status: u16, location: &str) -> Hop {
    let o = parse_url(origin).unwrap();
    redirect_step(&o, &o, method, status, location, false)
}

#[test]
fn redirects_into_private_or_metadata_space_are_refused() {
    for loc in [
        "http://127.0.0.1/admin",
        "http://localhost:8080/",
        "http://169.254.169.254/latest/meta-data/",
        "http://2852039166/",
        "http://[::ffff:169.254.169.254]/",
        "http://metadata.google.internal/",
        "http://192.168.1.1/",
        "http://intranet/",
    ] {
        match hop("https://api.example.com/start", "GET", 302, loc) {
            Hop::Refuse(_) => {}
            other => panic!("{loc}: {other:?}"),
        }
    }
    // Public to public is followed.
    match hop(
        "https://api.example.com/start",
        "GET",
        301,
        "https://cdn.example.net/x",
    ) {
        Hop::Follow {
            method, keep_body, ..
        } => {
            assert_eq!(method, "GET");
            assert!(!keep_body);
        }
        other => panic!("{other:?}"),
    }
    // Userinfo and non-http schemes are not followed.
    for loc in [
        "https://api.example.com@evil.com/",
        "file:///etc/passwd",
        "ftp://x.example.com/",
    ] {
        assert!(
            matches!(
                hop("https://api.example.com/", "GET", 302, loc),
                Hop::Refuse(_)
            ),
            "{loc}"
        );
    }
}

#[test]
fn an_approved_local_target_may_only_redirect_within_its_own_origin() {
    match hop("http://localhost:3000/login", "GET", 302, "/dashboard") {
        Hop::Follow { url, .. } => assert_eq!(url.as_str(), "http://localhost:3000/dashboard"),
        other => panic!("{other:?}"),
    }
    for loc in [
        "http://localhost:4000/",
        "http://127.0.0.1:3000/",
        "http://169.254.169.254/",
        "https://localhost:3000/",
    ] {
        assert!(
            matches!(
                hop("http://localhost:3000/login", "GET", 302, loc),
                Hop::Refuse(_)
            ),
            "{loc}"
        );
    }
}

#[test]
fn full_access_follows_a_redirect_into_local_space_but_never_into_metadata() {
    let o = parse_url("http://localhost:3000/login").unwrap();
    match redirect_step(&o, &o, "GET", 302, "http://127.0.0.1:4000/other", true) {
        Hop::Follow { url, .. } => assert_eq!(url.as_str(), "http://127.0.0.1:4000/other"),
        other => panic!("{other:?}"),
    }
    // A blocked target is out of reach on Full Access too.
    for loc in [
        "http://169.254.169.254/latest/meta-data/",
        "http://[fd00:ec2::254]/",
        "http://metadata.google.internal/",
    ] {
        assert!(
            matches!(redirect_step(&o, &o, "GET", 302, loc, true), Hop::Refuse(_)),
            "{loc}"
        );
    }
}

#[test]
fn a_write_is_never_resent_to_a_host_the_user_did_not_approve() {
    // 307/308 keep the method and body: cross-origin, that is a POST to a host
    // nobody approved, so it is handed back instead of followed.
    for status in [307, 308] {
        assert!(
            matches!(
                hop(
                    "https://api.example.com/x",
                    "POST",
                    status,
                    "https://other.example.com/y"
                ),
                Hop::Stop(_)
            ),
            "{status}"
        );
        match hop("https://api.example.com/x", "PUT", status, "/y") {
            Hop::Follow {
                method, keep_body, ..
            } => {
                assert_eq!(method, "PUT");
                assert!(keep_body);
            }
            other => panic!("{status}: {other:?}"),
        }
    }
    // 303 (and 301/302 after a POST) become a body-less GET, which is a read.
    for status in [301, 302, 303] {
        match hop(
            "https://api.example.com/x",
            "POST",
            status,
            "https://other.example.com/y",
        ) {
            Hop::Follow {
                method, keep_body, ..
            } => {
                assert_eq!(method, "GET");
                assert!(!keep_body);
            }
            other => panic!("{status}: {other:?}"),
        }
    }
    // HEAD stays HEAD.
    match hop("https://api.example.com/x", "HEAD", 303, "/y") {
        Hop::Follow { method, .. } => assert_eq!(method, "HEAD"),
        other => panic!("{other:?}"),
    }
}

// ───────────────────────────── end to end, on loopback ─────────────────────────────

/// A one-shot-per-connection HTTP server on 127.0.0.1. `route` maps the
/// request line (e.g. `GET /json HTTP/1.1`) plus the raw request to a full
/// response. Returns the base URL and the captured raw requests.
fn server(
    route: fn(&str, &str, u16) -> Vec<u8>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = l.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { break };
            let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // Read the head, then any Content-Length body.
            loop {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let raw = String::from_utf8_lossy(&buf).to_string();
            let line = raw.lines().next().unwrap_or("").to_string();
            s2.lock().unwrap().push(raw.clone());
            let _ = s.write_all(&route(&line, &raw, port));
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

fn resp(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut b = out.into_bytes();
    b.extend_from_slice(body);
    b
}

fn routes(line: &str, raw: &str, port: u16) -> Vec<u8> {
    let path = line.split_whitespace().nth(1).unwrap_or("/");
    match path {
        "/json" => resp(
            "200 OK",
            &[
                ("Content-Type", "application/json"),
                ("Set-Cookie", "sid=SECRET_SET"),
                ("X-Request-Id", "abc123"),
                ("X-Internal", "hidden"),
            ],
            br#"{"ok":true,"items":[1,2]}"#,
        ),
        "/echo" => resp(
            "201 Created",
            &[("Content-Type", "text/plain")],
            raw.as_bytes(),
        ),
        "/big" => resp(
            "200 OK",
            &[("Content-Type", "text/plain")],
            &vec![b'a'; 50_000],
        ),
        "/bin" => resp(
            "200 OK",
            &[("Content-Type", "application/octet-stream")],
            &[0u8, 159, 146, 150, 0, 1, 2, 255],
        ),
        "/to-metadata" => resp(
            "302 Found",
            &[("Location", "http://169.254.169.254/latest/meta-data/")],
            b"",
        ),
        "/to-decimal" => resp("302 Found", &[("Location", "http://2852039166/")], b""),
        "/to-other-port" => resp("302 Found", &[("Location", "http://127.0.0.1:9/")], b""),
        "/to-same" => resp(
            "302 Found",
            &[("Location", &format!("http://127.0.0.1:{port}/json"))],
            b"",
        ),
        "/loop" => resp("302 Found", &[("Location", "/loop")], b""),
        "/post-307" => resp("307 Temporary Redirect", &[("Location", "/echo")], b""),
        _ => resp("404 Not Found", &[], b"nope"),
    }
}

fn opts() -> Options<'static> {
    Options {
        keep_spill: false,
        allow_local: false,
        check_hop: &|_, _| Ok(()),
    }
}

#[test]
fn redirect_hops_reapply_the_effective_method_and_scoped_host_denies() {
    let origin = parse_url("https://a.example.com/start").unwrap();
    for (method, status, location, pattern, project, deny) in [
        (
            "GET",
            302,
            "https://b.example.com/end",
            "!http_request:* b.example.com",
            "",
            true,
        ),
        (
            "GET",
            302,
            "https://b.example.com/end",
            "!http_request:* b.example.com",
            CWD,
            true,
        ),
        (
            "GET",
            302,
            "https://b.example.com/end",
            "!http_request:* b.example.com",
            OTHER_PROJECT,
            false,
        ),
        // The effective request after POST + 302 is GET, not POST.
        (
            "POST",
            302,
            "https://b.example.com/end",
            "!http_request:GET b.example.com",
            "",
            true,
        ),
        (
            "POST",
            302,
            "https://b.example.com/end",
            "!http_request:POST b.example.com",
            "",
            false,
        ),
        (
            "GET",
            302,
            "https://b.example.com:8443/end",
            "!http_request:* b.example.com:8443",
            "",
            true,
        ),
        (
            "GET",
            302,
            "https://b.example.com.evil.com/end",
            "!http_request:* b.example.com",
            "",
            false,
        ),
        ("GET", 302, "/end", "!http_request", "", true),
        ("GET", 302, "/end", "!http_request *", "", true),
        (
            "POST",
            307,
            "/end",
            "!http_request:POST a.example.com",
            "",
            true,
        ),
    ] {
        let rules = [AllowRule {
            pattern: pattern.into(),
            project: project.into(),
        }];
        let ctx = ctx("turbo", false, &rules);
        let check = |m: &str, u: &reqwest::Url| {
            super::permissions::http_hop_denial(m, u, &ctx).map_or(Ok(()), Err)
        };
        let opts = Options {
            keep_spill: false,
            allow_local: false,
            check_hop: &check,
        };
        let hop = opts.redirect_step(&origin, &origin, method, status, location);
        assert_eq!(
            matches!(&hop, Hop::Refuse(_)),
            deny,
            "{method} {status} {location} {pattern}: {hop:?}"
        );
        if deny {
            let Hop::Refuse(reason) = hop else {
                unreachable!()
            };
            assert!(reason.contains("Denied by the rule"), "{reason}");
        } else {
            assert!(matches!(hop, Hop::Follow { .. }), "{hop:?}");
        }
    }
}

#[tokio::test]
async fn a_local_json_response_comes_back_pretty_with_selected_headers() {
    let (base, _) = server(routes);
    let out = execute(
        &json!({"method": "GET", "url": format!("{base}/json")}),
        &opts(),
    )
    .await
    .expect("loopback execution is allowed once the gate has approved it");
    assert!(out.starts_with("HTTP 200"), "{out}");
    assert!(out.contains("\"items\": ["), "pretty-printed JSON: {out}");
    assert!(
        out.to_lowercase()
            .contains("content-type: application/json"),
        "{out}"
    );
    assert!(
        out.contains("abc123"),
        "x-request-id is a selected header: {out}"
    );
    assert!(
        !out.contains("hidden"),
        "unlisted headers are dropped: {out}"
    );
    assert!(!out.contains("SECRET_SET"), "set-cookie is redacted: {out}");
}

#[tokio::test]
async fn a_post_sends_headers_and_json_and_reports_the_status() {
    let (base, seen) = server(routes);
    let out = execute(
        &json!({"method": "POST", "url": format!("{base}/echo"), "headers": {"X-Trace": "t1"}, "json": {"a": 1}}),
        &opts(),
    )
    .await
    .unwrap();
    assert!(out.starts_with("HTTP 201"), "{out}");
    let raw = seen.lock().unwrap().join("\n");
    assert!(raw.starts_with("POST /echo"), "{raw}");
    assert!(raw.to_lowercase().contains("x-trace: t1"), "{raw}");
    assert!(
        raw.to_lowercase()
            .contains("content-type: application/json"),
        "{raw}"
    );
    assert!(raw.contains(r#"{"a":1}"#), "{raw}");
}

#[tokio::test]
async fn a_redirect_into_metadata_space_is_refused_mid_chain() {
    let (base, _) = server(routes);
    for path in ["/to-metadata", "/to-decimal", "/to-other-port"] {
        let err = execute(
            &json!({"method": "GET", "url": format!("{base}{path}")}),
            &opts(),
        )
        .await
        .expect_err(path);
        assert!(err.to_lowercase().contains("redirect"), "{path}: {err}");
    }
    // Same-origin redirects on an approved local target are followed.
    let out = execute(
        &json!({"method": "GET", "url": format!("{base}/to-same")}),
        &opts(),
    )
    .await
    .unwrap();
    assert!(out.starts_with("HTTP 200"), "{out}");
    assert!(out.contains("redirected"), "{out}");
    // A loop stops at the hop limit rather than spinning.
    let err = execute(
        &json!({"method": "GET", "url": format!("{base}/loop")}),
        &opts(),
    )
    .await
    .expect_err("loop");
    assert!(err.to_lowercase().contains("redirect"), "{err}");
    // Not following is honoured.
    let out = execute(
        &json!({"method": "GET", "url": format!("{base}/to-metadata"), "follow_redirects": false}),
        &opts(),
    )
    .await
    .unwrap();
    assert!(out.starts_with("HTTP 302"), "{out}");
}

#[tokio::test]
async fn bodies_are_capped_and_binary_is_summarised() {
    let (base, _) = server(routes);
    let out = execute(
        &json!({"method": "GET", "url": format!("{base}/big"), "max_bytes": 1000}),
        &opts(),
    )
    .await
    .unwrap();
    assert!(out.contains("truncated"), "{out}");
    assert!(out.len() < 5_000, "the cap held: {}", out.len());
    let out = execute(
        &json!({"method": "GET", "url": format!("{base}/bin")}),
        &opts(),
    )
    .await
    .unwrap();
    assert!(out.contains("binary"), "{out}");
    assert!(out.contains("8 bytes"), "{out}");
}

#[tokio::test]
async fn execution_refuses_metadata_even_if_the_gate_were_skipped() {
    // Defence in depth: `execute` re-checks the target itself, so a dispatch
    // path that forgot the gate still cannot reach the metadata service.
    for url in [
        "http://169.254.169.254/latest/meta-data/",
        "http://0xA9FEA9FE/",
    ] {
        let err = execute(&json!({"method": "GET", "url": url}), &opts())
            .await
            .expect_err(url);
        assert!(err.to_lowercase().contains("refused"), "{url}: {err}");
    }
    let err = execute(
        &json!({"method": "GET", "url": "http://x.example.com/", "headers": {"Host": "evil"}}),
        &opts(),
    )
    .await
    .expect_err("host header");
    assert!(err.contains("Host"), "{err}");
}
