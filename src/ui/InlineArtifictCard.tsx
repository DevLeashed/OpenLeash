import { useEffect, useState } from "react";
import { Layers, Send } from "lucide-react";
import { artifictApi, artifictContentError, type ArtifictFeedback, type ArtifictFormat } from "./artifictApi";
import { useStore } from "../store";
import { ArtifictContentPreview } from "./ArtifictWorkspace";
import { Button, TextArea } from "./primitives";
import type { Item } from "../api";

const panel = { border: "1px solid var(--line)", background: "var(--panel)", borderRadius: 9, padding: 10 } as const;

export interface ArtifictWorkspaceSelection {
  artifictId: string;
  versionId: string;
}

type ArtifictCardData = {
  title: string;
  kind: ArtifictFormat;
  persistence: "session" | "project";
  content: string;
  artifictId?: string;
  versionId?: string;
};

const artifictTools = new Set(["artifict_preview", "artifict_create", "artifict_revise"]);
const formats = new Set<ArtifictFormat>(["html", "svg", "json", "markdown", "text", "image"]);

/** Artifict-producing calls get their own transcript row while running. Their
 *  JSON arguments belong to the tool protocol, not to a visible code dump. */
export function isArtifictTool(item: Item): boolean {
  return item.kind === "tool" && artifictTools.has(String(item.data?.name ?? ""));
}

export function hasArtifictCardSlot(item: Item): boolean {
  if (item.kind === "artifict") return true;
  if (!isArtifictTool(item)) return false;
  if (item.data?.status === "running") return true;
  if (item.data?.status !== "ok" || !item.data?.artifict_card || typeof item.data.artifict_card !== "object") return false;
  const content = typeof item.data.artifict_card.content === "string" ? item.data.artifict_card.content : item.data.input?.content;
  return typeof content === "string" && !!content.trim() && !artifictContentError(content);
}

function completeCard(item: Item): ArtifictCardData | null {
  const data = item.data ?? {};
  let meta: Record<string, unknown>;
  let content: unknown;
  if (isArtifictTool(item) && data.status === "ok" && data.artifict_card && typeof data.artifict_card === "object") {
    // Tool input is attached once the provider has finished assembling the call.
    // Only a completed, successful tool result is allowed to reveal its source.
    meta = data.artifict_card as Record<string, unknown>;
    content = typeof meta.content === "string" ? meta.content : data.input?.content;
  } else return null;

  const inputTitle = typeof data.input?.title === "string" ? data.input.title.trim() : "";
  const title = typeof meta.title === "string" && meta.title.trim() ? meta.title.trim() : inputTitle || item.text.trim();
  const kind = meta.kind;
  const persistence = meta.persistence;
  if (!title || typeof kind !== "string" || !formats.has(kind as ArtifictFormat)
    || (persistence !== "session" && persistence !== "project")
    || typeof content !== "string" || !content.trim() || artifictContentError(content)) return null;

  const artifictId = typeof meta.artifict_id === "string" && meta.artifict_id.trim() ? meta.artifict_id : undefined;
  const versionId = typeof meta.version_id === "string" && meta.version_id.trim() ? meta.version_id : undefined;
  if (persistence === "project" && (!artifictId || !versionId)) return null;
  return { title, kind: kind as ArtifictFormat, persistence, content, artifictId, versionId };
}

function buildingTitle(item: Item): string {
  const input = item.data?.input;
  const title = typeof input?.title === "string" ? input.title.trim() : "";
  return title || (item.data?.name === "artifict_preview" ? "a visual" : "an artifict");
}

/** The inline round-trip for a saved artifict: ask, send, and see the reply,
 *  without leaving the conversation. It shares the workbench's one handoff —
 *  `artifict_feedback_submit`, whose backend delivery is what starts the agent
 *  turn — so there is exactly one path by which a request reaches the agent. */
function InlineArtifictAsk({ taskId, card }: { taskId: string; card: ArtifictCardData }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [open, setOpen] = useState(false);
  // The artifict's own thread, kept live by the same event the workbench uses.
  const published = useStore((s) => s.artificts[taskId]?.[card.artifictId!]);
  const [feedback, setFeedback] = useState<ArtifictFeedback[]>([]);
  useEffect(() => {
    let alive = true;
    artifictApi.feedbackList(taskId, card.artifictId).then((rows) => { if (alive) setFeedback(rows.filter((f) => f.artifict_id === card.artifictId)); }).catch(() => {});
    return () => { alive = false; };
  }, [taskId, card.artifictId, published]);
  const send = async () => {
    if (busy || !text.trim()) return;
    setBusy(true); setError(""); setNotice("");
    try {
      const detail = await artifictApi.submitFeedback(taskId, card.artifictId!, { version_id: card.versionId!, feedback: text.trim(), annotation_ids: [], interactive_state: null });
      setFeedback(detail.feedback);
      setText(""); setNotice("Sent · the agent is picking this up");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const pending = feedback.filter((f) => f.status === "submitted");
  const latest = [...feedback].reverse().find((f) => f.response);
  if (!open) return <div style={{ display: "flex", alignItems: "center", gap: 8, paddingTop: 8 }}>
    <Button variant="ghost" style={{ height: 26, padding: "0 9px", fontSize: 11 }} onClick={() => setOpen(true)}>Ask for a change…</Button>
    {pending.length > 0 && <span style={{ color: "var(--accent)", fontSize: 11 }}>{pending.length} sent · awaiting agent</span>}
    {!pending.length && latest?.response && <span style={{ color: "var(--st-done)", fontSize: 11 }}>Agent {latest.decision === "needs_clarification" ? "asked about this" : "responded"}</span>}
    {error && <span role="alert" style={{ color: "var(--red)", fontSize: 11 }}>{error}</span>}
  </div>;
  return <div style={{ paddingTop: 8 }}>
    <TextArea aria-label={`Request about ${card.title}`} rows={3} value={text} onChange={(e) => setText(e.currentTarget.value)} placeholder="What should the agent change about this? Sending starts the agent on this exact version." style={{ width: "100%" }} />
    {(latest?.response || pending.length > 0) && <div style={{ ...panel, marginTop: 6, padding: 8 }}>
      {pending.length > 0 && <div style={{ color: "var(--accent)", fontSize: 11 }}>Sent · the agent is picking this up</div>}
      {latest?.response && <><div style={{ color: latest.decision === "needs_clarification" ? "var(--st-pause)" : "var(--st-done)", fontSize: 11 }}>Agent · {latest.decision === "needs_clarification" ? "needs clarification" : "addressed"}</div><div style={{ fontSize: 12.5, whiteSpace: "pre-wrap" }}>{latest.response}</div></>}
    </div>}
    {error && <div role="alert" style={{ color: "var(--red)", fontSize: 11, marginTop: 4 }}>{error}</div>}
    {notice && <div role="status" style={{ color: "var(--st-done)", fontSize: 11, marginTop: 4 }}>{notice}</div>}
    <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, marginTop: 6 }}>
      <Button variant="ghost" style={{ height: 26, padding: "0 9px", fontSize: 11 }} onClick={() => { setOpen(false); setText(""); setError(""); }}>Close</Button>
      <Button variant="primary" style={{ height: 26, padding: "0 9px", fontSize: 11 }} disabled={busy || !text.trim()} onClick={() => void send()}><Send size={12} />Send</Button>
    </div>
  </div>;
}

/** One-off and saved artificts stay in the transcript at the tool's own
 *  position. A saved artifict is interactive here too — you can ask for a change
 *  without leaving the conversation — while the project workbench stays the
 *  place for versions, comments and comparison. `taskId` enables that inline
 *  round-trip; without it the card is a static preview. */
export function InlineArtifictCard({ item, taskId, onOpenArtifict }: {
  item: Item;
  taskId?: string;
  onOpenArtifict?: (selection: ArtifictWorkspaceSelection) => void;
}) {
  // While this chat is on its Artificts studio, a saved artifict is already on
  // screen beside the transcript: the card collapses to one row that points at
  // it, so the same design is not drawn twice.
  const studioOpen = useStore((s) => !!taskId && s.view === "session" && s.task === taskId && (s.tasks[taskId]?.artifict_open ?? false));
  if (isArtifictTool(item) && item.data?.status === "running") {
    const title = buildingTitle(item);
    const hasDraft = typeof item.data?.preview_draft === "string" && item.data.preview_draft.length > 0;
    return <section aria-label={`Building ${title}`} aria-live="polite" style={{ border: "1px solid var(--line)", borderRadius: 14, margin: "12px 0", padding: 12, background: "var(--panel)" }}>
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
  if (saved && studioOpen) return <section className="artifict-inline collapsed" aria-label={`Artifict: ${card.title}`}>
    <Layers size={13} aria-hidden="true" />
    <strong className="artifict-inline-title">{card.title}</strong>
    <span className="artifict-inline-meta">{item.data?.name === "artifict_revise" ? "Revised" : "Saved"} · in the studio</span>
    {onOpenArtifict && <Button variant="ghost" className="artifict-action" aria-label={`Show ${card.title} in the studio`} onClick={() => onOpenArtifict({ artifictId: card.artifictId!, versionId: version })}>Show</Button>}
  </section>;
  // The frame belongs to the preview below (the shared canvas, or the isolated
  // HTML runtime); this row names the artifct above it instead of boxing the
  // whole thing in a second border.
  return <section className="artifict-inline" aria-label={`Artifict: ${card.title}`}>
    <header className="artifict-inline-bar">
      <strong className="artifict-inline-title">{card.title}</strong>
      <span className="artifict-inline-meta">{saved ? `Saved to project · version ${version.slice(0, 8)}` : "One-off · in this chat"}</span>
      {saved && onOpenArtifict && <Button variant="default" className="artifict-action artifict-open-studio" aria-label={`Open ${card.title} in Artificts`} title="Comment on it, tweak it and compare versions in the studio" onClick={() => onOpenArtifict({ artifictId: card.artifictId!, versionId: version })}><Layers size={12} aria-hidden="true" />Open in Artificts</Button>}
    </header>
    {/* The same renderer the workbench uses, so a collaboration record is an
        explorer here too instead of a wall of raw JSON. */}
    <ArtifictContentPreview format={card.kind} content={card.content} label={card.title} autoStart />
    {saved && taskId && <InlineArtifictAsk taskId={taskId} card={card} />}
  </section>;
}

