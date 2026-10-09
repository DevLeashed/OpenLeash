import { describe, expect, it } from "vitest";
import { guess, remoteReasoning } from "./ModelManager";

describe("smart model configuration", () => {
  it("uses explicit reasoning levels and parameter mapping from model metadata", () => {
    expect(remoteReasoning({ supported_reasoning_levels: [{ effort: "low" }, { effort: "high" }], reasoning_param: "reasoning.effort" })).toEqual({ levels: ["low", "high"], param: "reasoning.effort" });
  });

  it("infers a known reasoning parameter from supported parameters", () => {
    expect(remoteReasoning({ supported_parameters: ["reasoning_effort"] })).toEqual({ levels: [], param: "reasoning_effort" });
    expect(remoteReasoning({ supported_parameters: ["reasoning"] })).toEqual({ levels: [], param: "reasoning.effort" });
  });

  it("keeps model-id guesses when provider metadata omits reasoning", () => {
    expect(guess("claude-sonnet-5", "anthropic")).toMatchObject({ reasoning_levels: ["low", "medium", "high", "xhigh", "max"], reasoning_param: "anthropic_effort" });
    expect(guess("deepseek-r1", "openai")).toMatchObject({ reasoning_levels: ["off", "on"], reasoning_param: "thinking" });
  });
});
