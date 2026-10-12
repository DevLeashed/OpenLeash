import { describe, expect, it } from "vitest";
import { guess, remoteReasoning } from "./ModelManager";

describe("smart model configuration", () => {
  it("reads OpenRouter reasoning effort metadata and orders levels low to high", () => {
    expect(remoteReasoning({ reasoning: { supported_efforts: ["max", "xhigh", "high", "medium", "low", "minimal", "none"] }, supported_parameters: ["reasoning"] })).toEqual({ levels: ["none", "minimal", "low", "medium", "high", "xhigh", "max"], param: "reasoning.effort" });
  });

  it("does not offer disabling reasoning when OpenRouter says it is mandatory", () => {
    expect(remoteReasoning({ reasoning: { mandatory: true, supported_efforts: ["high", "medium", "low", "none"] } })).toEqual({ levels: ["low", "medium", "high"], param: "reasoning.effort" });
  });

  it("detects OpenRouter's token-budget-only reasoning models", () => {
    expect(remoteReasoning({ reasoning: { supports_max_tokens: true }, supported_parameters: ["reasoning"] })).toEqual({ levels: ["1024", "4000", "8000", "16000", "32000"], param: "reasoning.max_tokens" });
  });

  it("uses OpenRouter's full effort ladder when its endpoint reports null", () => {
    expect(remoteReasoning({ reasoning: { supported_efforts: null, mandatory: false }, supported_parameters: ["reasoning"] }, "openrouter")).toEqual({ levels: ["none", "minimal", "low", "medium", "high", "xhigh", "max"], param: "reasoning.effort" });
  });

  it("uses OpenRouter's explicit reasoning_effort metadata when provided", () => {
    expect(remoteReasoning({ provider: "openrouter", reasoning_effort: ["low", "medium", "high"], supported_parameters: ["reasoning_effort"] })).toEqual({ levels: ["low", "medium", "high"], param: "reasoning_effort" });
  });

  it("keeps explicit provider model specs ahead of guesses", () => {
    expect(remoteReasoning({ supported_reasoning_levels: [{ effort: "low" }, { effort: "high" }], reasoning_param: "reasoning.effort" })).toEqual({ levels: ["low", "high"], param: "reasoning.effort" });
  });

  it("infers a known reasoning parameter from supported parameters", () => {
    expect(remoteReasoning({ supported_parameters: ["reasoning_effort"] })).toEqual({ levels: [], param: "reasoning_effort" });
    expect(remoteReasoning({ supported_parameters: ["reasoning"] })).toEqual({ levels: [], param: "reasoning.effort" });
  });

  it("uses adaptive effort for Haiku 5.x and budget thinking for Haiku 4.x", () => {
    expect(guess("claude-haiku-5-5", "anthropic")).toMatchObject({ context: 1_000_000, output: 128_000, reasoning_levels: ["low", "medium", "high", "xhigh", "max"], reasoning_param: "anthropic_effort" });
    expect(guess("claude-haiku-5-5-20261001", "anthropic")).toMatchObject({ reasoning_param: "anthropic_effort" });
    expect(guess("claude-haiku-4-5-20251001", "anthropic")).toMatchObject({ context: 200_000, output: 64_000, reasoning_levels: ["1024", "4000", "8000", "16000", "32000"], reasoning_param: "thinking_budget" });
  });

  it("keeps model-id guesses when provider metadata omits reasoning", () => {
    expect(guess("claude-sonnet-5", "anthropic")).toMatchObject({ reasoning_levels: ["low", "medium", "high", "xhigh", "max"], reasoning_param: "anthropic_effort" });
    expect(guess("deepseek-r1", "openai")).toMatchObject({ reasoning_levels: ["off", "on"], reasoning_param: "thinking" });
  });
});
