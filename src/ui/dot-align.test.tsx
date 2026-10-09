// @vitest-environment jsdom
/// <reference types="node" />
// The status dot that rides the "N live" header chip was drawn for the agent
// tree, where a 5px top margin lines it up with the first line of a row's text.
// The chip is not a tree row: it is a centre-aligned button, and the margin
// dropped the dot below the text — the dot hung under the number, which is what
// the field screenshot showed.
//
// jsdom has no layout, so this cannot measure pixels. What it can pin is the
// rule the fix encodes, and that no call site re-adds the offset by hand — two
// of them used to, and the tree rows still need it.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync("src/App.css", "utf8");
const sources = ["src/ui/Session.tsx", "src/ui/Overlays.tsx"]
  .map((f) => readFileSync(f, "utf8"));

/** The `margin-top` a rule declares, or null when it declares none. */
const rule = (sel: string) => {
  const i = css.indexOf(sel + " {");
  if (i < 0) return null;
  return /margin-top:\s*([^;]+)/.exec(css.slice(i, css.indexOf("}", i)))?.[1]?.trim() ?? null;
};

describe("the swarm dot is not carrying a tree row's offset", () => {
  it("leaves the base dot centred, and gives the offset to the tree rows alone", () => {
    // The bug: the base `.tdot` rule carried `margin-top: 5px`, so every dot
    // that is not in a tree row was pushed below its text. The fix moves the
    // offset onto `.trow .tdot`, which is the only place it was ever wanted.
    expect(rule(".tdot"), "the base dot must carry no vertical offset").toBeNull();
    expect(rule(".trow .tdot"), "the tree rows keep the offset they were drawn for").toBe("5px");
  });

  it("has no call site re-applying the offset inline to work around it", () => {
    // Two call sites used to hand-patch `marginTop: 0` against the old rule.
    // Left in, they are dead weight; re-added, they hide the real rule.
    for (const src of sources) {
      const inline = /className=\{?["'][^"']*\btdot\b[^"']*["'][^}]*?marginTop/.exec(src);
      expect(inline?.[0], "no inline marginTop on a tdot").toBeUndefined();
    }
  });
});
