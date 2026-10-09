/** Smarter markdown for agent replies: clickable file paths, numeric table
 *  columns, and automatic done/open progress bars. */
import { memo, useSyncExternalStore } from "react";
import ReactMarkdown from "react-markdown";
import type { Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { invoke } from "@tauri-apps/api/core";
import { flash, get } from "../store";
import { Thumb } from "./primitives";
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
  const done = head.findIndex((h) => /^(done|completed?|passed|closed)$/.test(h));
  const open = head.findIndex((h) => /^(open|remaining|todo|left|failed|pending)$/.test(h));
  const total = head.findIndex((h) => /^total$/.test(h));
  if (done < 0 || !numeric[done] || (open < 0 || !numeric[open]) && (total < 0 || !numeric[total])) return;
  rows.forEach((r, i) => {
    const bar: Node = { type: "tableCell", children: i === 0 ? [{ type: "text", value: "Progress" }] : [] };
    if (i > 0) {
      const d = num(text(cell(r, done) ?? { type: "x" }));
      const all = open >= 0 ? d + num(text(cell(r, open) ?? { type: "x" })) : num(text(cell(r, total) ?? { type: "x" }));
      prop(bar, "dataBar", all > 0 ? Math.max(0, Math.min(1, d / all)) : -1);
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

export function copyText(t: string, what = "Copied") {
  navigator.clipboard?.writeText(t).then(() => flash(what)).catch(() => flash("Couldn't copy"));
}

/** Plain text of a rendered code block (for its copy button). */
function codeOf(node: any): string {
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(codeOf).join("");
  return node?.props?.children != null ? codeOf(node.props.children) : "";
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
function CodeBlock({ lang, code }: { lang: string; code: string }) {
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
}

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

export const mdComponents: Components = {
  img: ({ src, alt }) => <Thumb src={typeof src === "string" ? src : ""} alt={alt ?? ""} />,
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
      return (
        <td className="barcell">
          {v >= 0 && <span className="cellbar"><span style={{ width: `${v * 100}%`, background: v >= 1 ? "#4ade80" : "var(--accent, #a78bfa)" }} /></span>}
          <span className="cellpct">{v >= 0 ? `${Math.round(v * 100)}%` : "—"}</span>
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

/** Markdown for a run of text that lives inside a sentence, a label or a title:
 *  block elements are unwrapped to their text, so nothing nests a paragraph and
 *  the `**bold**` an agent typed reads as bold rather than as asterisks. */
export const MdInline = memo(({ text }: { text: string | null | undefined }) => (
  <ReactMarkdown remarkPlugins={[remarkGfm, remarkOl]} components={mdComponents} disallowedElements={INLINE_BLOCKS} unwrapDisallowed>
    {text ?? ""}
  </ReactMarkdown>
));
