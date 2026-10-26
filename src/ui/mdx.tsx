/** Smarter markdown for agent replies: clickable file paths, numeric table
 *  columns, and automatic done/open progress bars. */
import { Children, memo, useSyncExternalStore } from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import type { Components, UrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { invoke } from "@tauri-apps/api/core";
import { flash, get } from "../store";
import { ReplyImage } from "./ReplyImage";
import { ReplyTable } from "./ReplyTable";
import { grammarsReady, highlight, onReady } from "./highlight";

// ───────────── remark plugin: annotate the tree before it becomes HTML ─────────────

type Node = { type: string; value?: string; children?: Node[]; align?: (string | null)[]; data?: { hProperties?: Record<string, unknown> } };

const text = (n: Node): string => n.value ?? (n.children ?? []).map(text).join("");
const NUM = /^[\s$€£]*[-+]?[\d,]*\.?\d+\s*[%kKmMbB]?\s*$/;
const num = (s: string) => Number(s.replace(/[^\d.-]/g, ""));
/** Both `PATH` and `NUM` have adjacent quantifiers with no length bound, so a long
 *  run of near-matching input is quadratic — at 64 KB `NUM.test` alone costs
 *  ~10s, and the block is re-entered on every stream tick. Neither pattern can
 *  match a string this long anyway, so the guard costs nothing real. */
const NUM_MAX = 32;
const PATH_MAX = 200;
const isNum = (s: string) => s.length <= NUM_MAX && NUM.test(s);
const isPath = (s: string) => s.length <= PATH_MAX && PATH.test(s);
const prop = (n: Node, k: string, v: unknown) => {
  n.data = n.data ?? {};
  n.data.hProperties = { ...(n.data.hProperties ?? {}), [k]: v };
};
/** Looks like a file path: has an extension or a slash, no spaces. */
export const PATH = /^(?:[A-Za-z]:)?[\w.@~-]*[\\/]?[\w.@~\\/*-]*\.[A-Za-z0-9]{1,8}(?::\d+){0,2}$|^[\w.@~-]+(?:[\\/][\w.@~*-]+)+[\\/]?(?::\d+){0,2}$/;

/** What a progress cell shows. The rounded percentage is the label the reader
 *  sees; the exact fraction goes into the cell's sort key, so two cells that
 *  both read "75%" still order by their real values. */
const barLabel = (fraction: number) => fraction >= 0 ? `${Math.round(fraction * 100)}%` : "—";

function table(t: Node) {
  const rows = t.children ?? [];
  if (rows.length < 2) return;
  const header = rows[0];
  if (!header) return;
  const cols = header.children?.length ?? 0;
  const cell = (r: Node, c: number) => r.children?.[c];
  // Columns where every body cell is a number: right-aligned, tabular figures.
  const numeric: boolean[] = [];
  for (let c = 0; c < cols; c++) {
    const body = rows.slice(1).map((r) => text(cell(r, c) ?? { type: "x" }).trim()).filter(Boolean);
    numeric[c] = body.length > 0 && body.every((s) => isNum(s));
    if (numeric[c]) for (const r of rows) { const x = cell(r, c); if (x) prop(x, "className", ["num"]); }
  }
  // Done + Open (or Done + Total) columns: add a progress column.
  const head = (header.children ?? []).map((h) => text(h).trim().toLowerCase());
  // Evidence stages are independent counts, not interchangeable definitions of done.
  const stages = ["implemented", "tested", "verified", "accepted"].filter(stage => head.includes(stage));
  const totalIndex = head.findIndex(h => h === "total");
  if (stages.length && totalIndex >= 0 && numeric[totalIndex]) {
    for (const stage of stages) {
      const index = head.indexOf(stage);
      if (!numeric[index]) continue;
      rows.forEach((r, i) => {
        const bar: Node = { type: "tableCell", children: i === 0 ? [{ type: "text", value: `${stage[0]!.toUpperCase()}${stage.slice(1)} / total` }] : [] };
        if (i > 0) {
          const value = num(text(cell(r, index) ?? { type: "x" }));
          const denominator = num(text(cell(r, totalIndex) ?? { type: "x" }));
          const fraction = denominator > 0 && value >= 0 && value <= denominator ? value / denominator : -1;
          prop(bar, "dataBar", fraction);
          // Text in the cell, for the same reason as the Progress column below.
          bar.children = [{ type: "text", value: barLabel(fraction) }];
        }
        r.children = [...(r.children ?? []), bar];
        if (i === 0) t.align = [...(t.align ?? []), null];
      });
    }
    return;
  }
  const done = head.findIndex((h) => /^(done|completed?|passed|closed)$/.test(h));
  const open = head.findIndex((h) => /^(open|remaining|todo|left|failed|pending)$/.test(h));
  const total = head.findIndex((h) => /^total$/.test(h));
  if (done < 0 || !numeric[done] || (open < 0 || !numeric[open]) && (total < 0 || !numeric[total])) return;
  rows.forEach((r, i) => {
    const bar: Node = { type: "tableCell", children: i === 0 ? [{ type: "text", value: "Progress" }] : [] };
    if (i > 0) {
      const d = num(text(cell(r, done) ?? { type: "x" }));
      const all = open >= 0 ? d + num(text(cell(r, open) ?? { type: "x" })) : num(text(cell(r, total) ?? { type: "x" }));
      const fraction = all > 0 ? Math.max(0, Math.min(1, d / all)) : -1;
      prop(bar, "dataBar", fraction);
      // The cell carries the percentage as its own text. The bar beside it is
      // drawn by the `td` component, so a cell element that holds nothing is
      // all the sortable reply table can see — and it orders a column by the
      // text it can read, so a textless cell sorts as an empty string and the
      // Progress column silently never reorders. Keeping the label in the tree
      // also means the number is in the transcript, not only in a style.
      bar.children = [{ type: "text", value: barLabel(fraction) }];
    }
    r.children = [...(r.children ?? []), bar];
  });
  t.align = [...(t.align ?? []), null];
}

export function remarkOl() {
  return (tree: Node) => {
    const walk = (n: Node) => {
      if (n.type === "table") table(n);
      if (n.type === "inlineCode") prop(n, "dataInline", "1");
      if (n.type === "paragraph" && (n.children ?? []).filter((c) => c.type === "inlineCode").length > 5) prop(n, "dataChips", "1");
      n.children?.forEach(walk);
    };
    walk(tree);
  };
}

// ───────────── components ─────────────

// Imported for this file's own code blocks and re-exported so existing
// `from "./mdx"` imports keep working: the behaviour (including the fallback for
// browsers without the async clipboard) lives in exactly one place. See
// `clipboard.ts` for why the obvious optional-chain form did nothing on remote.
import { copyText } from "./clipboard";
export { copyText };

/** Plain text of a rendered code block (for its copy button). */
function codeOf(node: unknown): string {
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(codeOf).join("");
  const el = node as { props?: { children?: unknown } } | null | undefined;
  return el?.props?.children != null ? codeOf(el.props.children) : "";
}

function openPath(path: string) {
  const s = get();
  const task = s.task ? s.tasks[s.task] : undefined;
  invoke("open_path", { cwd: task?.cwd ?? "", path }).catch((e) => flash(String(e)));
}

function Chips({ children }: { children: React.ReactNode }) {
  return <div className="chips"><p>{children}</p></div>;
}

/** The language label react-markdown puts on a fence, as `language-ts` →
 *  `ts`. Unknown or absent labels come back as "", which means plain text. */
export function fenceLang(className: string | undefined): string {
  const m = /(?:^|\s)language-(\S+)/.exec(className ?? "");
  return m?.[1] ?? "";
}

/** A fenced code block, syntax-highlighted when the label names a grammar.
 *  Falls back to the uncoloured text rather than dropping the block, so an
 *  unknown language (or one we chose not to ship) still reads correctly. */
const CodeBlock = memo(function CodeBlock({ lang, code }: { lang: string; code: string }) {
  // The grammars are a lazy chunk, so the first render of a code block is plain
  // text and the colour arrives a tick later. Subscribing through
  // `useSyncExternalStore` makes that a local re-render of this one block
  // rather than of the transcript, which matters because the block is usually
  // inside a reply that is still streaming.
  const ready = useSyncExternalStore(
    (cb) => onReady(cb),
    () => grammarsReady(),
    // Server/static rendering has no chunk to wait for, and it produces the
    // initial markup: report not-ready and let it fall back to plain text
    // rather than reading a module-level flag that may not be initialised yet.
    () => false,
  );
  const text = code.replace(/\n$/, "");
  const nodes = lang && ready ? highlight(lang, text) : null;
  // Just the inner <code>, no wrapper: the `pre` component below already emits
  // the .codewrap frame and the copy button, so wrapping here too produced a
  // nested .codewrap with a duplicate Copy button on every fenced block.
  return nodes ? <code className={lang ? `language-${lang}` : undefined}>{renderNodes(nodes)}</code> : <code>{text}</code>;
});

/** Turn a hast fragment into React nodes. The tree comes from highlight.js, so
 *  every node is one of three shapes: element, text, or root. */
function renderNodes(nodes: import("hast").RootContent[]): React.ReactNode {
  return nodes.map((n, i) => {
    if (n.type === "text") return n.value;
    if (n.type !== "element") return null;
    const cls = (n.properties?.className as string[] | undefined)?.join(" ");
    const kids = "children" in n ? renderNodes(n.children as import("hast").RootContent[]) : null;
    return <span key={i} className={cls}>{kids}</span>;
  });
}

/**
 * react-markdown's default blanks every URL with a scheme it doesn't know, which
 * also ate a reply's `data:image/png` picture and its `C:\out\chart.png` path
 * (`C:` reads as a scheme). An image's `src` is passed through untouched so
 * `ReplyImage` can decide: it is the one place that sorts https / inline data /
 * a confined local path from everything else, and it never puts the raw value
 * anywhere but an `<img>`. Links keep the default sanitising.
 */
export const replyUrlTransform: UrlTransform = (url, key, node) =>
  key === "src" && node.tagName === "img" ? url : defaultUrlTransform(url);

export const mdComponents: Components = {
  table: ({ children }) => <ReplyTable>{children}</ReplyTable>,
  img: ({ src, alt, title }) => <ReplyImage key={src} src={typeof src === "string" ? src : ""} alt={alt ?? ""} title={title} />,
  code: ({ node, className, children, ...rest }) => {
    const inline = (node?.properties as Record<string, unknown> | undefined)?.dataInline != null;
    const s = String(children ?? "");
    if (inline && isPath(s.trim())) {
      return <code className="pathchip" title="Open file" onClick={() => openPath(s)}>{children}</code>;
    }
    const { "data-inline": _i, ...attrs } = rest as Record<string, unknown>;
    // A fenced block arrives here as the `code` inside `pre`. Colouring it needs
    // the fence label and the whole text, so the work happens in `CodeBlock`.
    if (!inline) return <CodeBlock lang={fenceLang(className)} code={s} />;
    return <code className={className} {...attrs}>{children}</code>;
  },
  p: ({ node, children }) => (node?.properties as Record<string, unknown> | undefined)?.dataChips != null ? <Chips>{children}</Chips> : <p>{children}</p>,
  td: ({ node, children, className, style }) => {
    const bar = (node?.properties as Record<string, unknown> | undefined)?.dataBar;
    if (bar != null) {
      const v = Number(bar);
      // The label comes from the cell's own text, which the table plugin puts
      // there; the fallback only covers a bar that arrived without one.
      const label = Children.count(children) ? children : barLabel(v);
      return (
        <td className="barcell">
          {v >= 0 && <span className="cellbar"><span style={{ width: `${v * 100}%`, background: v >= 1 ? "#4ade80" : "var(--accent, #a78bfa)" }} /></span>}
          <span className="cellpct">{label}</span>
        </td>
      );
    }
    return <td className={className} style={style}>{children}</td>;
  },
  a: ({ href, children }) => <a href={href} target="_blank" rel="noreferrer">{children}</a>,
  pre: ({ children }) => (
    <div className="codewrap">
      <pre>{children}</pre>
      <div className="copybtn" title="Copy code" onClick={() => copyText(codeOf(children).replace(/\n$/, ""), "Code copied")}>Copy</div>
    </div>
  ),
};

// ───────────── inline markdown ─────────────

// Hoisted: a fresh plugin or component object on every render defeats
// react-markdown's identity checks, and a question card re-renders on every key.
const INLINE_BLOCKS = ["p", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "pre", "blockquote", "hr", "table", "thead", "tbody", "tr", "th", "td"];

/// The remark stack every `ReactMarkdown` in the app uses, built once.
///
/// A plugin array built inline on each render is a new identity every time, and
/// react-markdown answers by rebuilding its whole processor (`unified().use(...)`)
/// for that render — so the cost is paid per render of every markdown surface,
/// including the ones inside a streamed reply. Shared with `mdComponents` below
/// so the two halves a `ReactMarkdown` needs are hoisted together.
export const MD_PLUGINS = [remarkGfm, remarkOl];

/** Markdown for a run of text that lives inside a sentence, a label or a title:
 *  block elements are unwrapped to their text, so nothing nests a paragraph and
 *  the `**bold**` an agent typed reads as bold rather than as asterisks. */
export const MdInline = memo(({ text }: { text: string | null | undefined }) => (
  <ReactMarkdown remarkPlugins={MD_PLUGINS} components={mdComponents} urlTransform={replyUrlTransform} disallowedElements={INLINE_BLOCKS} unwrapDisallowed>
    {text ?? ""}
  </ReactMarkdown>
));
