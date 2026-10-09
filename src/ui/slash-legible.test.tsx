// @vitest-environment jsdom
/// <reference types="node" />
// The slash menu floats directly over the transcript, so the one thing it may
// never do is let the transcript read back through it. In the field it did: a
// busy chat behind the composer put every message, tool diff and sidebar row
// straight through the menu, and "/goal" was competing with four lines of grey
// text for the same pixels.
//
// The cause is not the alpha. `--panel-glass` is 0.9 opaque, which is plenty on
// its own — a 0.1 bleed-through of #f4f4f5 body text over #222225 lands at
// #373740, still clearly legible, and the blur that was supposed to smear the
// rest away was doing nothing at all:
//
//   `.slash` is a child of `.composer`, and in a session `.composer.float`
//   carries `backdrop-filter: blur(24px)`. Per the Filter Effects spec an
//   element with a backdrop-filter becomes a *backdrop root*, and a nested
//   backdrop-filter samples only what is painted inside its own root — which,
//   for a menu that opens *above* the composer, is nothing. So the blur ran
//   against an empty backdrop and the raw transcript came through instead.
//
// The fix is the one `.queue` and `.asklater` already use for surfaces floating
// over the chat: an opaque `--raise`, no filter. Glass stays where the blur can
// reach the page — `.pop`, `.palette`, `.ddpop` are portalled to the body,
// outside every backdrop root.
//
// jsdom has no layout and no cascade, so this cannot measure pixels. What it can
// pin is the rule the fix encodes and the trap that made it necessary.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync("src/App.css", "utf8");
const composer = readFileSync("src/ui/Composer.tsx", "utf8");

/** The body of a rule, or "" when no such rule exists. */
const decl = (sel: string) => {
  const i = css.indexOf(sel + " {");
  return i < 0 ? "" : css.slice(i + sel.length + 2, css.indexOf("}", i));
};

describe("the slash menu is a surface, not a window", () => {
  it("paints an opaque fill rather than a translucent one", () => {
    const d = decl(".slash");
    expect(d, "the .slash rule must exist").not.toBe("");
    // A 0.1 bleed-through is enough to make bright text legible over a dark
    // fill, so "nearly opaque" is not a passing bar here — only opaque is.
    expect(d, "the slash menu must have a fill of its own").toMatch(/background:\s*var\(--(raise|panel|field)\)/);
    expect(d, "a glass fill here is the bug — the transcript reads through it").not.toContain("panel-glass");
  });

  it("does not lean on a blur that cannot reach anything", () => {
    // Kept as its own assertion so the blur coming back is caught: it looks
    // like the fix, and it is the thing that was never working.
    expect(decl(".slash"), "the backdrop under this element is empty — blurring it does nothing").not.toContain("backdrop-filter");
  });

  it("still has the backdrop root that makes that true", () => {
    // The reason the filter could not work. If someone ever drops the blur off
    // `.composer.float`, the glass comes back with a working blur and this whole
    // file is describing a bug that no longer exists.
    expect(decl(".composer.float"), "the composer still blurs the chat behind it").toContain("backdrop-filter");
  });

  it("names the command in a theme colour, not a hardcoded light one", () => {
    // The other half of the same complaint: the command column was pinned to
    // #f4f4f5, which is `--fg` in dark and an invisible white on white in light.
    const cmd = /<span className="mono"[^>]*color:\s*"([^"]+)"/.exec(composer);
    expect(cmd?.[1], "the command name uses the theme foreground").toBe("var(--fg)");
  });
});