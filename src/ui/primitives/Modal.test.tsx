import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("../../App.css", import.meta.url), "utf8");
const chatStatsCss = readFileSync(new URL("../ChatStats.css", import.meta.url), "utf8");

// jsdom has no overflow layout engine. Guard the CSS constraints responsible
// for keeping tall modal forms scrollable without compressing their controls.
describe("Modal scroll layout", () => {
  it("allows the body to shrink and scroll", () => {
    const body = css.match(/\.modal \.mb \{([^}]+)\}/)?.[1];
    expect(body).toMatch(/min-height:\s*0\s*;/);
    expect(body).toMatch(/overflow-y:\s*auto\s*;/);
  });

  it("keeps chat stats scrollable below a pinned header", () => {
    const body = chatStatsCss.match(/\.modal-chat-stats > \.chat-stats-body \{([^}]+)\}/)?.[1];
    expect(body).toMatch(/flex-shrink:\s*1\s*;/);
    expect(body).toMatch(/min-height:\s*0\s*;/);
    expect(body).toMatch(/overflow-y:\s*auto\s*;/);
  });

  it("preserves the height of form rows, headers and footers", () => {
    expect(css).toMatch(/\.modal > :not\(\.mb\), \.modal \.mb > \*\s*\{\s*flex-shrink:\s*0;\s*\}/);
  });

  it("lets a two-column field pair give way instead of clipping its second column", () => {
    // A phone cannot fit two labelled inputs side by side; the pair stacks rather
    // than running the second one off the edge of a vertically-scrolling body.
    expect(css).toMatch(/\.modal \.mb > \.fieldrow\s*\{\s*flex-shrink:\s*1;\s*\}/);
    expect(css).toMatch(/@media \(max-width: 640px\)[\s\S]*\.fieldrow \{\s*flex-direction: column;/);
  });
});
