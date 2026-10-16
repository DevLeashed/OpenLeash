import { parseArtifictState, MAX_ARTIFICT_STATE_BYTES } from "./artifictApi";
import { describe, expect, it } from "vitest";

describe("bounded manual prototype state", () => {
  it("accepts only bounded object JSON snapshots", () => {
    expect(parseArtifictState("[1,2]").error).toContain("JSON object");
    expect(parseArtifictState("x".repeat(MAX_ARTIFICT_STATE_BYTES + 1)).error).toContain("16 KB");
    expect(parseArtifictState('{"choice":"compact"}')).toEqual({ value: { choice: "compact" }, error: "" });
  });
});
