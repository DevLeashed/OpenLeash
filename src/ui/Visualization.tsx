import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { RefreshCw, Code2, Square, Copy, Maximize2, Minimize2 } from "lucide-react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";
import { copyText, MD_PLUGINS, mdComponents, replyUrlTransform } from "./mdx";
import { Button, Thumb, useEscapeClose } from "./primitives";
import { ImageExplanation } from "./ImageExplanation";
import { svgImageUrl } from "./hostTheme";
import { buildVisualizationDocument, captureTheme } from "./vizDocument";
import { listenForVisualizationDraft, type VisualizationFeedbackDraft } from "./visualizationFeedback";

// Re-exported for callers and tests that treat the viewer as the home of the
// document builder; the implementation lives beside the frame that runs it.
export { buildVisualizationDocument, captureTheme };

export const MAX_VIZ_BYTES = 240 * 1024; // leave space for the document wrapper
export const MAX_VIZ_COUNT = 8;
const toolbarButtonStyle = { height: 27, padding: "0 9px", borderRadius: 7, fontSize: 11.5, gap: 6 } as const;
export type VizPart = { kind: "markdown"; source: string } | { kind: "visualization" | "diagram" | "image"; source: string } | { kind: "placeholder"; source: string; message?: string };
/** Parse fences before rendering: Markdown accepts unclosed fences while streaming.
 *
 *  Two syntaxes qualify, and both are read here rather than in Markdown, which
 *  has no idea what these blocks mean: an exact, top-level `openleash-*` fence,
 *  and a bare complete top-level `<svg>` block. Everything else stays inert
 *  Markdown — including raw HTML that is not one whole SVG document. */
export function visualizationParts(text: string): VizPart[] {
  const lines = text.split(/\r?\n/); const out: VizPart[] = []; let plain: string[] = []; let count = 0;
  /** One diagram/visualization, whichever syntax named it. The bounds are the
   *  reply's, not the fence's: an unfenced SVG shares them, so it cannot smuggle
   *  a block past the count or the size limit. */
  const emit = (kind: "visualization" | "diagram" | "image", source: string, closed: boolean) => {
    if (plain.length) out.push({ kind: "markdown", source: plain.join("\n") });
    plain = [];
    if (!closed) out.push({ kind: "placeholder", source });
    else if (!source.trim()) out.push({ kind: "placeholder", source, message: "This interactive visualization is empty." });
    else if (count >= MAX_VIZ_COUNT) out.push({ kind: "placeholder", source, message: `Only ${MAX_VIZ_COUNT} visualizations per reply can run.` });
    else if (new TextEncoder().encode(source).length > MAX_VIZ_BYTES) out.push({ kind: "placeholder", source, message: "This interactive visualization is too large to run." });
    else { out.push({ kind, source }); count++; }
  };
  for (let i = 0; i < lines.length; i++) {
    const opening = /^( {0,3})(`{3,}|~{3,})(.*)$/.exec(lines[i] ?? "");
    if (!opening) {
      // A bare SVG. Only at the start of a block, and only when the whole block
      // is one complete SVG document: an SVG nested in other raw HTML, or one
      // being discussed inline in a sentence, is not a diagram and stays text.
      const end = i === 0 || !(lines[i - 1] ?? "").trim() ? bareSvgEnd(lines, i) : -1;
      if (end >= i) {
        const source = lines.slice(i, end + 1).join("\n");
        if (isSvgDocument(source)) { emit("diagram", source, true); i = end; continue; }
      }
      plain.push(lines[i] ?? "");
      continue;
    }
    const marker = opening[2]!; let end = i + 1;
    const closing = new RegExp(`^ {0,3}${marker[0]}{${marker.length},}\\s*$`);
    while (end < lines.length && !closing.test(lines[end] ?? "")) end++;
    const source = lines.slice(i + 1, end).join("\n");
    const tag = opening[3];
    if (tag !== "openleash-viz" && tag !== "openleash-svg" && tag !== "openleash-image") plain.push(...lines.slice(i, Math.min(end + 1, lines.length)));
    else emit(tag === "openleash-svg" ? "diagram" : tag === "openleash-image" ? "image" : "visualization", source, end < lines.length);
    i = end;
  }
  if (plain.length) out.push({ kind: "markdown", source: plain.join("\n") });
  return out;
}

/** The line that ends a bare SVG block, or -1 when this is not one.
 *
 *  Blank lines do not end it, even though Markdown's own HTML-block rule says
 *  they do: a real diagram is written with them between its groups, and that rule
 *  is part of why the field case showed as a wall of escaped XML — Markdown broke
 *  one element into half a dozen blocks and the rest of the reply carried on
 *  around it. What bounds the block instead is its closing tag, and what proves
 *  it was a diagram at all is `isSvgDocument`: everything between has to parse as
 *  exactly one `<svg>` element, so prose that merely mentions `<svg` collects a
 *  range that fails the parse and stays text. */
function bareSvgEnd(lines: string[], start: number): number {
  if (!/^ {0,3}<svg(?=[\s/>])/i.test(lines[start] ?? "")) return -1;
  for (let end = start; end < lines.length; end++) {
    if (/<\/svg>\s*$/.test(lines[end] ?? "")) return end;
  }
  return -1;
}

/** Is this all one drawn SVG document? Parsed detached, like `diagramTitle`, so
 *  the check never touches the application's DOM.
 *
 *  Well-formedness alone is too weak to be the rule, because loose text is legal
 *  XML: a range that swallowed a paragraph between two `<svg>` lines parses too,
 *  and drawing it would drop that paragraph from the reader's view. A diagram
 *  puts its words in `<text>`/`<title>`/`<desc>`, never directly between its
 *  elements, so the root's own text nodes — and a root with nothing to draw —
 *  mean this was prose, not markup. */
function isSvgDocument(source: string): boolean {
  // No DOM to check with: the image either renders or reports that it could not,
  // which is the same bargain the fenced path makes.
  if (typeof DOMParser === "undefined") return true;
  const doc = new DOMParser().parseFromString(source, "image/svg+xml");
  if (doc.querySelector("parsererror")) return false;
  const root = doc.documentElement;
  if (root?.localName !== "svg") return false;
  const kids = [...root.childNodes];
  return kids.some((n) => n.nodeType === 1) && !kids.some((n) => n.nodeType === 3 && (n.nodeValue ?? "").trim() !== "");
}

function VisualizationPlaceholder({ source, message }: { source: string; message: string }) {
  const [showSource, setShowSource] = useState(false);
  return <section aria-label="Interactive visualization unavailable" style={{ border: "1px solid var(--line)", borderRadius: 10, margin: "12px 0", padding: 10 }}>
    <p role="status" aria-live="polite" style={{ margin: "0 0 8px", color: "var(--mut)" }}>{message}</p>
    <Button variant="ghost" className="viz-toolbar-button" aria-expanded={showSource} onClick={() => setShowSource((value) => !value)}><Code2 size={13} />{showSource ? "Hide source" : "Source"}</Button>
    {showSource && <pre style={{ padding: 12, maxHeight: 320, overflow: "auto", whiteSpace: "pre-wrap" }}><code>{source}</code></pre>}
  </section>;
}
/** CSS values are host-owned, but escape structural characters defensively. The
 *  wrapper precedes any supplied head/body so tokens exist when scripts run. */
/** CSS values are host-owned, but escape structural characters defensively. The
 * wrapper precedes any supplied head/body so tokens exist when scripts run. */
export function Visualization({ source, autoStart = false, label, onFeedbackDraft }: { source: string; autoStart?: boolean; label?: string; onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void }) {
  const title = label?.trim() || "Interactive visualization";
  const frame = useRef<HTMLIFrameElement>(null);
  const viewer = useRef<HTMLDivElement>(null);
  const returnFocus = useRef<HTMLButtonElement | null>(null);
  const [height, setHeight] = useState(420);
  const [width, setWidth] = useState("100%");
  const [expanded, setExpanded] = useState(false);
  const collapse = useCallback(() => setExpanded(false), []);
  useEscapeClose(expanded, collapse);
  useEffect(() => {
    if (!expanded) return;
    const opener = returnFocus.current;
    opener?.focus();
    const onFocus = (event: FocusEvent) => {
      if (event.target instanceof Node && !viewer.current?.contains(event.target)) opener?.focus();
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Tab" || !viewer.current) return;
      const stops = [...viewer.current.querySelectorAll<HTMLElement>("button:not([disabled]), select, iframe")];
      const first = stops[0], last = stops[stops.length - 1];
      if (event.shiftKey ? document.activeElement === first : document.activeElement === last) {
        event.preventDefault(); (event.shiftKey ? last : first)?.focus();
      }
    };
    window.addEventListener("focusin", onFocus);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("focusin", onFocus);
      window.removeEventListener("keydown", onKey, true);
      opener?.focus();
    };
  }, [expanded]);
  const [id, setId] = useState<string | null>(null); const [busy, setBusy] = useState(autoStart); const [error, setError] = useState("");
  const [showSource, setShowSource] = useState(false); const [attempt, setAttempt] = useState(0);
  const current = useRef<string | null>(null); const generation = useRef(0);
  const release = (token: string) => { void invoke("visualization_release", { id: token }).catch(() => {}); };
  useEffect(() => {
    if (!id || !onFeedbackDraft || !frame.current) return;
    return listenForVisualizationDraft(frame.current, onFeedbackDraft);
  }, [id, onFeedbackDraft]);
  const publish = useCallback(async () => {
    generation.current++;
    const gen = generation.current;
    if (current.current) release(current.current);
    current.current = null; setId(null); setBusy(true); setError("");
    try {
      const token = await invoke<string>("visualization_publish", { source: buildVisualizationDocument(source, captureTheme()) });
      if (generation.current !== gen) { release(token); return; }
      current.current = token; setId(token);
    } catch (e) { if (generation.current === gen) setError(String(e)); }
    finally { if (generation.current === gen) setBusy(false); }
  }, [source]);
  useEffect(() => {
    if (autoStart) void publish();
  }, [autoStart, publish, attempt]);
  useEffect(() => () => {
    generation.current++;
    if (current.current) release(current.current);
    current.current = null;
  }, [source]);
  const stop = () => {
    generation.current++;
    if (current.current) release(current.current);
    current.current = null; setId(null); setBusy(false); collapse();
  };
  return <section className="viz-frame" aria-label={title}>
    {expanded && <div aria-hidden="true" onClick={collapse} style={{ position: "fixed", inset: 0, zIndex: 959, background: "rgba(0,0,0,.65)" }} />}
    {/* Change only host layout: moving the iframe to a portal would reload its document and lose interactive state. */}
    <div ref={viewer} className={expanded ? "viz-expanded-viewer" : undefined} role={expanded ? "dialog" : undefined} aria-modal={expanded ? true : undefined} aria-label={expanded ? `${title} preview` : undefined} style={expanded ? { position: "fixed", inset: 16, zIndex: 960, display: "flex", flexDirection: "column", background: "var(--bg)", border: "1px solid var(--line)", borderRadius: 10, overflow: "hidden", boxShadow: "0 24px 80px rgba(0,0,0,.5)" } : undefined}>
    <div role="toolbar" aria-label={`${title} controls`} style={{ display: "flex", gap: 6, flexWrap: "wrap", padding: "9px 12px", alignItems: "center", flexShrink: 0, borderBottom: "1px solid var(--line)", background: "var(--panel)" }}>
      <strong title={title} style={{ flex: 1, minWidth: 160, fontSize: 13 }}>{title}</strong>
      {(id || expanded) && <Button variant="ghost" style={toolbarButtonStyle} aria-expanded={expanded} aria-haspopup="dialog" onClick={(event) => { returnFocus.current = event.currentTarget; setExpanded(value => !value); }}>{expanded ? <Minimize2 size={13} /> : <Maximize2 size={13} />}{expanded ? "Collapse preview" : "Expand preview"}</Button>}
      {!busy && !id && !error && <Button variant="primary" style={toolbarButtonStyle} onClick={() => void publish()}><RefreshCw size={13} />{autoStart ? "Restart preview" : "Preview"}</Button>}
      <label style={{ fontSize: 12 }}>Height <select aria-label="Preview height" value={height} onChange={e => setHeight(Number(e.target.value))}><option value={280}>Compact</option><option value={420}>Standard</option><option value={720}>Tall</option></select></label>
      <label style={{ fontSize: 12 }}>Width <select aria-label="Preview width" value={width} onChange={e => setWidth(e.target.value)}><option value="100%">Responsive</option><option value="375px">Phone</option><option value="768px">Tablet</option></select></label>
      <Button variant="ghost" style={toolbarButtonStyle} aria-expanded={showSource} onClick={() => setShowSource(!showSource)}><Code2 size={13} />{showSource ? "Hide source" : "Source"}</Button>
      <Button variant="ghost" style={toolbarButtonStyle} onClick={() => copyText(source, "Visualization source copied")}><Copy size={13} />Copy source</Button>
      {(busy || id) && <Button variant="ghost" style={{ ...toolbarButtonStyle, color: "var(--red)" }} onClick={stop}><Square size={12} />Stop</Button>}
    </div>
    {busy && <p role="status" aria-live="polite" style={{ padding: "0 10px", color: "var(--mut)" }}>Starting interactive preview…</p>}
    {error && <p role="alert" style={{ padding: 10 }}>{error} <Button variant="ghost" style={toolbarButtonStyle} onClick={() => autoStart ? setAttempt(value => value + 1) : void publish()}><RefreshCw size={13} />Retry preview</Button></p>}
    {id && <iframe ref={frame} key={id} title={`${title} preview`} sandbox="allow-scripts" referrerPolicy="no-referrer" src={convertFileSrc(id, "openleash-viz")} style={{ display: "block", width, maxWidth: "100%", height: expanded ? "100%" : height, flex: expanded ? 1 : undefined, minHeight: expanded ? 0 : undefined, border: 0, background: "var(--bg)" }} />}
    {showSource && <pre style={{ padding: 12, maxHeight: expanded ? "30vh" : 320, flexShrink: 0, overflow: "auto", whiteSpace: "pre-wrap" }}><code>{source}</code></pre>}
    </div>
  </section>;
}
export function diagramTitle(source: string): string {
  // Parse detached XML only; never insert supplied nodes into the application.
  if (typeof DOMParser === "undefined") return "Agent-generated diagram";
  const doc = new DOMParser().parseFromString(source, "image/svg+xml");
  if (doc.querySelector("parsererror")) return "Agent-generated diagram";
  return doc.querySelector("title")?.textContent?.trim().slice(0, 160) || "Agent-generated diagram";
}

/** Image-context SVG disables scripts, foreign DOM access and external resources. */
function InlineDiagram({ source }: { source: string }) {
  const title = diagramTitle(source);
  const [failed, setFailed] = useState(false);
  const [showSource, setShowSource] = useState(false);
  return <section aria-label={title} style={{ border: "1px solid var(--line)", borderRadius: 10, margin: "12px 0", padding: 12 }}>
    <div role="toolbar" aria-label="Diagram controls" style={{ display: "flex", gap: 8, marginBottom: 10 }}>
      <strong style={{ flex: 1 }}>{title}</strong>
      <Button variant="ghost" aria-expanded={showSource} onClick={() => setShowSource(value => !value)}>{showSource ? "Hide source" : "Source"}</Button>
      <Button variant="ghost" onClick={() => copyText(source, "Diagram source copied")}>Copy source</Button>
    </div>
    {failed ? <p role="status">This diagram could not be displayed. Source is still available.</p> : <Thumb src={svgImageUrl(source)} alt={title} onError={() => setFailed(true)} style={{ display: "block", width: "100%", maxHeight: 560, objectFit: "contain" }} />}
    {showSource && <pre style={{ maxHeight: 320, overflow: "auto", whiteSpace: "pre-wrap" }}><code>{source}</code></pre>}
  </section>;
}

export function AssistantMarkdown({ text, streaming = false }: { text: string; streaming?: boolean }) {
  // Memoized on the text alone. Splitting the reply into lines and building a
  // matcher per fence is O(reply length), and this component re-renders on every
  // throttled tick of a streamed reply (~8/s at MD_THROTTLE_MS), so unmemoized it
  // re-scanned the whole answer each time for a part list that only changes when
  // the text does. `streaming` is deliberately not a dependency of the parts: it
  // only affects the placeholder message below.
  const parts = useMemo(() => visualizationParts(text), [text]);
  const mapped = parts.map((part, i) => part.kind === "visualization"
    ? <Visualization key={`${i}:${part.source}`} source={part.source} autoStart />
    : part.kind === "image"
      ? <ImageExplanation key={`${i}:${part.source}`} source={part.source} />
    : part.kind === "diagram"
      ? <InlineDiagram key={`${i}:${part.source}`} source={part.source} />
    : part.kind === "placeholder"
      ? <VisualizationPlaceholder key={i} source={part.source} message={part.message ?? (streaming ? "Working on the interactive preview…" : "This interactive visualization is incomplete.")} />
      : <ReactMarkdown key={i} remarkPlugins={MD_PLUGINS} components={mdComponents} urlTransform={replyUrlTransform}>{part.source}</ReactMarkdown>);
  return mapped;
}
