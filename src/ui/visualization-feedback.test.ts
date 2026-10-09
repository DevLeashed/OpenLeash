import { parseArtifactState, MAX_ARTIFACT_STATE_BYTES } from "./artifactApi";
import { describe, expect, it } from "vitest";

describe("bounded manual prototype state", () => {
  it("accepts only bounded object JSON snapshots", () => {
    expect(parseArtifactState("[1,2]").error).toContain("JSON object");
    expect(parseArtifactState("x".repeat(MAX_ARTIFACT_STATE_BYTES + 1)).error).toContain("16 KB");
    expect(parseArtifactState('{"choice":"compact"}')).toEqual({ value: { choice: "compact" }, error: "" });
  });
});
