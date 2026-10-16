// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { hostFontMono, hostFontSans, svgImageUrl } from "./hostTheme";

afterEach(() => {
  document.documentElement.style.cssText = "";
  document.body.style.cssText = "";
});

const decode = (url: string) => decodeURIComponent(url.split(",")[1]!);

describe("the face handed to documents the host renders", () => {
  it("prefers the app's declared token", () => {
    document.documentElement.style.setProperty("--font-sans", '"Geist", system-ui, sans-serif');
    expect(hostFontSans()).toBe('"Geist", system-ui, sans-serif');
  });

  it("refuses a family inherited from an element the app does not style", () => {
    // `font-family` inherits, so without the app's stylesheet both `html` and
    // `body` report the user agent's serif. Trusting either is what put
    // generated documents in Times; only the declared token is safe.
    document.documentElement.style.fontFamily = '"Times New Roman"';
    document.body.style.fontFamily = '"Times New Roman"';
    expect(hostFontSans()).toBe("system-ui, sans-serif");
    expect(hostFontMono()).toBe("ui-monospace, monospace");
  });

  it("carries the app's monospace face once it declares one", () => {
    document.documentElement.style.setProperty("--mono", '"Geist Mono", ui-monospace, monospace');
    expect(hostFontMono()).toBe('"Geist Mono", ui-monospace, monospace');
  });
});

describe("an SVG image document", () => {
  it("gets the app's face, so a text node that names none is not a serif", () => {
    document.documentElement.style.setProperty("--font-sans", "Geist, system-ui, sans-serif");
    const svg = decode(svgImageUrl('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><text x="1" y="5">hi</text></svg>'));
    expect(svg).toContain('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10" font-family="Geist, system-ui, sans-serif">');
    expect(svg).toContain("<text x=\"1\" y=\"5\">hi</text>");
  });

  it("leaves a document that names its own family exactly as written", () => {
    const source = '<svg xmlns="http://www.w3.org/2000/svg" font-family="Georgia"><text>hi</text></svg>';
    expect(decode(svgImageUrl(source))).toBe(source);
  });

  it("handles a self-closing root, and content that is not an SVG at all", () => {
    document.documentElement.style.setProperty("--font-sans", "Geist");
    expect(decode(svgImageUrl('<svg xmlns="http://www.w3.org/2000/svg"/>'))).toBe('<svg xmlns="http://www.w3.org/2000/svg" font-family="Geist"/>');
    expect(svgImageUrl("not svg")).toContain("data:image/svg+xml;charset=utf-8,not%20svg");
  });

  it("defines the app's own tokens inside, so a diagram drawn with them renders", () => {
    // The failure this prevents, seen in the field: an agent draws with the
    // app's tokens — which the app's own guidance points it at — and an image
    // document has no stylesheet, so every `fill="var(--card)"` resolves to
    // nothing and the whole diagram arrives as an empty black rectangle.
    document.documentElement.style.setProperty("--bg", "#161618");
    document.documentElement.style.setProperty("--panel", "#1e1e22");
    const svg = decode(svgImageUrl('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="var(--bg)"/></svg>'));
    expect(svg).toContain("<style>:root{--bg:#161618;");
    expect(svg, "`--card` is the token the app paints its panels with").toContain("--card:#1e1e22;");
    // The rule has to land inside the root element or nothing it defines applies:
    // after the opening tag's attributes, before the element closes.
    expect(svg.indexOf("<style>")).toBeGreaterThan(svg.indexOf("viewBox"));
    expect(svg.indexOf("</style>")).toBeLessThan(svg.indexOf("</svg>"));
  });

  it("opens a self-closing root to hold the theme, and leaves an untokenised document alone", () => {
    document.documentElement.style.setProperty("--bg", "#161618");
    // A root that closes itself cannot hold the rule: the style element would
    // sit outside it and the document would not parse at all.
    const selfClosing = decode(svgImageUrl('<svg xmlns="http://www.w3.org/2000/svg" fill="var(--accent)"/>'));
    expect(selfClosing).toContain("<style>:root{--bg:#161618;");
    expect(selfClosing.endsWith("</style></svg>")).toBe(true);
    // No token in the source means no injection: a document written with literal
    // colours stays byte-identical to what the agent wrote.
    const literal = '<svg xmlns="http://www.w3.org/2000/svg" font-family="Georgia"><rect fill="#123"/></svg>';
    expect(decode(svgImageUrl(literal))).toBe(literal);
  });
});
