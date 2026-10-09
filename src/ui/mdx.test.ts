import { describe, expect, it } from "vitest";
import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import { PATH, remarkOl } from "./mdx";

const run = (md: string) => {
  const p = unified().use(remarkParse).use(remarkGfm);
  const tree = p.parse(md);
  (remarkOl() as (t: unknown) => void)(tree);
  return tree as any;
};

describe("remarkOl", () => {
  it("marks numeric columns and adds a progress column for Done/Open", () => {
    const t = run("| Bucket | Files | Done | Open |\n|---|---|---|---|\n| Active | 73 | 1,968 | 983 |\n| Limbo | 1 | 0 | 0 |").children[0];
    const [head, active, limbo] = t.children;
    expect(head.children.length).toBe(5);
    expect(head.children[0].data?.hProperties?.className).toBeUndefined();
    expect(head.children[2].data.hProperties.className).toEqual(["num"]);
    expect(active.children[4].data.hProperties.dataBar).toBeCloseTo(1968 / 2951);
    expect(limbo.children[4].data.hProperties.dataBar).toBe(-1);
  });
  it("flags chip-heavy paragraphs only", () => {
    expect(run("`a` `b` `c` `d` `e` `f`").children[0].data.hProperties.dataChips).toBe("1");
    expect(run("`a` and `b`").children[0].data).toBeUndefined();
  });
  it("recognises file paths, not arbitrary code", () => {
    for (const p of ["src/ui/mdx.tsx", "TODO/*.md", "lib.rs:42", "C:\\x\\y.ts", "src-tauri/src"]) expect(PATH.test(p), p).toBe(true);
    for (const p of ["repo-wide-bug-hunt-2026-08-21", "npm run build", "foo()", "x = 1"]) expect(PATH.test(p), p).toBe(false);
  });
});

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ReactMarkdown from "react-markdown";
import { mdComponents, MdInline } from "./mdx";

describe("rendered", () => {
  it("wires flags through to the components", () => {
    const md = "| Bucket | Done | Open |\n|---|---|---|\n| Active | 1,968 | 983 |\n\nSee `src/ui/mdx.tsx` and `npm run x`.\n\n`a` `b` `c` `d` `e` `f`";
    const html = renderToStaticMarkup(createElement(ReactMarkdown, { remarkPlugins: [remarkGfm, remarkOl], components: mdComponents }, md));
    expect(html).toContain('class="num"');
    expect(html).toContain("cellbar");
    expect(html).toContain('class="pathchip"');
    expect(html).toContain("<code>npm run x</code>");
    expect(html).toContain('class="chips"');
  });
});

describe("inline markdown", () => {
  const html = (text: string | null) => renderToStaticMarkup(createElement(MdInline, { text }));

  // A question is a sentence inside a sentence-sized box, so it has to read as
  // prose: emphasis, code and links are the whole point of the pipe.
  it("renders the emphasis and code an agent types by hand", () => {
    const out = html("**the upstream side** isn't self-consistent; see `src/ui/Overlays.tsx`.");
    expect(out).toContain("<strong>the upstream side</strong>");
    expect(out).toContain('class="pathchip"');
    expect(out).not.toContain("**");
    expect(out).not.toContain("`");
  });

  it("keeps a link clickable and out to a new tab", () => {
    const out = html("see [the report](https://example.com/r) for detail");
    expect(out).toContain('href="https://example.com/r"');
    expect(out).toContain('target="_blank"');
    expect(out).toContain('rel="noreferrer"');
  });

  // react-markdown wraps loose text in a paragraph. Unwrapped, so a label can't
  // smuggle a block into a flex row and break the card's layout.
  it("unwraps block elements so nothing nests a paragraph", () => {
    const out = html("Which **database**?\n\n- `src/lib.rs`\n- `Cargo.toml`");
    expect(out).not.toContain("<p>");
    expect(out).not.toContain("<ul>");
    expect(out).not.toContain("<li>");
    // The text of the list survives; only the tags go.
    expect(out).toContain("src/lib.rs");
    expect(out).toContain("Cargo.toml");
  });

  it("leaves plain text exactly as it was", () => {
    expect(html("Which database?")).toBe("Which database?");
    // A backslash or a lone asterisk is the agent's, not a broken construct.
    expect(html("a * b")).toBe("a * b");
    expect(html("")).toBe("");
    expect(html(null)).toBe("");
  });
});
