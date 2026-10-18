// @vitest-environment jsdom
// A streaming reply is parsed a block at a time so that only its growing tail is
// parsed again on each tick (see `mdBlocks.tsx`). That is only worth having if
// the blocks render exactly what the whole reply renders: every fixture here is
// rendered whole and rendered block by block, and the two have to match.
import { describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ReactMarkdown from "react-markdown";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));

import { MD_PLUGINS, mdComponents, replyUrlTransform } from "./mdx";
import { splitMarkdownBlocks } from "./mdBlocks";

const html = (md: string) => renderToStaticMarkup(createElement(ReactMarkdown, { remarkPlugins: MD_PLUGINS, components: mdComponents, urlTransform: replyUrlTransform }, md))
  // Whole, react-markdown puts a "\n" text node between top-level blocks; the
  // blocks rendered apart have none at their edges. Neither renders anything
  // (raw HTML is shown as escaped text, so that edge counts too).
  .replace(/>\s+(?=<|&lt;)/g, ">");

const FIXTURES: Record<string, string> = {
  prose: "# Title\n\nFirst paragraph with **bold**.\n\nSecond paragraph.\n\n## Next\n\nThird.\n",
  fence: "Intro.\n\n```ts\nconst a = 1;\n\nfunction f() {}\n\n// still code\n```\n\nAfter the code.\n",
  tildeFence: "Intro.\n\n~~~\na\n\nb ``` not a closer\n\nc\n~~~\n\nAfter.\n",
  longFence: "````md\n```\ninner\n```\n\nstill inside\n````\n\nout\n",
  looseList: "- one\n\n- two\n\n- three\n\nAfter the list.\n",
  tightThenPara: "- one\n- two\n\nParagraph.\n\n- again\n",
  orderedContinuation: "1. first\n\n   more of first\n\n2. second\n\n3. third\n\nDone.\n",
  orderedStart: "3. three\n4. four\n\nBreak.\n\n7. seven\n",
  fenceInList: "1. Run it:\n\n   ```sh\n   npm ci\n\n   npm test\n   ```\n\n2. Then:\n\n   ```\n   x\n\ny at column 0 closes the item\n   ```\n\nEnd.\n",
  misindentedCloser: "- a\n  ```\n  x\n```\n\nstill code\n\nmore\n",
  table: "| a | b |\n| - | - |\n| 1 | 2 |\n\n| done | open |\n| ---- | ---- |\n| 3 | 1 |\n\nText.\n",
  quote: "> quoted\n> more\n\n> another quote\n\nplain\n",
  indentedCode: "Para.\n\n    code line\n\n    more code\n\nBack.\n",
  thematic: "Para\n\n---\n\nNext\n\n***\n",
  setextLike: "Heading\n=======\n\nText\n\n---\n",
  referenceLinks: "See [the docs][d].\n\nMore text.\n\n[d]: https://example.com\n",
  footnotes: "A claim.[^1]\n\nMore.\n\n[^1]: The source.\n",
  htmlComment: "Before.\n\n<!-- a comment\n\nthat spans -->\n\nAfter.\n",
  crlf: "One.\r\n\r\nTwo.\r\n\r\n```\r\na\r\n\r\nb\r\n```\r\n\r\nThree.\r\n",
  nestedList: "- a\n  - b\n\n    ```\n    code\n\n    more\n    ```\n\nout\n",
  inlineBackticks: "```not a fence``` inline\n\nnext\n",
};

describe("splitting a streaming reply into blocks", () => {
  it.each(Object.entries(FIXTURES))("renders %s the same block by block as whole", (_name, md) => {
    const blocks = splitMarkdownBlocks(md);
    expect(blocks.join(""), "the blocks are the text, cut, nothing lost").toBe(md);
    expect(blocks.map(html).join("")).toBe(html(md));
  });

  it.each(Object.entries(FIXTURES))("renders every streamed prefix of %s the same block by block", (_name, md) => {
    // Mid-stream is when the cut is actually used: an unclosed fence, a list
    // whose next item has not arrived, a table one row in.
    for (let n = md.indexOf("\n") + 1; n > 0 && n <= md.length; n = md.indexOf("\n", n) + 1) {
      const prefix = md.slice(0, n);
      expect(splitMarkdownBlocks(prefix).map(html).join(""), JSON.stringify(prefix)).toBe(html(prefix));
    }
  });

  it("cuts prose at its blank lines", () => {
    expect(splitMarkdownBlocks(FIXTURES.prose!)).toEqual(["# Title\n\n", "First paragraph with **bold**.\n\n", "Second paragraph.\n\n", "## Next\n\n", "Third.\n"]);
  });

  it("never cuts inside a fenced code block", () => {
    const blocks = splitMarkdownBlocks(FIXTURES.fence!);
    expect(blocks).toEqual(["Intro.\n\n", "```ts\nconst a = 1;\n\nfunction f() {}\n\n// still code\n```\n\n", "After the code.\n"]);
  });

  it("never cuts in front of a list item, which may continue a loose list", () => {
    expect(splitMarkdownBlocks(FIXTURES.looseList!)).toEqual(["- one\n\n- two\n\n- three\n\n", "After the list.\n"]);
  });

  it("does not cut a text with reference or footnote definitions", () => {
    expect(splitMarkdownBlocks(FIXTURES.referenceLinks!)).toHaveLength(1);
    expect(splitMarkdownBlocks(FIXTURES.footnotes!)).toHaveLength(1);
  });

  it("stops cutting at an HTML block that runs across blank lines", () => {
    expect(splitMarkdownBlocks(FIXTURES.htmlComment!)).toEqual(["Before.\n\n", "<!-- a comment\n\nthat spans -->\n\nAfter.\n"]);
  });

  it("leaves the last, still-growing line in the trailing block", () => {
    // `1` may yet become `1. item`, which would continue the list above.
    expect(splitMarkdownBlocks("- a\n\n1")).toEqual(["- a\n\n1"]);
    expect(splitMarkdownBlocks("Para.\n\nNext")).toEqual(["Para.\n\nNext"]);
    expect(splitMarkdownBlocks("Para.\n\nNext\n")).toEqual(["Para.\n\n", "Next\n"]);
  });

  it("keeps earlier blocks unchanged as the text grows, so their memo holds", () => {
    const full = FIXTURES.orderedContinuation! + FIXTURES.fence! + FIXTURES.prose!;
    let prev: string[] = [];
    for (let n = 1; n <= full.length; n++) {
      const blocks = splitMarkdownBlocks(full.slice(0, n));
      // Every block but the last one before is final: it is still there, unchanged.
      for (let i = 0; i < prev.length - 1; i++) expect(blocks[i]).toBe(prev[i]);
      prev = blocks;
    }
  });
});
