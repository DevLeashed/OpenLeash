// Tests for the Subagents editor's group/step helpers. Pure functions, so this
// runs without mounting the dialog. The properties under test are the ones the
// Rust side also enforces — the point is that the dialog catches a bad pattern
// *before* the settings write, with the same verdict.
import { describe, expect, it } from "vitest";
import type { ToolGroup } from "../api";
import { GROUP_NAMES, groupsOf, hasEditRestriction, restrictionLabel, stepsLabel, toToolGroups, validateGroups } from "./agent-groups";

describe("groupsOf", () => {
  it("reads both wire shapes", () => {
    const wire: ToolGroup[] = ["read", { name: "edit", file_regex: "\\.md$", description: "docs" }];
    expect(groupsOf({ groups: wire })).toEqual([
      { name: "read", fileRegex: "", description: "" },
      { name: "edit", fileRegex: "\\.md$", description: "docs" },
    ]);
  });
  it("treats a missing groups field as none", () => {
    expect(groupsOf({})).toEqual([]);
  });
});

describe("toToolGroups", () => {
  it("emits a bare string when the group is unrestricted", () => {
    expect(toToolGroups([{ name: "read", fileRegex: "", description: "" }])).toEqual(["read"]);
  });
  it("emits the tuple only when a pattern or description is set", () => {
    expect(toToolGroups([{ name: "edit", fileRegex: "\\.md$", description: "" }])).toEqual([
      { name: "edit", file_regex: "\\.md$", description: "" },
    ]);
  });
  it("drops empty rows so a half-typed row never saves", () => {
    expect(toToolGroups([{ name: "", fileRegex: "", description: "" }])).toEqual([]);
  });
  it("round-trips through the wire shape", () => {
    const rows = [{ name: "edit", fileRegex: "\\.(md|mdx)$", description: "docs only" }];
    expect(groupsOf({ groups: toToolGroups(rows) })).toEqual(rows);
  });
});

describe("validateGroups", () => {
  it("accepts an unrestricted group and a restricted edit group", () => {
    expect(validateGroups([{ name: "read", fileRegex: "", description: "" }])).toBeNull();
    expect(validateGroups([{ name: "edit", fileRegex: "\\.(md|mdx)$", description: "docs" }])).toBeNull();
  });
  it("rejects an unmatched paren, the same pattern Rust refuses", () => {
    // `\(` would be valid — a literal paren — which is why this test uses `(`.
    const err = validateGroups([{ name: "edit", fileRegex: "(unclosed", description: "" }]);
    expect(err).toMatch(/fileRegex/);
  });
  it("rejects an unknown group name", () => {
    expect(validateGroups([{ name: "write", fileRegex: "", description: "" }])).toMatch(/Unknown tool group/);
  });
  it("rejects a fileRegex on a group that cannot enforce it", () => {
    expect(validateGroups([{ name: "command", fileRegex: "\\.sh$", description: "" }])).toMatch(/edit/);
  });
  it("rejects two edit groups with patterns", () => {
    const err = validateGroups([
      { name: "edit", fileRegex: "\\.md$", description: "" },
      { name: "edit", fileRegex: "\\.ts$", description: "" },
    ]);
    expect(err).toMatch(/only be listed once/);
  });
  it("accepts every group name Roo documents", () => {
    for (const n of GROUP_NAMES) expect(validateGroups([{ name: n, fileRegex: n === "edit" ? "\\.md$" : "", description: "" }])).toBeNull();
  });
});

describe("hasEditRestriction / restrictionLabel", () => {
  it("detects a restricted edit group", () => {
    expect(hasEditRestriction([{ name: "read", fileRegex: "", description: "" }])).toBe(false);
    expect(hasEditRestriction([{ name: "edit", fileRegex: "\\.md$", description: "" }])).toBe(true);
  });
  it("prefers the description in the label when there is one", () => {
    expect(restrictionLabel([{ name: "edit", fileRegex: "\\.md$", description: "docs only" }])).toBe("edits docs only");
    expect(restrictionLabel([{ name: "edit", fileRegex: "\\.md$", description: "" }])).toBe("edits matching \\.md$");
  });
});

describe("stepsLabel", () => {
  it("says no limit at zero or absent", () => {
    expect(stepsLabel(0)).toBe("no step limit");
    expect(stepsLabel(undefined)).toBe("no step limit");
  });
  it("counts steps and gets the singular right", () => {
    expect(stepsLabel(1)).toBe("stops after 1 step");
    expect(stepsLabel(60)).toBe("stops after 60 steps");
  });
});
