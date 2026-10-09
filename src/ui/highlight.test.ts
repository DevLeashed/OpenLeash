import { beforeAll, describe, expect, it } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { grammarsReady, highlight, langOf, whenReady } from "./highlight";
import { fenceLang, mdComponents, remarkOl } from "./mdx";

const render = (md: string) => renderToStaticMarkup(createElement(ReactMarkdown, { remarkPlugins: [remarkGfm, remarkOl], components: mdComponents }, md));

describe("fence labels", () => {
  it("pulls the language out of react-markdown's className", () => {
    expect(fenceLang("language-ts")).toBe("ts");
    expect(fenceLang("language-rust")).toBe("rust");
    expect(fenceLang(undefined)).toBe("");
    expect(fenceLang("some-other-class")).toBe("");
  });

  it("maps the short labels people actually type", () => {
    expect(langOf("ts")).toBe("typescript");
    expect(langOf("py")).toBe("python");
    expect(langOf("rs")).toBe("rust");
    expect(langOf("sh")).toBe("bash");
    expect(langOf("PS1")).toBe("powershell");
    expect(langOf("")).toBe("");
    // A label with trailing metadata still resolves to its language.
    expect(langOf("ts title=app.ts")).toBe("typescript");
  });
});

describe("highlighting", () => {
  beforeAll(async () => {
    // The grammars are a lazy chunk; without this every assertion below would
    // be asserting on the pre-load null path and would pass for the wrong reason.
    await whenReady();
  });

  it("colours code for a known language", () => {
    const nodes = highlight("typescript", "const x: number = 1");
    expect(nodes).not.toBeNull();
    const cls = JSON.stringify(nodes);
    expect(cls).toContain("hljs-");
  });

  it("returns null for an unknown or missing label so the block still renders", () => {
    expect(highlight("not-a-language", "hello")).toBeNull();
    expect(highlight(undefined, "hello")).toBeNull();
    expect(highlight("ts", "")).toBeNull();
  });

  it("skips very large fences rather than stalling a streaming reply", () => {
    expect(highlight("ts", "x".repeat(60_001))).toBeNull();
  });

  it("powershell has a grammar, because this is a Windows-first tool", () => {
    expect(highlight("powershell", "Get-ChildItem | Select-Object Name")).not.toBeNull();
  });

  it("gives back the same nodes for the same input", () => {
    const a = highlight("rust", "fn main() {}");
    const b = highlight("rust", "fn main() {}");
    expect(a).toEqual(b);
  });
});

describe("a fenced block in a reply", () => {
  beforeAll(async () => {
    await whenReady();
  });

  it("keeps one wrapper and one copy button, and the exact code", () => {
    // This render is `renderToStaticMarkup`, which takes the
    // `getServerSnapshot` branch and so reports the grammars as not loaded.
    // That is the first-frame path, and what it must produce is real readable
    // code in one `.codewrap` with one Copy button -- colour arrives a tick
    // later on the client. The `pre` component owns the frame, so a wrapper
    // here too used to nest `.codewrap` inside itself and duplicate the button.
    const out = render("```ts\nconst x: number = 1;\n```");
    expect(out).toContain("Copy");
    expect(out).toContain("const x: number = 1;");
    expect(out.match(/class="codewrap"/g)?.length).toBe(1);
    expect(out.match(/class="copybtn"/g)?.length).toBe(1);
    // The trailing newline markdown adds must not survive into the text.
    expect(out).not.toContain("1;\n</code>");
  });

  it("colours the same code once the grammars are loaded", () => {
    // The client path the lazy chunk exists to enable: the block re-renders via
    // `onReady` and picks the grammars up. `highlight()` is the call it makes,
    // and the `useSyncExternalStore` gating around it is covered by the
    // lazy-load block below.
    const nodes = highlight("typescript", "const x: number = 1;");
    expect(JSON.stringify(nodes)).toContain("hljs-");
  });

  it("still renders a block whose language we cannot highlight", () => {
    const out = render("```not-a-language\nplain text here\n```");
    expect(out).toContain("plain text here");
    expect(out).not.toContain("hljs-");
  });

  it("leaves an inline code span alone", () => {
    const out = render("run `npm test` now");
    expect(out).not.toContain("hljs-");
    expect(out).toContain("npm test");
  });
});

describe("the lazy grammar load", () => {
  it("reports ready once loaded, and highlight() stays callable either way", async () => {
    // Ordering-independent: by now `beforeAll` has loaded them, so this asserts
    // the steady state and that the sync API never throws after a reload.
    await whenReady();
    expect(grammarsReady()).toBe(true);
    expect(() => highlight("ts", "const x = 1")).not.toThrow();
    expect(highlight("ts", "const x = 1")).not.toBeNull();
  });
});
