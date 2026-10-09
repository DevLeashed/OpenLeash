import { afterEach, describe, expect, it, vi } from "vitest";
import { listenForVisualizationDraft, MAX_VIZ_FEEDBACK_PER_PREVIEW, parseVisualizationFeedbackDraft, type VisualizationFeedbackDraft } from "./visualizationFeedback";

function frame() { return { contentWindow: {} } as HTMLIFrameElement; }
function message(source: MessageEventSource, origin: string, data: unknown) {
  const event = new Event("message") as MessageEvent;
  Object.defineProperties(event, { source: { value: source }, origin: { value: origin }, data: { value: data } });
  return event;
}
afterEach(() => vi.restoreAllMocks());

describe("model-generated visualization feedback", () => {
  it("accepts only bounded versioned state/target payloads", () => {
    expect(parseVisualizationFeedbackDraft({ type: "openleash:feedback-draft", version: 1, target: { selector: "button", label: "Save" }, state: { choice: "compact" } })).toEqual({ target: { selector: "button", label: "Save" }, state: { choice: "compact" } });
    expect(parseVisualizationFeedbackDraft({ type: "openleash:feedback-draft", version: 1, state: { choice: "x".repeat(9 * 1024) } })).toBeNull();
    expect(parseVisualizationFeedbackDraft({ type: "openleash:feedback-draft", version: 1, run_command: "rm -rf" })).toBeNull();
  }),
  it("rejects state with excessive depth, nesting work, or unsupported target keys", () => {
    let state: Record<string, unknown> = {};
    for (let i = 0; i < 10; i++) state = { child: state };
    expect(parseVisualizationFeedbackDraft({ type: "openleash:feedback-draft", version: 1, state })).toBeNull();
    const broad = Object.fromEntries(Array.from({ length: 501 }, (_, i) => [`k${i}`, { value: i }]));
    expect(parseVisualizationFeedbackDraft({ type: "openleash:feedback-draft", version: 1, state: broad })).toBeNull();
    expect(parseVisualizationFeedbackDraft({ type: "openleash:feedback-draft", version: 1, target: { text: "Sensitive DOM text" } })).toBeNull();
  });
  it("filters to the exact opaque iframe and rate-limits/caps draft-only callbacks", () => {
    const preview = frame(), other = frame(), onDraft = vi.fn<(draft: VisualizationFeedbackDraft) => void>();
    let time = 1_000;
    const target = new EventTarget();
    const stop = listenForVisualizationDraft(preview, onDraft, target as Window, () => time);
    const valid = { type: "openleash:feedback-draft", version: 1, state: { volume: 8 } };
    target.dispatchEvent(message(other.contentWindow!, "null", valid));
    target.dispatchEvent(message(preview.contentWindow!, "https://evil.example", valid));
    target.dispatchEvent(message(preview.contentWindow!, "null", valid));
    expect(onDraft).toHaveBeenCalledTimes(1);
    time += 149; target.dispatchEvent(message(preview.contentWindow!, "null", valid));
    expect(onDraft).toHaveBeenCalledTimes(1);
    for (let i = 1; i < MAX_VIZ_FEEDBACK_PER_PREVIEW + 4; i++) {
      time += 150; target.dispatchEvent(message(preview.contentWindow!, "null", { ...valid, state: { volume: i } }));
    }
    expect(onDraft).toHaveBeenCalledTimes(MAX_VIZ_FEEDBACK_PER_PREVIEW);
    stop(); time += 150; target.dispatchEvent(message(preview.contentWindow!, "null", valid));
    expect(onDraft).toHaveBeenCalledTimes(MAX_VIZ_FEEDBACK_PER_PREVIEW);
  });
});
