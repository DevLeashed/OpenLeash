import { artifactContentError, type ArtifactFormat } from "./artifactApi";
import { ArtifactContentPreview } from "./ArtifactWorkspace";
import { Button } from "./primitives";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { mdComponents, remarkOl } from "./mdx";
import type { Item } from "../api";

const panel = { border: "1px solid var(--line)", background: "var(--panel)", borderRadius: 9, padding: 10 } as const;

export interface ArtifactWorkspaceSelection {
  artifactId: string;
  versionId: string;
}

type ArtifactCardData = {
  title: string;
  kind: ArtifactFormat;
  persistence: "session" | "project";
  content: string;
  artifactId?: string;
  versionId?: string;
};

const artifactTools = new Set(["artifact_preview", "artifact_create", "artifact_revise"]);
const formats = new Set<ArtifactFormat>(["html", "svg", "json", "markdown"]);

/** Artifact-producing calls get their own transcript row while running. Their
 *  JSON arguments belong to the tool protocol, not to a visible code dump. */
export function isArtifactTool(item: Item): boolean {
  return item.kind === "tool" && artifactTools.has(String(item.data?.name ?? ""));
}

export function hasArtifactCardSlot(item: Item): boolean {
  if (item.kind === "artifact") return true;
  if (!isArtifactTool(item)) return false;
  if (item.data?.status === "running") return true;
  if (item.data?.status !== "ok" || !item.data?.artifact_card || typeof item.data.artifact_card !== "object") return false;
  const content = typeof item.data.artifact_card.content === "string" ? item.data.artifact_card.content : item.data.input?.content;
  return typeof content === "string" && !!content.trim() && !artifactContentError(content);
}

function completeCard(item: Item): ArtifactCardData | null {
  const data = item.data ?? {};
  let meta: Record<string, unknown>;
  let content: unknown;
  if (isArtifactTool(item) && data.status === "ok" && data.artifact_card && typeof data.artifact_card === "object") {
    // Tool input is attached once the provider has finished assembling the call.
    // Only a completed, successful tool result is allowed to reveal its source.
    meta = data.artifact_card as Record<string, unknown>;
    content = typeof meta.content === "string" ? meta.content : data.input?.content;
  } else return null;

  const inputTitle = typeof data.input?.title === "string" ? data.input.title.trim() : "";
  const title = typeof meta.title === "string" && meta.title.trim() ? meta.title.trim() : inputTitle || item.text.trim();
  const kind = meta.kind;
  const persistence = meta.persistence;
  if (!title || typeof kind !== "string" || !formats.has(kind as ArtifactFormat)
    || (persistence !== "session" && persistence !== "project")
    || typeof content !== "string" || !content.trim() || artifactContentError(content)) return null;

  const artifactId = typeof meta.artifact_id === "string" && meta.artifact_id.trim() ? meta.artifact_id : undefined;
  const versionId = typeof meta.version_id === "string" && meta.version_id.trim() ? meta.version_id : undefined;
  if (persistence === "project" && (!artifactId || !versionId)) return null;
  return { title, kind: kind as ArtifactFormat, persistence, content, artifactId, versionId };
}

function buildingTitle(item: Item): string {
  const input = item.data?.input;
  const title = typeof input?.title === "string" ? input.title.trim() : "";
  return title || (item.data?.name === "artifact_preview" ? "a visual" : "an artifact");
}

/** One-off and saved artifacts stay in the transcript at the tool's own
 *  position; saved versions can additionally be opened in the project workbench. */
export function InlineArtifactCard({ item, onOpenArtifact }: {
  item: Item;
  onOpenArtifact?: (selection: ArtifactWorkspaceSelection) => void;
}) {
  if (isArtifactTool(item) && item.data?.status === "running") {
    const title = buildingTitle(item);
    const hasDraft = typeof item.data?.preview_draft === "string" && item.data.preview_draft.length > 0;
    return <section aria-label={`Building ${title}`} aria-live="polite" style={{ border: "1px solid var(--line)", borderRadius: 10, margin: "12px 0", padding: 12, background: "var(--panel)" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 24 }}>
        <span aria-hidden="true" className="ultraspark" />
        <strong style={{ flex: 1, minWidth: 0 }}>Building {title}…</strong>
        <span style={{ color: "var(--mut3)", fontSize: 10.5 }}>PREVIEW</span>
      </div>
      <div role="status" className={hasDraft ? "shimmer" : undefined} style={{ color: "var(--mut)", fontSize: 12, marginTop: 6 }}>
        {hasDraft ? "Composing the preview…" : "Preparing the preview…"}
      </div>
    </section>;
  }

  const card = completeCard(item);
  if (!card) return null;
  const saved = card.persistence === "project";
  const version = card.versionId!;
  return <section aria-label={`Artifact: ${card.title}`} style={{ border: "1px solid var(--line)", borderRadius: 10, margin: "12px 0", overflow: "hidden", background: "var(--bg)" }}>
    <header style={{ display: "flex", alignItems: "center", flexWrap: "wrap", gap: 8, padding: "9px 12px", borderBottom: "1px solid var(--line)", background: "var(--panel)" }}>
      <strong style={{ flex: 1, minWidth: 120, fontSize: 13 }}>{card.title}</strong>
      <span style={{ color: "var(--mut3)", fontSize: 10.5, fontWeight: 600, letterSpacing: ".04em" }}>
        {saved ? `Saved to project · version ${version.slice(0, 8)}` : "One-off · in this chat"}
      </span>
      {saved && onOpenArtifact && <Button variant="ghost" style={{ height: 26, padding: "0 9px", fontSize: 11 }} aria-label={`Open ${card.title} in Artifacts`} onClick={() => onOpenArtifact({ artifactId: card.artifactId!, versionId: version })}>Open in Artifacts</Button>}
    </header>
    <div style={{ padding: 10 }}>{card.kind === "html"
      ? <ArtifactContentPreview format="html" content={card.content} label={card.title} autoStart />
      : card.kind === "markdown"
        ? <div className="md" style={{ ...panel, maxHeight: 420, overflow: "auto" }}><ReactMarkdown remarkPlugins={[remarkGfm, remarkOl]} components={mdComponents}>{card.content}</ReactMarkdown></div>
        : card.kind === "svg"
          ? <div style={{ ...panel, textAlign: "center" }}><img alt="SVG artifact preview" src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(card.content)}`} style={{ maxWidth: "100%", maxHeight: 370 }} /></div>
          : <pre aria-label="JSON artifact preview" style={{ ...panel, maxHeight: 420, overflow: "auto", whiteSpace: "pre-wrap" }}><code>{card.content}</code></pre>}
    </div>
  </section>;
}

