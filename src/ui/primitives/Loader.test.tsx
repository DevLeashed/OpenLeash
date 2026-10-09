import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { AgentColor, Loader } from "./Loader";

const css = readFileSync("src/App.css", "utf8");

describe("shared loading and model-response spinner", () => {
  it.each(["agent", "general"] as const)("sizes the %s SVG without injected styles", (variant) => {
    const html = renderToStaticMarkup(<Loader variant={variant} size={13} label="Loading conversation…" />);
    expect(html).toContain('width="13" height="13"');
    expect(html).toContain('--loader-size:13px');
    expect(html).toContain('role="status"');
    expect(html).not.toContain("<style");
  });

  it("retains the model accent for response spinners", () => {
    const html = renderToStaticMarkup(<AgentColor.Provider value="#abcdef"><Loader variant="agent" /></AgentColor.Provider>);
    expect(html).toContain("color:#abcdef");
    expect(html).toContain("ld-trace-spin");
  });

  it("ships size and motion rules with the app instead of relying on mount-time injection", () => {
    expect(css).toMatch(/\.loader > svg\s*\{[^}]*width: var\(--loader-size, 14px\);[^}]*height: var\(--loader-size, 14px\)/);
    expect(css).toContain("animation: olLoaderTrace .9s linear infinite");
    expect(css).toContain("animation: olLoaderArc .7s linear infinite");
    expect(css).toMatch(/@media \(prefers-reduced-motion: reduce\)\s*\{\s*\.loader-agent[^}]*animation: none/);
  });
});
