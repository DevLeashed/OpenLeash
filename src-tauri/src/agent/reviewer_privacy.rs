//! Release-gate tests for what leaves the machine as a *label*.
//!
//! `Target::label()` is the string the chat shows as "serving": it goes on
//! every reply (`item.data["via"]`), into the task's `serving` field, into the
//! "Now using …" / "is now on … (was …)" notices, and into the status panel.
//! All of those are things a user screenshots or pastes into a bug report, so
//! this string is effectively public output.
//!
//! It used to prefer `account.email` over `account.label`, which meant a Codex
//! or Claude sign-in address was printed in the transcript of every turn it
//! served. That is a privacy bug, not a styling preference: the address is
//! also the account's identity, it is persisted into chat exports, and it is
//! visible to anyone looking over the user's shoulder or reading a shared
//! screen. The label ("Codex 1 · max") already distinguishes pooled accounts,
//! which is the only job this string has.
//!
//! These tests are deliberately about the *string*, not the UI: the leak lived
//! in the backend formatter, so a frontend snapshot would not have caught the
//! regression that re-introduced it.

#![cfg(test)]

use super::providers::Target;
use super::store::Account;

fn account(email: &str, label: &str) -> Account {
    Account {
        id: "a1".into(),
        kind: "codex".into(),
        label: label.into(),
        email: email.into(),
        access_token: "tok".into(),
        ..Default::default()
    }
}

fn target(email: &str, label: &str) -> Target {
    Target {
        model_id: "codex/gpt-6-sol".into(),
        prov: "codex".into(),
        model: "gpt-6-sol".into(),
        account: Some(account(email, label)),
        api_key: None,
    }
}

#[test]
fn serving_label_never_contains_the_account_email() {
    let t = target("someone@example.com", "Codex 1 · max");
    let label = t.label();
    assert!(
        !label.contains("someone@example.com"),
        "the serving label leaked the account email: {label}"
    );
    // The whole point of the string is telling two pooled accounts apart, so
    // the label has to still be in there — a fix that just dropped the account
    // would pass the check above while losing the information.
    assert!(
        label.contains("Codex 1 · max"),
        "the serving label lost the account label: {label}"
    );
}

#[test]
fn serving_label_survives_a_masked_or_empty_email() {
    // A label-only fallback that emits "model · " when the account has no
    // label would read as a typo in the transcript.
    assert_eq!(target("", "").label(), "codex/gpt-6-sol");
    assert_eq!(target("someone@example.com", "").label(), "codex/gpt-6-sol");
}

#[test]
fn serving_label_for_an_api_key_still_shows_the_key_hint() {
    // The account branch changed; the key-pool branch must not have. `key_hint`
    // is already a truncated hint and never the whole key.
    let t = Target {
        model_id: "anthropic/claude-opus-5".into(),
        prov: "anthropic".into(),
        model: "claude-opus-5".into(),
        account: None,
        api_key: Some("sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789".into()),
    };
    let label = t.label();
    assert!(
        label.starts_with("anthropic/claude-opus-5 · key "),
        "{label}"
    );
    assert!(
        !label.contains("abcdefghijklmnopqrstuvwxyz0123456789"),
        "the full key leaked into the serving label: {label}"
    );
}
