import { describe, expect, it } from "vitest";
import { applyEditsToSource, colorToHex, cssValue, editNote, liveEdits, pxNumber, safeSelector, type EditSet } from "./artifictEdits";

const text = (selector: string, originalText: string, next: string, label = "heading"): EditSet => ({ [selector]: { selector, label, originalText, text: next, styles: {} } });

describe("applyEditsToSource", () => {
  it("replaces a heading's text where it occurs once between tags, escaping the new text", () => {
    const source = "<main>\n  <h1>\n    Pricing\n  </h1>\n  <p>Plans for teams</p>\n</main>";
    const result = applyEditsToSource(source, text("main > h1", "\n    Pricing\n  ", "Plans & <prices>"));
    expect(result.failed).toEqual([]);
    expect(result.text).toBe(1);
    expect(result.content).toContain("<h1>\n    Plans &amp; &lt;prices&gt;\n  </h1>");
    expect(result.content).toContain("<p>Plans for teams</p>");
  });

  it("matches rendered text against its escaped source form", () => {
    const result = applyEditsToSource("<h2>Fish &amp; chips</h2>", text("h2", "Fish & chips", "Fish and chips"));
    expect(result.failed).toEqual([]);
    expect(result.content).toBe("<h2>Fish and chips</h2>");
  });

  it("refuses an ambiguous text change instead of guessing which copy was meant", () => {
    const source = "<title>Pricing</title><h1>Pricing</h1>";
    const result = applyEditsToSource(source, text("h1", "Pricing", "Plans"));
    expect(result.text).toBe(0);
    expect(result.failed).toEqual([{ label: "heading", reason: expect.stringContaining("appears 2 times") }]);
    expect(result.content).toBe(source);
  });

  it("refuses text a script renders, which is not in the source at all", () => {
    const result = applyEditsToSource("<div id=app></div><script>app.textContent='Hi'</script>", text("#app", "Hi", "Hello"));
    expect(result.failed[0]?.reason).toContain("not written literally");
  });

  it("does not touch attribute values that contain the same text", () => {
    const result = applyEditsToSource('<a title="Buy">Buy</a>', text("a", "Buy", "Purchase", "link"));
    expect(result.content).toBe('<a title="Buy">Purchase</a>');
  });

  it("writes style edits into one marked block before </body>, and merges a second round into it", () => {
    const doc = "<!doctype html><html><head><style>h1{color:red}</style></head><body><h1>Hi</h1></body></html>";
    const first = applyEditsToSource(doc, { "body > h1": { selector: "body > h1", label: "heading", originalText: "Hi", styles: { color: "#112233", "font-size": "40" } } });
    expect(first.failed).toEqual([]);
    expect(first.styles).toBe(2);
    expect(first.content).toMatch(/<h1>Hi<\/h1><style data-openleash-edits>[\s\S]*body > h1 \{ color: #112233 !important; font-size: 40px !important; \}[\s\S]*<\/style>\n<\/body><\/html>$/);
    const second = applyEditsToSource(first.content, { "body > h1": { selector: "body > h1", label: "heading", originalText: "Hi", styles: { "font-size": "48px" } }, "#cta": { selector: "#cta", label: "button", originalText: null, styles: { "border-radius": "24" } } });
    expect(second.failed).toEqual([]);
    expect(second.content.match(/data-openleash-edits/g)).toHaveLength(1);
    expect(second.content).toContain("body > h1 { color: #112233 !important; font-size: 48px !important; }");
    expect(second.content).toContain("#cta { border-radius: 24px !important; }");
  });

  it("appends the block to a fragment", () => {
    const result = applyEditsToSource("<section>Hi</section>  ", { section: { selector: "section", label: "section", originalText: "Hi", styles: { padding: "24" } } });
    expect(result.content).toBe("<section>Hi</section>\n<style data-openleash-edits>\n/* Direct edits made in the OpenLeash studio. Keep them, or fold them into the design's own CSS. */\nsection { padding: 24px !important; }\n</style>\n");
  });

  it("rejects values and selectors that could break out of the style element", () => {
    const evil = applyEditsToSource("<p>x</p>", { p: { selector: "p", label: "text", originalText: "x", styles: { color: "red;}</style><script>alert(1)</script>" } } });
    expect(evil.failed[0]?.reason).toContain("invalid value");
    expect(evil.content).not.toContain("<script>");
    const badSelector = applyEditsToSource("<p>x</p>", { "p{}": { selector: "p</style>", label: "text", originalText: "x", styles: { color: "#fff" } } });
    expect(badSelector.failed[0]?.reason).toContain("selector");
    expect(badSelector.content).not.toContain("p</style>");
  });
});

describe("value helpers", () => {
  it("validates the shapes the panel produces and nothing else", () => {
    expect(cssValue("color", "#ABCDEF")).toBe("#abcdef");
    expect(cssValue("color", "rgb(1, 2, 3)")).toBe("rgb(1, 2, 3)");
    expect(cssValue("color", "red; display:none")).toBeNull();
    expect(cssValue("font-size", "18")).toBe("18px");
    expect(cssValue("font-size", "9999")).toBeNull();
    expect(cssValue("font-weight", "700")).toBe("700");
    expect(cssValue("font-weight", "bold")).toBeNull();
    expect(colorToHex("rgb(17, 24, 39)")).toBe("#111827");
    expect(colorToHex("rgba(0, 0, 0, 0)")).toBeNull();
    expect(colorToHex("#abc")).toBe("#aabbcc");
    expect(pxNumber("8px 16px")).toBe(8);
    expect(safeSelector("main > section:nth-of-type(2) > h2")).toBe(true);
    expect(safeSelector("a{b}")).toBe(false);
  });

  it("sends the frame only elements that changed, and notes what changed", () => {
    const edits: EditSet = {
      h1: { selector: "h1", label: "heading “Hi”", originalText: "Hi", text: "Hello", styles: {} },
      p: { selector: "p", label: "text", originalText: "x", text: "x", styles: {} },
    };
    expect(liveEdits(edits)).toEqual([{ selector: "h1", styles: {}, text: "Hello" }]);
    const result = applyEditsToSource("<h1>Hi</h1><p>x</p>", edits);
    expect(editNote(edits, result)).toBe("Edited on the canvas: 1 text change (heading “Hi”)");
    const long: EditSet = Object.fromEntries(Array.from({ length: 40 }, (_, i) => [`s${i}`, { selector: `s${i}`, label: `a fairly long element label number ${i}`, originalText: null, styles: { padding: "4" } }]));
    expect(new TextEncoder().encode(editNote(long, applyEditsToSource("<p></p>", long))).byteLength).toBeLessThanOrEqual(300);
  });
});
