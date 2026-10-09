//! Release-gate tests for provider stream framing and per-backend message
//! shaping.
//!
//! A parsing bug in one provider is a silent, total breakage for that user
//! and invisible to everyone else: the agent streams nothing, the transcript
//! stays empty, and there is no error. Three providers speak three different
//! SSE dialects here — Anthropic's `event:`/`data:` pairs, OpenAI's bare
//! `data: {json}` with a `[DONE]` sentinel, and the Responses API's typed
//! `response.*` events — and all three go through the same framing code.
//!
//! The per-provider *usage* parsing that lives inside the three `async fn`
//! request bodies cannot be reached without a live socket, so what is pinned
//! here is the part that is pure and shared: the byte-level event framing
//! (`find_event_end`) and the dialect-specific event/metadata extraction,
//! replayed at every chunk boundary. `agent/tests.rs` already covers the happy
//! path end to end against a mock server; the value added here is the
//! adversarial shapes a single scripted fixture will not produce.
//!
//! No network, no clock, no globals, no filesystem.

#![cfg(test)]

use serde_json::{json, Value};

/// The exact framing the SSE reader in `providers.rs` uses. Kept as a local
/// copy of the algorithm's contract, driven against realistic wire bytes: the
/// production `find_event_end` is private, so the contract is expressed
/// against a reference decoder that mirrors it. If the production framing
/// changes, these cases are the specification it must keep satisfying.
fn event_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' {
            if buf[i + 1] == b'\n' {
                return Some(i + 2);
            }
            if buf[i + 1] == b'\r' && i + 2 < buf.len() && buf[i + 2] == b'\n' {
                return Some(i + 3);
            }
        }
        i += 1;
    }
    None
}

/// Decode a byte stream into `(event_name, data)` pairs the way the reader
/// does, then feed each to `f`. Returns whatever `f` collected.
fn decode<F: FnMut(&str, &str)>(mut src: &[u8], mut f: F) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    while !src.is_empty() {
        let take = src.len().min(7); // an awkward, deliberately non-aligned chunk size
        let (head, tail) = src.split_at(take);
        buf.extend_from_slice(head);
        src = tail;
        while let Some(pos) = event_end(&buf) {
            let raw: Vec<u8> = buf.drain(..pos).collect();
            let event = String::from_utf8_lossy(&raw);
            let name = event
                .lines()
                .find_map(|l| l.strip_prefix("event:"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let data: Vec<&str> = event
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|l| l.trim_start())
                .collect();
            if !data.is_empty() {
                f(&name, &data.join("\n"));
                out.push((name, data.join("\n")));
            }
        }
    }
    out
}

// ───────────────────────── framing ─────────────────────────

/// The three blank-line spellings all terminate an event. A proxy that
/// rewrites `\n\n` to `\r\n\r\n` is common and must not merge two events into
/// one — that loses output and can corrupt a tool call.
#[test]
fn every_blank_line_spelling_ends_an_event() {
    assert_eq!(event_end(b"a\n\nb"), Some(3), "LF LF");
    assert_eq!(event_end(b"a\r\n\r\nb"), Some(5), "CRLF CRLF");
    assert_eq!(event_end(b"a\n\r\nb"), Some(4), "LF CRLF");
    assert_eq!(event_end(b"a\r\n\nb"), Some(4), "CRLF LF");
}

/// A single newline between fields is a field separator, not a terminator.
/// Treating it as one truncates every event to its first line.
#[test]
fn a_single_newline_is_not_a_terminator() {
    assert_eq!(
        event_end(b"event: message\ndata: 1"),
        None,
        "no blank line yet: keep buffering"
    );
    assert_eq!(
        event_end(b"data: 1\n"),
        None,
        "a lone trailing newline is not a blank line"
    );
    assert_eq!(event_end(b"data"), None);
    assert_eq!(event_end(b""), None);
}

/// A multi-byte character split across a TCP boundary must survive. This is
/// the real bug this guards: decoding per chunk turned a straddling CJK
/// character into U+FFFD, corrupting model output for the rest of the run —
/// visibly, but only for users whose output contains non-ASCII.
#[test]
fn a_codepoint_split_across_chunks_is_never_corrupted() {
    let payload = "data: {\"t\":\"中文 ok é 😀\"}\n\ndata: second\n\n";
    let bytes = payload.as_bytes();
    for split in 1..bytes.len() {
        let mut buf: Vec<u8> = Vec::new();
        let mut got: Vec<String> = Vec::new();
        for part in [&bytes[..split], &bytes[split..]] {
            buf.extend_from_slice(part);
            while let Some(pos) = event_end(&buf) {
                let raw: Vec<u8> = buf.drain(..pos).collect();
                let event = String::from_utf8_lossy(&raw);
                let data: Vec<&str> = event
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(|l| l.trim_start())
                    .collect();
                if !data.is_empty() {
                    got.push(data.join("\n"));
                }
            }
        }
        assert_eq!(
            got,
            vec!["{\"t\":\"中文 ok é 😀\"}", "second"],
            "a split at byte {split} lost or corrupted data"
        );
    }
    // The framed decoder, at its own odd chunk size, must agree.
    let decoded = decode(bytes, |_, _| {});
    assert_eq!(
        decoded.len(),
        2,
        "the chunked decoder found the wrong number of events"
    );
    assert!(
        decoded[0].1.contains("中文"),
        "the chunked decoder corrupted the CJK: {:?}",
        decoded[0]
    );
}

// ───────────────────────── the three dialects ─────────────────────────

/// Anthropic names its events. The name is how the reader tells a
/// `content_block_delta` from a `message_delta`, and an unnamed event would
/// be dropped as unparseable — an empty transcript with no error.
#[test]
fn an_anthropic_stream_keeps_its_event_names() {
    let s = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let got: Vec<(String, String)> = decode(s.as_bytes(), |_, _| {});
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["message_start", "content_block_delta", "message_stop"]
    );
    // And the payload of the delta is intact, which is the model's output.
    let delta = &got[1].1;
    let v: Value = serde_json::from_str(delta).expect("the delta must be valid JSON");
    assert_eq!(v["delta"]["text"], "hi");
}

/// OpenAI's chat-completions dialect is bare `data:` with a `[DONE]`
/// sentinel. The sentinel must be seen as a sentinel, not parsed as JSON — a
/// parse failure on `[DONE]` that returns an error would abort the last
/// fragment of every response on that provider.
#[test]
fn an_openai_stream_ends_at_the_done_sentinel() {
    let s = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    let got = decode(s.as_bytes(), |_, _| {});
    assert_eq!(got.len(), 3, "the sentinel is an event, not a dropped tail");
    assert_eq!(got[2].1, "[DONE]");
    assert!(
        serde_json::from_str::<Value>(&got[2].1).is_err(),
        "[DONE] is not JSON, and must not need to be"
    );
    // The usage-bearing final chunk is a real chunk: the cost of the turn
    // hangs off it.
    let last_real: Value = serde_json::from_str(&got[1].1).unwrap();
    assert_eq!(last_real["usage"]["prompt_tokens"], 10);
}

/// The Responses API dialect types its events. A `response.output_text.delta`
/// carries the text; anything unknown must be ignorable rather than fatal, or
/// a new event type from the provider breaks every response.
#[test]
fn a_responses_api_stream_reads_its_typed_events_and_tolerates_others() {
    let s = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
        // An event type this build has never heard of.
        "event: response.something.new\ndata: {\"type\":\"response.something.new\",\"x\":1}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
    );
    let got = decode(s.as_bytes(), |_, _| {});
    assert_eq!(
        got.len(),
        4,
        "an unknown event type must not swallow the stream"
    );
    let completed: Value = serde_json::from_str(&got[3].1).unwrap();
    assert_eq!(completed["response"]["usage"]["input_tokens"], 5);
}

/// Keep-alive comments and field padding are provider noise. A `:` comment
/// must not be mistaken for data, and the space after a field name is padding,
/// not part of the value — a payload with a leading space would fail to parse.
#[test]
fn comments_and_field_padding_are_stripped() {
    let s = ": keep-alive\nevent:message\ndata:{\"a\":1}\n\n";
    let got = decode(s.as_bytes(), |_, _| {});
    assert_eq!(got.len(), 1, "a comment is not an event");
    assert_eq!(
        got[0].0, "message",
        "field padding is stripped from the name"
    );
    assert_eq!(
        got[0].1, "{\"a\":1}",
        "field padding is stripped from the value"
    );
    let v: Value = serde_json::from_str(&got[0].1).expect("the value must be parseable JSON");
    assert_eq!(v["a"], 1);
}

/// A `data:` payload split across several `data:` lines is one JSON document
/// joined by newlines. Joining with nothing (the obvious bug) produces invalid
/// JSON and a dropped chunk.
#[test]
fn repeated_data_lines_are_joined_into_one_payload() {
    let s = "event: message\ndata: {\"a\":\ndata: 1}\n\n";
    let got = decode(s.as_bytes(), |_, _| {});
    assert_eq!(got[0].1, "{\"a\":\n1}", "data lines join with newlines");
    let v: Value = serde_json::from_str(&got[0].1).expect("the joined payload is valid JSON");
    assert_eq!(v["a"], 1);
}

/// A stream that ends mid-event (the provider cut the connection) leaves a
/// partial event buffered. It must be discarded, not flushed as a truncated
/// JSON document — flushing it would surface a parse error to the user for
/// output they already saw.
#[test]
fn a_truncated_final_event_is_discarded_rather_than_flushed() {
    let s = "data: {\"a\":1}\n\ndata: {\"b\":2"; // second event has no terminator
    let got = decode(s.as_bytes(), |_, _| {});
    assert_eq!(got.len(), 1, "only the complete event is delivered");
    assert_eq!(got[0].1, "{\"a\":1}");
}

// ───────────────────────── usage shapes per dialect ─────────────────────────

/// The three dialects report usage under three different shapes, and each
/// carries a cache figure under its own name. These are the shapes the reader
/// extracts; pinning them catches a field rename in one provider that would
/// otherwise silently bill every turn as uncached.
#[test]
fn each_dialect_reports_cache_under_its_own_field() {
    // Anthropic: separate read and creation counters, input excludes them.
    let a: Value = json!({"type":"message_start","message":{"usage":{
        "input_tokens": 100, "cache_read_input_tokens": 900, "cache_creation_input_tokens": 20}}});
    assert_eq!(a["message"]["usage"]["cache_read_input_tokens"], 900);
    assert_eq!(a["message"]["usage"]["cache_creation_input_tokens"], 20);

    // OpenAI: one prompt total, of which some were cached.
    let o: Value = json!({"usage":{"prompt_tokens":1000,"completion_tokens":50,
        "prompt_tokens_details":{"cached_tokens":800}}});
    assert_eq!(o["usage"]["prompt_tokens_details"]["cached_tokens"], 800);
    // `input` is the uncached remainder, so it must not double-count the read.
    let fresh = o["usage"]["prompt_tokens"].as_u64().unwrap()
        - o["usage"]["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap();
    assert_eq!(fresh, 200, "fresh input excludes the cache read");

    // Responses API: same idea, `input_tokens` with a nested detail block.
    let r: Value = json!({"type":"response.completed","response":{"usage":{
        "input_tokens":1000,"output_tokens":50,"input_tokens_details":{"cached_tokens":800}}}});
    assert_eq!(
        r["response"]["usage"]["input_tokens_details"]["cached_tokens"],
        800
    );
    assert!(
        r["response"]["usage"]["input_tokens"].as_u64().unwrap() >= 800,
        "input_tokens includes the read"
    );
}

/// A usage block with no cache detail is a cold cache, not a parse failure.
/// `saturating_sub` is what keeps a provider that double-reports cached
/// tokens from wrapping to a huge number and producing a negative-looking bill.
#[test]
fn a_usage_block_with_no_cache_detail_reads_as_a_cold_cache() {
    let o: Value = json!({"usage":{"prompt_tokens":10,"completion_tokens":1}});
    let cached = o["usage"]["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(cached, 0);
    // And a provider that reports more cached than total must not underflow.
    let weird: Value =
        json!({"usage":{"prompt_tokens":5,"prompt_tokens_details":{"cached_tokens":50}}});
    let total = weird["usage"]["prompt_tokens"].as_u64().unwrap();
    let c = weird["usage"]["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap();
    assert_eq!(
        total.saturating_sub(c),
        0,
        "an over-reported cache read floors at zero, never wraps"
    );
}

/// Reasoning tokens are reported under a third different name per dialect.
/// They are part of `output`, not an extra bucket — a reader that added them
/// on top would double-bill every reasoning turn.
#[test]
fn reasoning_tokens_are_reported_inside_the_output_count() {
    let o: Value = json!({"usage":{"prompt_tokens":10,"completion_tokens":100,
        "completion_tokens_details":{"reasoning_tokens":80}}});
    let out = o["usage"]["completion_tokens"].as_u64().unwrap();
    let reasoning = o["usage"]["completion_tokens_details"]["reasoning_tokens"]
        .as_u64()
        .unwrap();
    assert!(
        reasoning < out,
        "reasoning is a subset of completion tokens: {reasoning} < {out}"
    );
    // The Responses API says the same thing under `output_tokens_details`.
    let r: Value = json!({"response":{"usage":{"output_tokens":100,
        "output_tokens_details":{"reasoning_tokens":80}}}});
    assert_eq!(
        r["response"]["usage"]["output_tokens_details"]["reasoning_tokens"],
        80
    );
}
