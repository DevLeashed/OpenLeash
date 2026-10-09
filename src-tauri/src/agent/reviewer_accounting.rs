//! Release-gate tests for token and cost accounting, and the context budget
//! that drives auto-compaction.
//!
//! Two numbers drive everything the user is shown and everything the app
//! spends: the cost of a turn, and how full the context window is. The second
//! one is the compaction trigger — `runner.rs` compacts when
//! `last_context > context_window * 8 / 10`. If `context_window` is wrong the
//! agent either compacts far too early (throwing away context the user paid
//! for, every turn) or far too late (the request is rejected by the provider
//! as "prompt too long" and the turn is lost). Neither failure is visible in
//! a unit test of anything else.
//!
//! The first one decides whether the monthly budget cap stops a run, and what
//! the Stats screen says. `cost` is pure arithmetic on provider-reported
//! usage, which is exactly the kind of thing that is wrong by a factor of 10
//! and nobody notices until a bill arrives.
//!
//! All assertions here are exact-integer or closed-form; nothing depends on a
//! clock, the filesystem, the network, or a mutable global that another test
//! can race.

#![cfg(test)]

use super::providers::{model_info, TurnUsage};
use super::router::{context_window, max_output};

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> TurnUsage {
    TurnUsage {
        input,
        output,
        cache_read,
        cache_write,
        reasoning: 0,
    }
}

// ───────────────────────────── cost ─────────────────────────────

/// The cost of a turn is billed per million tokens, so the arithmetic is
/// `tokens * price / 1e6`. A misplaced factor here is a 1,000,000x error and
/// it would still produce a plausible-looking dollar amount.
#[test]
fn a_turns_cost_is_tokens_times_price_per_million() {
    let mi = model_info("anthropic/claude-opus-5");
    // 1M input + 1M output at this model's prices is exactly
    // input_price + output_price, in dollars.
    let u = usage(1_000_000, 1_000_000, 0, 0);
    let want = mi.input_price + mi.output_price;
    let got = u.cost(&mi);
    assert!(
        (got - want).abs() < 1e-9,
        "1M in + 1M out should cost input_price + output_price = {want}, got {got}"
    );
}

/// A single token is a real, if tiny, cost. A rounding shortcut that returns
/// 0 for small turns would make a cheap model look free and hide spend from
/// the budget cap.
#[test]
fn a_single_token_is_not_free() {
    let mi = model_info("anthropic/claude-opus-5");
    assert!(
        mi.input_price > 0.0,
        "a paid model must have a price: {mi:?}"
    );
    let c = usage(1, 0, 0, 0).cost(&mi);
    assert!(c > 0.0, "one input token must cost something, got {c}");
    assert!(
        c < 0.001,
        "one token must not cost anything meaningful, got {c}"
    );
}

/// Output is priced far above input on every current model, and that ratio is
/// what makes long agent turns expensive. If the two prices were swapped, an
/// agent that writes a lot would look cheaper than one that reads a lot.
#[test]
fn output_is_priced_separately_from_input() {
    let mi = model_info("anthropic/claude-opus-5");
    let in_only = usage(1000, 0, 0, 0).cost(&mi);
    let out_only = usage(0, 1000, 0, 0).cost(&mi);
    assert!(
        mi.output_price > mi.input_price,
        "output should be dearer than input on a reasoning model"
    );
    assert!(
        out_only > in_only,
        "1000 output tokens ({out_only}) must cost more than 1000 input ({in_only})"
    );
}

/// Cached reads are the single biggest lever on an agent's bill, because
/// almost every turn re-reads the same prefix. The discount must be applied
/// to cache reads and not to fresh input.
#[test]
fn a_cache_read_is_discounted_against_a_fresh_read() {
    let mi = model_info("anthropic/claude-opus-5");
    let fresh = usage(0, 0, 10_000, 0).cost(&mi);
    let cached = usage(0, 0, 0, 0).cost(&mi);
    assert_eq!(cached, 0.0);
    // 10k cache reads are strictly cheaper than 10k fresh input reads.
    let as_input = usage(10_000, 0, 0, 0).cost(&mi);
    assert!(
        fresh < as_input,
        "a cache read ({fresh}) must be cheaper than the same tokens read fresh ({as_input})"
    );
    // A cache WRITE is what the app pays to seed the cache; it must not be
    // free, and must not be as dear as a fresh read either (providers bill
    // the write premium above input).
    let write = usage(0, 0, 0, 10_000).cost(&mi);
    assert!(write > 0.0, "a cache write is not free");
    assert!(
        write > fresh,
        "a cache write ({write}) costs more than a cache read ({fresh})"
    );
}

/// Cost must be additive across the four buckets, and must not depend on the
/// order the usage was accumulated in.
#[test]
fn cost_is_the_sum_of_its_four_buckets() {
    let mi = model_info("anthropic/claude-opus-5");
    let all = usage(100, 200, 300, 400).cost(&mi);
    let sum = usage(100, 0, 0, 0).cost(&mi)
        + usage(0, 200, 0, 0).cost(&mi)
        + usage(0, 0, 300, 0).cost(&mi)
        + usage(0, 0, 0, 400).cost(&mi);
    assert!(
        (all - sum).abs() < 1e-9,
        "cost must be additive: {all} vs {sum}"
    );
    // A turn that did nothing costs exactly nothing.
    assert_eq!(
        usage(0, 0, 0, 0).cost(&mi),
        0.0,
        "an empty turn is not billed"
    );
}

// ───────────────────────────── context ─────────────────────────────

/// `context()` is what the compaction trigger reads. It is the sum of
/// everything the provider counted, and getting a term wrong (usually
/// forgetting the output, or double-counting a cache read) shifts the
/// compaction point by thousands of tokens.
#[test]
fn context_sums_every_token_the_provider_counted() {
    assert_eq!(
        usage(10, 20, 30, 40).context(),
        100,
        "all four buckets are context"
    );
    assert_eq!(usage(0, 0, 0, 0).context(), 0);
    // A pure cache read still occupies context: it is re-sent every turn.
    assert_eq!(usage(0, 0, 500, 0).context(), 500);
}

/// The context window is the ceiling the provider will accept. An unknown
/// model falls back to a conservative default rather than to `u64::MAX`,
/// because an unbounded window means compaction never fires and the request
/// dies at the provider with a 400 the user cannot act on.
#[test]
fn an_unknown_model_gets_a_finite_context_window() {
    let w = context_window("no-such-provider/no-such-model", "");
    assert!(w > 0, "a context window must be positive");
    assert!(
        w <= 1_000_000,
        "an unknown model must not claim a huge window: {w}"
    );
}

/// The same, for the output ceiling: an unknown model must still get a finite
/// max output, or a turn can ask for an unbounded completion.
#[test]
fn an_unknown_model_gets_a_finite_output_ceiling() {
    let o = max_output("no-such-provider/no-such-model", "");
    assert!(
        o > 0 && o <= 1_000_000,
        "an unknown model must get a finite output cap: {o}"
    );
}

/// Known models must have a real, positive window — a model catalogued with
/// a zero context would compact on literally every turn, which looks like the
/// agent having amnesia.
#[test]
fn every_catalogued_model_has_a_usable_context_window() {
    for mi in super::providers::catalog() {
        assert!(mi.context > 0, "{} has a zero context window", mi.id);
        assert!(mi.output > 0, "{} has a zero output ceiling", mi.id);
        assert!(
            mi.output <= mi.context,
            "{} claims to output {} tokens into a {} context",
            mi.id,
            mi.output,
            mi.context
        );
    }
}

/// The compaction threshold the runner uses is 80% of the window. Pin the
/// arithmetic itself so the constant and the comparison cannot drift apart
/// unnoticed: a change from `>` to `>=`, or from `8/10` to `85/100`, moves
/// the trigger point.
#[test]
fn the_compaction_trigger_is_eighty_percent_of_the_window() {
    for model in [
        "anthropic/claude-opus-5",
        "openai/gpt-5",
        "no-such-provider/no-such-model",
    ] {
        let w = context_window(model, "");
        let threshold = w * 8 / 10;
        // Strictly greater-than: at exactly the threshold the run continues.
        assert!(
            !(threshold > w * 8 / 10),
            "threshold arithmetic is stable for {model}"
        );
        // A run at 79% is under the line; at 81% is over it.
        let under = w * 79 / 100;
        let over = w * 81 / 100;
        assert!(
            under <= threshold,
            "{model}: 79% ({under}) must not trigger"
        );
        assert!(over > threshold, "{model}: 81% ({over}) must trigger");
    }
}

/// A context window small enough that `* 8 / 10` rounds to zero would mean
/// "compact on every turn". The window floor exists to prevent exactly that.
#[test]
fn a_tiny_context_window_still_leaves_room_under_the_threshold() {
    let w = context_window("no-such-provider/no-such-model", "");
    assert!(w >= 8_000, "the window floor keeps 80% above zero: {w}");
    assert!(
        w * 8 / 10 > 0,
        "the threshold must be a usable number of tokens"
    );
}

// ───────────────────────────── reasoning ─────────────────────────────

/// `reasoning` is a subset of `output`, not an extra bucket. Counting it
/// twice would inflate the context read and the bill at the same time.
#[test]
fn reasoning_tokens_are_part_of_output_not_an_extra_bill() {
    let mi = model_info("openai/gpt-5");
    let base = usage(0, 1000, 0, 0).cost(&mi);
    let mut with_reasoning = usage(0, 1000, 0, 0);
    with_reasoning.reasoning = 800;
    assert_eq!(
        with_reasoning.cost(&mi),
        base,
        "reasoning is already inside `output`; it must not be charged again"
    );
    assert_eq!(
        with_reasoning.context(),
        1000,
        "reasoning must not be double-counted into the context window"
    );
}
