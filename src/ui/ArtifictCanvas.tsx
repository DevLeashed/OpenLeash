import { useState } from "react";
import ReactMarkdown from "react-markdown";
import { Code2, Copy } from "lucide-react";
import type { ArtifictAnnotation, ArtifictFormat } from "./artifictApi";
import { ArtifictPinLayer, type PinPoint } from "./ArtifictPinLayer";
import { ArtifictRecord, parseCollaborationRecord } from "./ArtifictRecord";
import { ImageExplanation, parseImageSpec } from "./ImageExplanation";
import { ReplyImage } from "./ReplyImage";
import { svgImageUrl } from "./hostTheme";
import { Button } from "./primitives";
import { MD_PLUGINS, copyText, mdComponents } from "./mdx";
import type { VisualizationFeedbackDraft } from "./visualizationFeedback";

/** Formats the host draws itself. `html` is the exception: it runs in its own
 *  isolated frame and keeps that frame's own controls. */
export type ArtifictReadableFormat = Exclude<ArtifictFormat, "html">;

/** The preview's width, in the sizes a design workspace frames a canvas with. */
export type ArtifictViewport = "full" | "tablet" | "phone";
const VIEWPORTS: [ArtifictViewport, string][] = [["full", "Full width"], ["tablet", "Tablet"], ["phone", "Phone"]];

/** Short monospace labels for the canvas bar. Markdown is abbreviated so every
 *  chip is the same width at the same font size. */
export const ARTIFICT_BADGE: Record<ArtifictFormat, string> = {
  html: "HTML", svg: "SVG", json: "JSON", markdown: "MD", text: "TEXT", image: "IMAGE",
};

/** The chrome every host-readable preview wears: one rounded, bordered frame
 *  with a compact bar and, under it, a stage. A device width narrows the stage
 *  rather than the frame, so the artifact keeps its own page width and is looked
 *  at the way it will be seen. A format that already is a frame — the isolated
 *  HTML runtime — is not wrapped in another one. */
export function ArtifictCanvas({ badge, tabs, actions, viewport = "full", bleed = false, scroll = false, label, children }: {
  badge: string;
  /** Left of the bar, beside the kind: the artifact and its source. */
  tabs?: React.ReactNode;
  /** Right of the bar. */
  actions?: React.ReactNode;
  viewport?: ArtifictViewport;
  /** Body runs edge to edge (a picture or diagram) instead of on the padded canvas. */
  bleed?: boolean;
  /** Body is capped to a viewport-relative height and scrolls, so one long
   *  document cannot push the rest of the surface off screen. */
  scroll?: boolean;
  label?: string;
  children: React.ReactNode;
}) {
  const body = ["artifict-canvas-body", bleed ? "bleed" : "", scroll ? "scroll" : ""].filter(Boolean).join(" ");
  return <section className="artifict-canvas" aria-label={label}>
    <div className="artifict-canvas-bar">
      <span className="artifict-badge">{badge}</span>
      {tabs}
      <span className="artifict-canvas-spacer" />
      {actions}
    </div>
    <div className="artifict-canvas-stage" data-viewport={viewport}>
      <div className={body}>{children}</div>
    </div>
  </section>;
}

/** Pretty-print valid JSON and leave anything else as written: a generic JSON
 *  artifict is often a fragment mid-edit, and reformatting must not hide it. */
function prettyJson(content: string): string {
  try { return JSON.stringify(JSON.parse(content), null, 2); } catch { return content; }
}

/** An `image` artifict is either a bare source (https, an inline raster `data:`
 *  URL or a path on this machine) or the bounded JSON image-explanation object
 *  that carries an optional before/after pair and numbered marks. The second
 *  form gets the explorer; the first is simply a picture. */
function ImageBody({ content, label, onFeedbackDraft }: { content: string; label?: string; onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void }) {
  const spec = parseImageSpec(content);
  if (spec) return <ImageExplanation source={content} onFeedbackDraft={onFeedbackDraft} bare />;
  return <div className="artifict-image"><ReplyImage src={content.trim()} alt={label ?? "Artifict image"} /></div>;
}

/** One host-readable preview: the shared frame, the artifact or its source, and
 *  the controls a viewer needs. HTML never reaches here. */
export function ArtifictOverview({ format, content, label, onFeedbackDraft, commentMode, annotations = [], activeId, onSelectAnnotation, onPoint }: {
  format: ArtifictReadableFormat;
  content: string;
  label?: string;
  onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void;
  commentMode?: boolean;
  annotations?: ArtifictAnnotation[];
  activeId?: string | null;
  onSelectAnnotation?: (annotation: ArtifictAnnotation) => void;
  onPoint?: (point: PinPoint) => void;
}) {
  const [showSource, setShowSource] = useState(false);
  const [viewport, setViewport] = useState<ArtifictViewport>("full");
  // A collaboration record is JSON, but it is a structured document rather than
  // a code dump, so it gets the explorer instead of the pretty-printed source.
  const record = format === "json" ? parseCollaborationRecord(content) : null;
  const scroll = format === "markdown" || format === "text" || (format === "json" && !record);
  const body = format === "markdown" ? <div className="md artifict-prose"><ReactMarkdown remarkPlugins={MD_PLUGINS} components={mdComponents}>{content}</ReactMarkdown></div>
    : format === "text" ? <div className="artifict-text">{content}</div>
    // An SVG data URL in an `<img>` is inert: no scripts, no external fetches.
    : format === "svg" ? <img className="artifict-svg" alt={label ?? "SVG artifict preview"} src={svgImageUrl(content)} />
    : format === "image" ? <ImageBody content={content} label={label} onFeedbackDraft={onFeedbackDraft} />
    : record ? <ArtifictRecord record={record} onDraft={onFeedbackDraft} bare />
    : <pre className="artifict-code"><code>{prettyJson(content)}</code></pre>;
  return <ArtifictCanvas
    badge={ARTIFICT_BADGE[format]}
    label={label}
    viewport={viewport}
    bleed={format === "svg" && !showSource}
    scroll={!showSource && scroll}
    tabs={<div className="artifict-tabs" role="group" aria-label="Artifact view">
      <Button variant={showSource ? "ghost" : "primary"} className="artifict-tab" aria-pressed={!showSource} onClick={() => setShowSource(false)}>Preview</Button>
      <Button variant={showSource ? "primary" : "ghost"} className="artifict-tab" aria-pressed={showSource} onClick={() => setShowSource(true)}><Code2 size={12} />Source</Button>
    </div>}
    actions={<>
      <select aria-label="Preview width" className="artifict-viewport" value={viewport} onChange={(event) => setViewport(event.currentTarget.value as ArtifictViewport)}>
        {VIEWPORTS.map(([value, text]) => <option key={value} value={value}>{text}</option>)}
      </select>
      <Button variant="ghost" className="artifict-action" onClick={() => copyText(content, "Artifict source copied")}><Copy size={13} />Copy</Button>
    </>}>
    {showSource
      ? <pre className="artifict-source" aria-label={`${label ?? "Artifict"} source`}><code>{content}</code></pre>
      : <ArtifictPinLayer
        label={label ?? "Artifict preview"}
        annotations={annotations}
        activeId={activeId}
        onSelect={onSelectAnnotation}
        onPoint={commentMode ? onPoint : undefined}
        commentMode={commentMode}
      >{body}</ArtifictPinLayer>}
  </ArtifictCanvas>;
}
