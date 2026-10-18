/** A streaming reply's markdown, parsed a block at a time.
 *
 *  A reply that is still streaming is re-parsed on every throttled tick, and
 *  parsing it whole each time makes a long answer quadratic: the thousandth line
 *  pays for the 999 before it again, eight times a second. Cut the text at block
 *  boundaries that markdown itself treats as hard edges and only the last block —
 *  the one still growing — is parsed again; the others are memoized on their
 *  source and their subtree is left alone.
 *
 *  The cut is only made while the reply streams. A finished reply is parsed whole
 *  once, so whatever this splitter gets wrong can only ever last as long as the
 *  stream does, and the transcript keeps exactly the rendering it had before. */
import { memo, useMemo } from "react";
import ReactMarkdown from "react-markdown";
import { MD_PLUGINS, mdComponents, replyUrlTransform } from "./mdx";

/** A list item marker at the start of a line: `-`, `+`, `*`, `1.` or `1)`. A line
 *  that opens with one after a blank line may continue the list above (a loose
 *  list), so it is never a place to cut. */
const LIST_MARKER = /^(?:[-+*]|\d{1,9}[.)])(?:[ \t]|$)/;
/** The HTML blocks that run across blank lines (CommonMark types 1–5). Past one
 *  of these the rest of the text is left as one block. */
const LONG_HTML = /^ {0,3}<(?:script|pre|style|textarea|!--|\?|![A-Za-z]|!\[CDATA\[)/i;
/** Link reference and footnote definitions apply to the whole document, so a
 *  text that has any is never cut: the definition may sit far from its use. */
const DEFINITION = /^ {0,3}\[[^\]\n]+\]:/m;

type Fence = { ch: string; len: number; container: boolean };

/** Cut `text` into chunks at block boundaries that are safe to parse on their own:
 *  a blank line followed by an unindented line that is not a list marker, with no
 *  fenced code block open across it. The chunks join back to `text` exactly.
 *
 *  Conservative on purpose — every rule here only ever declines a cut:
 *  - inside a ``` / ~~~ fence nothing is cut, however many blank lines it holds;
 *  - an indented line is a list continuation or indented code, never a cut;
 *  - a list marker after a blank line may continue a loose list, never a cut;
 *  - reference/footnote definitions or a long HTML block stop the cutting;
 *  - the last line is never a cut: it may still be growing (`1` → `1. item`).
 *
 *  A fence opened inside a list item ends where the item does (a non-blank line
 *  back at column 0), which is also how CommonMark reads it — a fence there is
 *  closed by its container, not only by a closing fence. */
export function splitMarkdownBlocks(text: string): string[] {
  if (DEFINITION.test(text) || text.includes("[^")) return [text];
  const out: string[] = [];
  let start = 0;
  let pos = 0;
  let fence: Fence | null = null;
  // Whether the last block may have been a list item or a block quote: decides
  // how an indented fence opener is read, and is cleared at every cut.
  let container = false;
  let prevBlank = false;
  for (;;) {
    const nl = text.indexOf("\n", pos);
    if (nl < 0) break;
    const line = text.slice(pos, text[nl - 1] === "\r" ? nl - 1 : nl);
    const blank = line.trim() === "";
    const unindented = !blank && line[0] !== " " && line[0] !== "\t";
    if (fence) {
      if (fence.container && unindented) {
        // The list item holding the fence is over, and the fence with it. This
        // line is read again below as an ordinary top-level line — even when it
        // is a closing fence: back at column 0 it is outside the item, so it
        // opens a new block rather than closing the one inside.
        fence = null;
      } else {
        if (isCloser(line, fence)) fence = null;
        prevBlank = false;
        pos = nl + 1;
        continue;
      }
    }
    if (prevBlank && unindented && !LIST_MARKER.test(line) && pos > start) {
      out.push(text.slice(start, pos));
      start = pos;
      container = false;
    }
    if (LONG_HTML.test(line)) break;
    const open = /^( *)(`{3,}|~{3,})(.*)$/.exec(line);
    // Four spaces of indent is indented code, not a fence — unless a list item
    // holds it, where the indent belongs to the item.
    if (open && (container || open[1]!.length <= 3) && !(open[2]![0] === "`" && open[3]!.includes("`"))) {
      fence = { ch: open[2]![0]!, len: open[2]!.length, container: container && open[1]!.length > 0 };
      // An unindented fence closes any list or quote above it.
      if (!open[1]) container = false;
      prevBlank = false;
      pos = nl + 1;
      continue;
    }
    if (!blank && (LIST_MARKER.test(line.trimStart()) || line.trimStart().startsWith(">"))) container = true;
    prevBlank = blank;
    pos = nl + 1;
  }
  out.push(text.slice(start));
  return out;
}

function isCloser(line: string, fence: Fence): boolean {
  const m = /^( *)(`{3,}|~{3,})[ \t]*$/.exec(line);
  // At the top level a closer indented four spaces is code inside the fence; in
  // a list item the indent is the item's, so any is accepted there.
  return !!m && (fence.container || m[1]!.length <= 3) && m[2]![0] === fence.ch && m[2]!.length >= fence.len;
}

/** One block, memoized on its source: a finished block is never parsed again. */
const MdBlock = memo(function MdBlock({ source }: { source: string }) {
  return <ReactMarkdown remarkPlugins={MD_PLUGINS} components={mdComponents} urlTransform={replyUrlTransform}>{source}</ReactMarkdown>;
});

/** Markdown that is parsed per block while `split` (the reply is streaming), and
 *  whole otherwise. Keyed by position, so the growing block keeps its DOM between
 *  ticks and a block that just closed is parsed once more, for the last time. */
export function MarkdownBlocks({ source, split }: { source: string; split: boolean }) {
  const blocks = useMemo(() => (split ? splitMarkdownBlocks(source) : [source]), [source, split]);
  return blocks.map((block, i) => <MdBlock key={i} source={block} />);
}
