import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw, Code2, Square, Copy, Maximize2, Minimize2 } from "lucide-react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { copyText, mdComponents, remarkOl } from "./mdx";
import { Button, useEscapeClose } from "./primitives";
import { listenForVisualizationDraft, type VisualizationFeedbackDraft } from "./visualizationFeedback";

export const MAX_VIZ_BYTES = 240 * 1024; // leave space for the document wrapper
export const MAX_VIZ_COUNT = 8;
const toolbarButtonStyle = { height: 27, padding: "0 9px", borderRadius: 7, fontSize: 11.5, gap: 6 } as const;
export type VizPart = { kind: "markdown"; source: string } | { kind: "visualization"; source: string } | { kind: "placeholder"; source: string; message?: string };
/** Parse fences before rendering: Markdown accepts unclosed fences while streaming.
 * Only exact, top-level opt-in fences qualify; all other Markdown stays inert. */
export function visualizationParts(text: string): VizPart[] {
  const lines = text.split(/\r?\n/); const out: VizPart[] = []; let plain: string[] = []; let count = 0;
  for (let i = 0; i < lines.length; i++) {
    const opening = /^( {0,3})(`{3,}|~{3,})(.*)$/.exec(lines[i] ?? "");
    if (!opening) { plain.push(lines[i] ?? ""); continue; }
    const marker = opening[2]!; let end = i + 1;
    const closing = new RegExp(`^ {0,3}${marker[0]}{${marker.length},}\\s*$`);
    while (end < lines.length && !closing.test(lines[end] ?? "")) end++;
    const source = lines.slice(i + 1, end).join("\n");
    if (opening[3] !== "openleash-viz") plain.push(...lines.slice(i, Math.min(end + 1, lines.length)));
    else {
      if (plain.length) out.push({ kind: "markdown", source: plain.join("\n") });
      plain = [];
      if (end === lines.length) out.push({ kind: "placeholder", source });
      else if (!source.trim()) out.push({ kind: "placeholder", source, message: "This interactive visualization is empty." });
      else if (count >= MAX_VIZ_COUNT) out.push({ kind: "placeholder", source, message: `Only ${MAX_VIZ_COUNT} visualizations per reply can run.` });
      else if (new TextEncoder().encode(source).length > MAX_VIZ_BYTES) out.push({ kind: "placeholder", source, message: "This interactive visualization is too large to run." });
      else { out.push({ kind: "visualization", source }); count++; }
    }
    i = end;
  }
  if (plain.length) out.push({ kind: "markdown", source: plain.join("\n") });
  return out;
}

function VisualizationPlaceholder({ source, message }: { source: string; message: string }) {
  const [showSource, setShowSource] = useState(false);
  return <section aria-label="Interactive visualization unavailable" style={{ border: "1px solid var(--line)", borderRadius: 10, margin: "12px 0", padding: 10 }}>
    <p role="status" aria-live="polite" style={{ margin: "0 0 8px", color: "var(--mut)" }}>{message}</p>
    <Button variant="ghost" className="viz-toolbar-button" aria-expanded={showSource} onClick={() => setShowSource((value) => !value)}><Code2 size={13} />{showSource ? "Hide source" : "Source"}</Button>
    {showSource && <pre style={{ padding: 12, maxHeight: 320, overflow: "auto", whiteSpace: "pre-wrap" }}><code>{source}</code></pre>}
  </section>;
}
export function captureTheme(): Record<string, string> {
  const s = getComputedStyle(document.documentElement);
  const get = (name: string, fallback: string) => s.getPropertyValue(`--${name}`).trim() || fallback;
  return { bg: get("bg", "#111"), fg: get("fg", "#eee"), card: get("panel", "#222"), muted: get("mut", "#999"), accent: get("accent", "#a78bfa"), primary: get("accent", "#a78bfa"), border: get("line", "#444"),
    ...Object.fromEntries(["#a78bfa", "#60a5fa", "#34d399", "#fbbf24", "#f472b6", "#fb923c"].map((c, i) => [`chart${i + 1}`, get(`chart${i + 1}`, c)])),
    "font-sans": s.fontFamily || "system-ui, sans-serif", "font-mono": "ui-monospace, monospace", radius: "10px" };
}
/** CSS values are host-owned, but escape structural characters defensively. The
 * wrapper precedes any supplied head/body so tokens exist when scripts run. */
export function buildVisualizationDocument(source: string, theme: Record<string, string>): string {
  const css = Object.entries(theme).filter(([k]) => /^[a-z0-9-]+$/.test(k)).map(([k, v]) => `--${k}:${v.replace(/[<>;{}\\]/g, "")};`).join("");
  const feedbackBridge = `<script>(()=>{const send=(draft)=>{try{parent.postMessage({type:"openleash:feedback-draft",version:1,...draft},"*")}catch{}};window.openleashFeedbackDraft=send;document.addEventListener("click",e=>{const t=e.target;const el=t instanceof Element?t.closest("[data-openleash-target]"):null;if(!el)return;const label=(el.getAttribute("data-openleash-target")||"").slice(0,300);if(label)send({target:{selector:"[data-openleash-target]",label}})},true)})();</script>`;
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>:root{${css}}body{margin:0;padding:16px;background:var(--bg);color:var(--fg);font-family:var(--font-sans)}*{box-sizing:border-box}</style>${feedbackBridge}</head><body>${source}</body></html>`;
}
export function Visualization({ source, autoStart = false, label, onFeedbackDraft }: { source: string; autoStart?: boolean; label?: string; onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void }) {
  const title = label?.trim() || "Interactive visualization";
  const frame = useRef<HTMLIFrameElement>(null);
  const viewer = useRef<HTMLDivElement>(null);
  const returnFocus = useRef<HTMLButtonElement | null>(null);
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
      const stops = [...viewer.current.querySelectorAll<HTMLElement>("button:not([disabled]), iframe")];
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
  return <section aria-label={title} style={{ border: "1px solid var(--line)", borderRadius: 10, margin: "12px 0", overflow: "hidden" }}>
    {expanded && <div aria-hidden="true" onClick={collapse} style={{ position: "fixed", inset: 0, zIndex: 959, background: "rgba(0,0,0,.65)" }} />}
    {/* Change only host layout: moving the iframe to a portal would reload its document and lose interactive state. */}
    <div ref={viewer} className={expanded ? "viz-expanded-viewer" : undefined} role={expanded ? "dialog" : undefined} aria-modal={expanded ? true : undefined} aria-label={expanded ? `${title} preview` : undefined} style={expanded ? { position: "fixed", inset: 16, zIndex: 960, display: "flex", flexDirection: "column", background: "var(--bg)", border: "1px solid var(--line)", borderRadius: 10, overflow: "hidden", boxShadow: "0 24px 80px rgba(0,0,0,.5)" } : undefined}>
    <div role="toolbar" aria-label={`${title} controls`} style={{ display: "flex", gap: 6, flexWrap: "wrap", padding: "9px 12px", alignItems: "center", flexShrink: 0, borderBottom: "1px solid var(--line)", background: "var(--panel)" }}>
      <strong title={title} style={{ flex: 1, minWidth: 160, fontSize: 13 }}>{title}</strong>
      {(id || expanded) && <Button variant="ghost" style={toolbarButtonStyle} aria-expanded={expanded} aria-haspopup="dialog" onClick={(event) => { returnFocus.current = event.currentTarget; setExpanded(value => !value); }}>{expanded ? <Minimize2 size={13} /> : <Maximize2 size={13} />}{expanded ? "Collapse preview" : "Expand preview"}</Button>}
      {!busy && !id && !error && <Button variant="primary" style={toolbarButtonStyle} onClick={() => void publish()}><RefreshCw size={13} />{autoStart ? "Restart preview" : "Preview"}</Button>}
      <Button variant="ghost" style={toolbarButtonStyle} aria-expanded={showSource} onClick={() => setShowSource(!showSource)}><Code2 size={13} />{showSource ? "Hide source" : "Source"}</Button>
      <Button variant="ghost" style={toolbarButtonStyle} onClick={() => copyText(source, "Visualization source copied")}><Copy size={13} />Copy source</Button>
      {(busy || id) && <Button variant="ghost" style={{ ...toolbarButtonStyle, color: "var(--red)" }} onClick={stop}><Square size={12} />Stop</Button>}
    </div>
    {busy && <p role="status" aria-live="polite" style={{ padding: "0 10px", color: "var(--mut)" }}>Starting interactive preview…</p>}
    {error && <p role="alert" style={{ padding: 10 }}>{error} <Button variant="ghost" style={toolbarButtonStyle} onClick={() => autoStart ? setAttempt(value => value + 1) : void publish()}><RefreshCw size={13} />Retry preview</Button></p>}
    {id && <iframe ref={frame} key={id} title={`${title} preview`} sandbox="allow-scripts" referrerPolicy="no-referrer" src={convertFileSrc(id, "openleash-viz")} style={{ display: "block", width: "100%", height: expanded ? "100%" : 420, flex: expanded ? 1 : undefined, minHeight: expanded ? 0 : undefined, border: 0, background: "var(--bg)" }} />}
    {showSource && <pre style={{ padding: 12, maxHeight: expanded ? "30vh" : 320, flexShrink: 0, overflow: "auto", whiteSpace: "pre-wrap" }}><code>{source}</code></pre>}
    </div>
  </section>;
}
export function AssistantMarkdown({ text, streaming = false }: { text: string; streaming?: boolean }) {
  const mapped = visualizationParts(text).map((part, i) => part.kind === "visualization"
    ? <Visualization key={`${i}:${part.source}`} source={part.source} autoStart />
    : part.kind === "placeholder"
      ? <VisualizationPlaceholder key={i} source={part.source} message={part.message ?? (streaming ? "Working on the interactive preview…" : "This interactive visualization is incomplete.")} />
      : <ReactMarkdown key={i} remarkPlugins={[remarkGfm, remarkOl]} components={mdComponents}>{part.source}</ReactMarkdown>);
  return mapped;
}
