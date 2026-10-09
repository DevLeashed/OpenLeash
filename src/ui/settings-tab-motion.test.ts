/// <reference types="node" />
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync("src/App.css", "utf8");

describe("Settings tab motion is temporarily disabled", () => {
  it("overrides the retained entrance animation for selected and deselected tabs", () => {
    const rules = [...css.matchAll(/\.stab\s*\{([^}]*)\}/g)]
      .map((match) => /animation:\s*([^;]+)/.exec(match[1]!)?.[1]?.trim())
      .filter((animation) => animation !== undefined);

    expect(rules).toContain("olRowIn .35s var(--out) both");
    expect(rules.at(-1)).toBe("none");
    // A selected-only override makes olRowIn restart when selection moves away.
    expect(css).not.toMatch(/\.stab\.on\s*\{[^}]*animation:/);
  });
});
