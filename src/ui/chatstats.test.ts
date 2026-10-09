import { describe, expect, it } from "vitest";
import { chatFacts } from "./ChatStats";
import type { ChatStat, Counter } from "./Stats";

const counter = (o: Partial<Counter>): Counter => ({
  requests: 0, input: 0, output: 0, cache_read: 0, cache_write: 0, errors: 0,
  total_ms: 0, ttft_ms: 0, ttft_n: 0, cost: 0, last_used: "", ...o,
});

const stat = (total: Partial<Counter>, rest: Partial<ChatStat> = {}): ChatStat => ({
  total: counter(total),
  models: {},
  tools: {},
  events: {},
  started: "",
  ...rest,
});

describe("per-chat stats", () => {
  it("separates the wall-clock span from the time the agent spent", () => {
    // The whole reason the panel shows two durations: a chat left open over lunch
    // has a long span and a short sum of responses. Collapsing them into one
    // "time taken" number reads as a slow model when nothing was wrong.
    const f = chatFacts(stat(
      { requests: 4, total_ms: 8_000, last_used: "2026-01-01T14:00:00Z" },
      { started: "2026-01-01T10:00:00Z" },
    ));
    expect(f.spanMs).toBe(4 * 3600_000);
    expect(f.busyMs).toBe(8_000);
    expect(f.avgMs).toBe(2_000);
  });

  it("averages only over requests that actually streamed", () => {
    const f = chatFacts(stat({
      requests: 10, ttft_ms: 2_000, ttft_n: 4,
    }));
    // Dividing by `requests` would report 200ms of "time to first token" for a
    // chat where only four responses ever reported one.
    expect(f.ttftMs).toBe(500);
    expect(f.ttftN).toBe(4);
  });

  it("reports no first-token time when nothing streamed", () => {
    const f = chatFacts(stat({ requests: 3 }));
    expect(f.ttftMs).toBe(0);
    expect(f.ttftN).toBe(0);
  });

  it("counts errors against attempts, not requests", () => {
    // A failed attempt is not a request, but it is something the chat spent
    // time on. Dividing by `requests` alone would exceed 100% and read as
    // "more failures than attempts happened".
    const f = chatFacts(stat({ requests: 3, errors: 1 }));
    expect(f.errRate).toBe(0.25);
    const none = chatFacts(stat({ requests: 0, errors: 0 }));
    expect(none.errRate).toBe(0);
  });

  it("never reports a negative span when the clock disagrees with itself", () => {
    // `last_used` is written per request and `started` per chat; a chat whose
    // stamps come back out of order should read as zero-length, not as a
    // negative duration rendered as e.g. "-4.0s".
    const f = chatFacts(stat(
      { requests: 1, last_used: "2026-01-01T10:00:00Z" },
      { started: "2026-01-01T12:00:00Z" },
    ));
    expect(f.spanMs).toBe(0);
  });

  it("leaves the span at zero when the chat has only ever errored", () => {
    // An error-created row has no successful request, and may have no start at
    // all; the panel must render, not divide by a missing date.
    const f = chatFacts(stat({ requests: 0, errors: 2 }));
    expect(f.spanMs).toBe(0);
    expect(f.avgMs).toBe(0);
    expect(f.startedAt).toBeNull();
  });

  it("counts the models a chat used, which is what a fallback looks like", () => {
    const f = chatFacts(stat({ requests: 3 }, {
      models: { "codex/gpt-6-sol": counter({}), "anthropic/claude-opus-5": counter({}) },
    }));
    expect(f.models).toBe(2);
    expect(chatFacts(stat({ requests: 3 }, { models: { "codex/gpt-6-sol": counter({}) } })).models).toBe(1);
  });

  it("totals every token class, cache writes included", () => {
    const f = chatFacts(stat({ input: 10, output: 5, cache_read: 100, cache_write: 7 }));
    expect(f.tokens).toBe(122);
  });
});