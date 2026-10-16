import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Layers, Maximize2, MessageSquarePlus, Minimize2, RefreshCw, Send, Trash2 } from "lucide-react";
import { ArtifictAnnotation, ArtifictDetail, ArtifictFeedback, ArtifictFormat, ArtifictSummary, artifictApi, parseArtifictState } from "./artifictApi";
import { ArtifictOverview, ARTIFICT_BADGE } from "./ArtifictCanvas";
import { ArtifictThumb } from "./ArtifictThumb";
import { ArtifictWipNotice } from "./ArtifictWipNotice";
import { type PinPoint } from "./ArtifictPinLayer";
import { Visualization } from "./Visualization";
import { Button, Input, TextArea } from "./primitives";
import { useStore } from "../store";
import type { VisualizationFeedbackDraft } from "./visualizationFeedback";

const note = { color: "var(--mut3)", fontSize: 11, lineHeight: 1.45 } as const;
const panel = { border: "1px solid var(--line)", background: "var(--panel)", borderRadius: 9, padding: 10 } as const;
const chip = { flex: "none", border: "1px solid var(--line)", borderRadius: 999, padding: "3px 8px", color: "var(--mut2)", fontSize: 10, fontWeight: 650, letterSpacing: ".06em" } as const;
const fieldLabel = { ...note, display: "block", marginTop: 6 } as const;
type ArtifictDraft = {
  /** The point a new comment is being pinned to, if any. */
  point: PinPoint | null;
  selector: string; target: string; commentText: string; annotationIds: string[];
  stateText: string; feedbackText: string; feedbackDraft: VisualizationFeedbackDraft | null;
};
const emptyDraft: ArtifictDraft = { point: null, selector: "", target: "", commentText: "", annotationIds: [], stateText: "", feedbackText: "", feedbackDraft: null };
export type DiffLine = { kind: "same" | "remove" | "add"; text: string };
export function artifictDiff(before: string, after: string): DiffLine[] {
  const a = before.split("\n"), b = after.split("\n"); let prefix = 0, suffix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix++;
  while (suffix < a.length - prefix && suffix < b.length - prefix && a[a.length - 1 - suffix] === b[b.length - 1 - suffix]) suffix++;
  return [...a.slice(0, prefix).map((text) => ({ kind: "same" as const, text })), ...a.slice(prefix, a.length - suffix).map((text) => ({ kind: "remove" as const, text })), ...b.slice(prefix, b.length - suffix).map((text) => ({ kind: "add" as const, text })), ...(suffix ? a.slice(a.length - suffix).map((text) => ({ kind: "same" as const, text })) : [])];
}
function ArtifictPreview({ detail, label, onFeedbackDraft, commentMode, annotations, activeId, onSelectAnnotation, onPoint }: {
  detail: ArtifictDetail; label?: string; onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void;
  commentMode?: boolean; annotations?: ArtifictAnnotation[]; activeId?: string | null;
  onSelectAnnotation?: (annotation: ArtifictAnnotation) => void; onPoint?: (point: PinPoint) => void;
}) {
  const { content } = detail.selected_version;
  return <ArtifictContentPreview format={detail.summary.format} content={content} label={label ?? detail.summary.title} onFeedbackDraft={onFeedbackDraft} commentMode={commentMode} annotations={annotations} activeId={activeId} onSelectAnnotation={onSelectAnnotation} onPoint={onPoint} />;
}

/** The preview surface. An HTML artifict runs in its own isolated frame, which
 *  owns its own coordinates and can only propose a point through the bounded
 *  draft message; every other format is host-readable and is drawn by the
 *  shared canvas inside the pin layer, so a comment can attach to the exact
 *  point the user clicked. */
export function ArtifictContentPreview({ format, content, label, onFeedbackDraft, autoStart = false, commentMode, annotations = [], activeId, onSelectAnnotation, onPoint }: {
  format: ArtifictFormat;
  content: string;
  label?: string;
  onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void;
  autoStart?: boolean;
  commentMode?: boolean;
  annotations?: ArtifictAnnotation[];
  activeId?: string | null;
  onSelectAnnotation?: (annotation: ArtifictAnnotation) => void;
  onPoint?: (point: PinPoint) => void;
}) {
  if (format === "html") return <Visualization source={content} label={label} autoStart={autoStart} onFeedbackDraft={onFeedbackDraft} />;
  return <ArtifictOverview format={format} content={content} label={label} onFeedbackDraft={onFeedbackDraft} commentMode={commentMode} annotations={annotations} activeId={activeId} onSelectAnnotation={onSelectAnnotation} onPoint={onPoint} />;
}

/** What the workspace knows about this record's requests: the ones still out,
 *  and the agent's last word on it. Not a conversation — the replies land in the
 *  chat as ordinary turns, and replaying them here is what made a second chat
 *  window — so this names the state and, when the agent asked something, offers
 *  the answer box. */
function ArtifictFeedbackStatus({ feedback, versions, busy, onAskAgent }: {
  feedback: ArtifictFeedback[];
  versions: ArtifictSummary["versions"];
  busy: boolean;
  onAskAgent?: (text: string) => void;
}) {
  const [reply, setReply] = useState<Record<string, string>>({});
  const outstanding = feedback.filter((entry) => entry.status === "submitted");
  const answered = feedback.filter((entry) => entry.response);
  const latest = answered[answered.length - 1];
  if (!outstanding.length && !latest) return null;
  const versionOf = (entry: ArtifictFeedback) => versions.find((v) => v.id === entry.version_id)?.number ?? "?";
  return <div className="artifict-status">
    {outstanding.map((entry) => <div key={entry.id}>
      <div style={{ ...note, color: "var(--accent)" }}>Sent · v{versionOf(entry)} · the agent is picking this up</div>
      <div style={{ fontSize: 12.5, whiteSpace: "pre-wrap", margin: "2px 0" }}>{entry.text || "(comments and state only)"}</div>
    </div>)}
    {latest && <div style={{ borderLeft: `2px solid ${latest.decision === "needs_clarification" ? "var(--st-pause)" : "var(--st-done)"}`, paddingLeft: 8 }}>
      <div style={note}>Agent · v{versionOf(latest)} · {latest.decision === "needs_clarification" ? "needs clarification" : "addressed"}</div>
      <div style={{ fontSize: 12.5, whiteSpace: "pre-wrap" }}>{latest.response}</div>
      {onAskAgent && latest.decision === "needs_clarification" && <div style={{ marginTop: 6 }}>
        <Input aria-label={`Answer for comment ${latest.id}`} value={reply[latest.id] ?? ""} onChange={(e) => setReply((old) => ({ ...old, [latest.id]: e.currentTarget.value }))} placeholder="Answer the question…" />
        <div style={{ textAlign: "right", marginTop: 4 }}><Button disabled={busy || !(reply[latest.id] ?? "").trim()} onClick={() => { onAskAgent(reply[latest.id]!.trim()); setReply((old) => ({ ...old, [latest.id]: "" })); }}>Answer</Button></div>
      </div>}
    </div>}
  </div>;
}

/** Ask the agent to act on this workspace artifict, staged into the request box.
 *  Staging, not sending: the user reviews and presses Send, which is the one
 *  action that reaches the agent. */
function requestTemplates(artifictId: string, versionId: string, number: number) {
  return {
    architecture: "Create or revise a grounded architecture map for this project as an openleash.collaboration.v1 JSON artifict. Inspect actual code first; include component nodes, dependency reasons, relative code references and explicit limitations. Do not invent dependencies or imply unrun verification.",
    investigate: "Investigate the visual issue discussed here using this exact artifict revision. Use an openleash.collaboration.v1 record with an annotated image/before-after comparison when evidence exists, actual changed-file references, exact verification commands and unresolved limitations. Preserve original images and do not label a mockup as captured output.",
    implement: `Implement artifict ${artifictId}, exact revision ${versionId} (v${number}), in the real project. First inspect its content, constraints and code references, explain any mismatch, then make and verify the changes under existing permissions. Link changed files, verification commands/results and remaining limitations back to this revision.`,
  };
}

/** Project-scoped, persistent artifict editor. Preview frames get no app
 *  bridge. Annotation targets, points and state snapshots are host-entered,
 *  untrusted data; only the explicit feedback submission makes them
 *  agent-readable, and that submission is also what starts the agent turn. */
export function ArtifictWorkspace({ taskId, taskTitle, initialSelection, onExpandedChange }: { taskId: string; taskTitle: string; initialSelection?: { artifictId: string; versionId: string } | null; onExpandedChange?: (expanded: boolean) => void }) {
  const [rows, setRows] = useState<ArtifictSummary[]>([]);
  const [artifictId, setArtifictId] = useState("");
  const [versionId, setVersionId] = useState("");
  const [detail, setDetail] = useState<ArtifictDetail | null>(null);
  const [compareVersion, setCompareVersion] = useState("");
  const [compareDetail, setCompareDetail] = useState<ArtifictDetail | null>(null);
  const [compare, setCompare] = useState(false);
  // The records are a view of their own rather than a rail beside the one you
  // opened: the workspace starts on the record itself, and "All artificts" is the
  // way to the set. Two things would otherwise split the width between them.
  const [gallery, setGallery] = useState(false);
  const [expanded, setExpanded] = useState(false);
  // Expand is a claim on the layout, and the layout belongs to the pane that
  // hosts this surface, so the state is reported out rather than acted on here.
  useEffect(() => { onExpandedChange?.(expanded); }, [expanded, onExpandedChange]);
  const [filter, setFilter] = useState("");
  const [selectionText, setSelectionText] = useState("");
  const [commentMode, setCommentMode] = useState(false);
  const [activeAnnotation, setActiveAnnotation] = useState<string | null>(null);
  // Drafts belong to an exact version, not whichever preview happens to be open.
  // They remain local and never cross the explicit submission boundary.
  const [drafts, setDrafts] = useState<Record<string, ArtifictDraft>>({});
  const draftKey = `${taskId}:${artifictId}:${versionId}`;
  const { point, selector, target, commentText, annotationIds, stateText, feedbackText, feedbackDraft } = drafts[draftKey] ?? emptyDraft;
  const updateDraft = (patch: Partial<ArtifictDraft>) => setDrafts((old) => ({ ...old, [draftKey]: { ...(old[draftKey] ?? emptyDraft), ...patch } }));
  const setSelector = (value: string) => updateDraft({ selector: value });
  const setTarget = (value: string) => updateDraft({ target: value });
  const setCommentText = (value: string) => updateDraft({ commentText: value });
  const setStateText = (value: string) => updateDraft({ stateText: value });
  const setFeedbackText = (value: string) => updateDraft({ feedbackText: value });
  const setFeedbackDraft = (value: VisualizationFeedbackDraft | null) => updateDraft({ feedbackDraft: value });
  const setAnnotationIds = (change: (ids: string[]) => string[]) => setDrafts((old) => {
    const draft = old[draftKey] ?? emptyDraft;
    return { ...old, [draftKey]: { ...draft, annotationIds: change(draft.annotationIds) } };
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const initialArtifictId = initialSelection?.artifictId ?? "";
  const initialVersionId = initialSelection?.versionId ?? "";
  // The backend publishes a new id every time an artifict is written — from the
  // agent's own tools as much as from here — so a revision the agent makes during
  // a turn lands in an open workspace without a manual Refresh.
  const publishedVersion = useStore((s) => s.artificts[taskId]?.[artifictId]);

  const load = useCallback(async (preferId?: string, preferVersion?: string) => {
    const list = await artifictApi.list(taskId);
    setRows(list);
    const id = preferId && list.some((item) => item.id === preferId) ? preferId : artifictId && list.some((item) => item.id === artifictId) ? artifictId : list[0]?.id ?? "";
    setArtifictId(id);
    const summary = list.find((item) => item.id === id);
    if (!summary) { setDetail(null); setVersionId(""); return; }
    const vid = preferVersion && summary.versions.some((item) => item.id === preferVersion) ? preferVersion : id === artifictId && summary.versions.some((item) => item.id === versionId) ? versionId : summary.latest_version_id;
    setVersionId(vid);
    const next = await artifictApi.get(taskId, id, vid);
    setDetail(next);
    const other = next.summary.versions.find((item) => item.id !== vid); setCompareVersion((old) => old !== vid && next.summary.versions.some((item) => item.id === old) ? old : other?.id ?? "");
  }, [taskId, artifictId, versionId]);

  useEffect(() => {
    let alive = true; setBusy(true); setError("");
    artifictApi.list(taskId).then(async (list) => {
      if (!alive) return;
      setRows(list);
      const id = initialArtifictId && list.some((item) => item.id === initialArtifictId) ? initialArtifictId : list[0]?.id ?? ""; setArtifictId(id);
      const summary = list.find((item) => item.id === id);
      if (summary) {
        const requestedVersion = initialArtifictId === id ? initialVersionId : undefined;
        const selectedVersion = requestedVersion && summary.versions.some((item) => item.id === requestedVersion) ? requestedVersion : summary.latest_version_id;
        const next = await artifictApi.get(taskId, summary.id, selectedVersion);
        if (!alive) return;
        setDetail(next); setVersionId(next.selected_version.id);
        setCompareVersion(next.summary.versions.at(-2)?.id ?? "");
      }
    }).catch((e) => { if (alive) setError(String(e)); }).finally(() => { if (alive) setBusy(false); });
    return () => { alive = false; };
  }, [taskId, initialArtifictId, initialVersionId]);
  useEffect(() => {
    let alive = true;
    if (artifictId && versionId) artifictApi.get(taskId, artifictId, versionId).then((next) => {
      if (!alive) return;
      setDetail(next);
    }).catch((e) => { if (alive) setError(String(e)); });
    else setDetail(null);
    return () => { alive = false; };
  }, [taskId, artifictId, versionId, publishedVersion]);
  useEffect(() => {
    let alive = true;
    if (compare && artifictId && compareVersion) artifictApi.get(taskId, artifictId, compareVersion).then((next) => { if (alive) setCompareDetail(next); }).catch((e) => { if (alive) setError(String(e)); });
    else setCompareDetail(null);
    return () => { alive = false; };
  }, [compare, taskId, artifictId, compareVersion]);
  // What the editor is showing, read by the publish handler below. Kept in a ref
  // because that handler must fire on the published version alone: depending on
  // `detail`/`versionId` would re-run it on every version switch, and each run
  // re-reads the whole artifict list.
  const view = useRef<{ artifict: string; version: string; tip: string } | null>(null);
  useEffect(() => {
    view.current = detail ? { artifict: detail.summary.id, version: detail.selected_version.id, tip: detail.summary.latest_version_id } : null;
  }, [detail]);
  // A revision the agent made mid-turn is what the user should see: it is the
  // answer to the request they just sent. Following the tip moves the view only
  // when they were already on the version that was current before this write —
  // a revision they deliberately pinned is theirs to keep looking at.
  useEffect(() => {
    if (!publishedVersion || !artifictId) return;
    const shown = view.current;
    if (shown && shown.artifict === artifictId && shown.version === shown.tip && publishedVersion !== shown.version) setVersionId(publishedVersion);
    let alive = true;
    artifictApi.list(taskId).then((list) => { if (alive) setRows(list); }).catch(() => {});
    return () => { alive = false; };
  }, [taskId, artifictId, publishedVersion]);

  useEffect(() => { setSelectionText(""); setActiveAnnotation(null); }, [artifictId, versionId]);
  const parsedState = useMemo(() => parseArtifictState(stateText), [stateText]);
  const comparedDetail = compareDetail?.summary.id === artifictId && compareDetail.selected_version.id === compareVersion ? compareDetail : null;
  const previewDraft = useCallback((draft: VisualizationFeedbackDraft) => {
    setDrafts((old) => ({ ...old, [draftKey]: { ...(old[draftKey] ?? emptyDraft), feedbackDraft: draft } }));
  }, [draftKey]);
  const selectedDetail = detail?.summary.id === artifictId && detail.selected_version.id === versionId ? detail : null;
  const current = selectedDetail?.selected_version;
  const usePreviewDraft = () => {
    if (!feedbackDraft) return;
    if (feedbackDraft.state) setStateText(JSON.stringify(feedbackDraft.state, null, 2));
    if (feedbackDraft.target?.selector) setSelector(feedbackDraft.target.selector);
    if (feedbackDraft.target?.label) setTarget(feedbackDraft.target.label);
    if (typeof feedbackDraft.target?.x === "number" && typeof feedbackDraft.target?.y === "number") updateDraft({ point: { x: feedbackDraft.target.x, y: feedbackDraft.target.y } });
    setFeedbackDraft(null);
  };
  const annotations: ArtifictAnnotation[] = detail?.annotations.filter((item) => item.version_id === versionId) ?? [];
  const feedbackHistory: ArtifictFeedback[] = detail?.feedback ?? [];
  const diff = current && comparedDetail ? artifictDiff(comparedDetail.selected_version.content, current.content) : [];
  const execute = async (fn: () => Promise<ArtifictDetail | undefined>, success: string, id = artifictId, vid?: string, onSaved?: () => void) => {
    if (busy) return;
    setBusy(true); setError(""); setNotice("");
    try {
      const result = await fn();
      onSaved?.();
      if (result) {
        setDetail(result);
        setRows((old) => old.map((row) => row.id === result.summary.id ? { ...row, ...result.summary, pendingFeedback: result.feedback.filter(f => f.status === "submitted" || f.decision === "needs_clarification").length } : row));
      } else await load(id, vid);
      if (id) setNotice(success);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const pickArtifict = (id: string) => { const row = rows.find((item) => item.id === id); setArtifictId(id); setVersionId(row?.latest_version_id ?? ""); setError(""); setNotice(""); setCompare(false); setGallery(false); };
  /** Pin a comment to a point in the preview. The comment is saved private to
   *  this exact revision; sharing it with the agent is a separate, explicit act. */
  const pinComment = (at: PinPoint) => { updateDraft({ point: at }); setCommentMode(true); };
  const saveComment = () => {
    if (!current || !commentText.trim()) return setError("Write a comment first.");
    if (selector.length > 300 || target.length > 300 || commentText.length > 4000) return setError("Selector and target are limited to 300 characters; comment is limited to 4,000.");
    void execute(() => artifictApi.annotate(taskId, artifictId, {
      version_id: current.id,
      text: commentText.trim(),
      selector: selector.trim() || undefined,
      target: target.trim() || undefined,
      point,
    }), `Comment saved on v${current.number}. Include it in your request when you send it.`, artifictId, current.id, () => setDrafts((old) => {
      const draft = old[draftKey] ?? emptyDraft;
      return { ...old, [draftKey]: { ...draft, commentText: draft.commentText === commentText ? "" : draft.commentText, point: null } };
    }));
  };
  const removeComment = (annotationId: string) => {
    void execute(() => artifictApi.removeAnnotation(taskId, artifictId, annotationId, current?.id), "Comment removed.", artifictId, current?.id, () => setAnnotationIds((ids) => ids.filter((id) => id !== annotationId)));
  };
  const shareFeedback = () => {
    if (!current) return setError("Select an exact artifict version first.");
    if (parsedState.error) return setError(parsedState.error);
    if (!feedbackText.trim() && !annotationIds.length && !parsedState.value) return setError("Write a request, choose a comment, or enter state values.");
    void execute(() => artifictApi.submitFeedback(taskId, artifictId, { version_id: current.id, feedback: feedbackText.trim(), annotation_ids: annotationIds, interactive_state: parsedState.value }), `Sent to the agent for v${current.number}.`, artifictId, current.id, () => setDrafts((old) => {
      const draft = old[draftKey] ?? emptyDraft;
      return { ...old, [draftKey]: { ...draft,
        feedbackText: draft.feedbackText === feedbackText ? "" : draft.feedbackText,
        stateText: draft.stateText === stateText ? "" : draft.stateText,
        annotationIds: draft.annotationIds.filter((id) => !annotationIds.includes(id)),
      } };
    }));
  };
  const templates = current ? requestTemplates(artifictId, current.id, current.number) : null;
  // Read once for the gallery and its own "nothing matches" state.
  const filteredRows = rows.filter((row) => `${row.title} ${row.format} ${row.excerpt ?? ""} ${row.recordStage ?? ""}`.toLowerCase().includes(filter.toLowerCase()));

  return <section aria-label="Artifict workspace" style={{ height: "100%", minHeight: 0, display: "flex", flexDirection: "column", overflow: "hidden" }}>
    <header className="artifict-workspace-bar" style={{ flex: "none", display: "flex", alignItems: "center", flexWrap: "wrap", gap: 6, padding: "8px 10px", borderBottom: "1px solid var(--line)" }}>
      <b style={{ flex: "1 1 160px", minWidth: 0, fontSize: 12.5, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{detail?.summary.title ?? "Artificts"}<span style={note}> · {taskTitle}</span></b>
      {rows.length > 0 && <Button variant="ghost" style={{ height: 26, padding: "0 8px", fontSize: 11.5 }} aria-expanded={gallery} onClick={() => setGallery((value) => !value)}>{gallery ? "Back to artifict" : "All artificts"}</Button>}
      <Button variant="ghost" style={{ height: 26, padding: "0 8px", fontSize: 11.5 }} aria-expanded={expanded} aria-label={expanded ? "Collapse artifict editor" : "Expand artifict editor"} onClick={() => setExpanded((value) => !value)}>{expanded ? <Minimize2 size={13} /> : <Maximize2 size={13} />}</Button>
      <Button variant="ghost" style={{ height: 26, padding: "0 8px", fontSize: 11.5 }} disabled={busy} aria-label="Refresh artificts" onClick={() => void execute(async () => undefined, "Artificts refreshed.")}><RefreshCw size={13} /></Button>
    </header>
    {/* One surface, one conversation. The workspace holds the record; the chat
        beside it is where the talking happens, and nothing here mirrors those
        turns into a second window of their own. */}
    {gallery ? <div className="artifict-gallery">
        <Input aria-label="Filter artificts" value={filter} onChange={e => setFilter(e.currentTarget.value)} placeholder="Find an artifict…" />
        <div className="artifict-cards">{filteredRows.map((row) => <div key={row.id} className="artifict-card" aria-current={row.id === artifictId ? "true" : undefined}>
          <div className="artifict-card-frame">
            {row.thumbnail ? <img className="artifict-thumb-media" alt="" src={row.thumbnail} /> : <ArtifictThumb format={row.format} content={row.preview ?? ""} title={row.title} live={row.id === artifictId} />}
            <span className="artifict-card-kind" aria-hidden="true">{ARTIFICT_BADGE[row.format]}</span>
          </div>
          <span className="artifict-row-title">{row.title}</span>
          <span className="artifict-row-meta">{row.recordStage ? `${row.recordStage} · ` : ""}v{row.version_count}{row.pendingFeedback ? ` · ${row.pendingFeedback} open` : ""}</span>
          {/* A live miniature is an isolated frame, and a frame cannot live inside
              a button — so the card is a plain box with a transparent overlay
              button, which is also what makes the whole card the target. */}
          <button type="button" className="artifict-card-open" aria-label={`Open ${row.title}`} disabled={busy} onClick={() => pickArtifict(row.id)} />
        </div>)}</div>
        {!rows.length && <div className="artifict-empty">
          <Layers size={18} aria-hidden="true" />
          <p style={{ ...note, maxWidth: "26ch" }}>No artificts yet. Ask the agent in the conversation to create one.</p>
        </div>}
        {!!rows.length && !filteredRows.length && <div className="artifict-blank">Nothing here matches “{filter}”.</div>}
      </div> : <main className="artifict-main">
        {busy && <div role="status" style={note}>Loading / saving…</div>}
        {error && <div role="alert" style={{ color: "var(--red)", fontSize: 12 }}>{error}</div>}
        {notice && <div role="status" style={{ color: "var(--st-done)", fontSize: 12 }}>{notice}</div>}
        {!detail && !busy && <div className="artifict-empty">
          <Layers size={20} aria-hidden="true" />
          <p style={{ ...note, maxWidth: "30ch" }}>No artificts yet. Ask the agent in the conversation to create one.</p>
          <p style={{ ...note, maxWidth: "42ch", color: "var(--mut2)" }}>A design, prototype, diagram, picture or decision record opens here as a versioned record you can comment on and send back to the agent.</p>
        </div>}
        {detail && current && <>
          <div className="artifict-version-bar">
            {/* Every host-readable format carries its own type chip in the canvas
                bar below; only the isolated HTML frame has no canvas, so it keeps
                the one here. */}
            {detail.summary.format === "html" && <span style={chip}>{detail.summary.format.toUpperCase()}</span>}
            <select aria-label="Exact artifict version" value={versionId} disabled={busy} onChange={(e) => { setVersionId(e.currentTarget.value); setCompareVersion(detail.summary.versions.find((v) => v.id !== e.currentTarget.value)?.id ?? ""); setError(""); setNotice(""); }} style={{ height: 26, padding: "0 6px", border: "1px solid var(--line)", borderRadius: 7, background: "var(--raise)", color: "var(--fg)", font: "inherit", fontSize: 11.5 }}>
              {detail.summary.versions.map((v) => <option key={v.id} value={v.id}>v{v.number} · {new Date(v.created_at).toLocaleString()}</option>)}
            </select>
            {detail.summary.versions.length > 1 && <Button variant={compare ? "primary" : "ghost"} style={{ height: 26, padding: "0 8px", fontSize: 11.5 }} aria-pressed={compare} onClick={() => setCompare(!compare)}>{compare ? "Hide comparison" : "Compare"}</Button>}
            <div style={{ flex: 1 }} />
            <Button variant={commentMode ? "primary" : "ghost"} style={{ height: 26, padding: "0 8px", fontSize: 11.5 }} aria-pressed={commentMode} onClick={() => { setCommentMode((value) => !value); updateDraft({ point: null }); }}><MessageSquarePlus size={12} />{commentMode ? "Click the preview" : "Comment"}</Button>
          </div>
          {compare && detail.summary.versions.length > 1 && <label style={{ ...panel, ...note, display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>Compare v{current.number} with <select aria-label="Compare against version" value={compareVersion} onChange={(e) => setCompareVersion(e.currentTarget.value)}>{detail.summary.versions.filter((v) => v.id !== versionId).map((v) => <option key={v.id} value={v.id}>v{v.number}</option>)}</select></label>}
          {/* What you are reading is a draft: the notice sits above the content
              it describes, and names the selected version's recorded stage so a
              comparison keeps the two versions' own claims apart. */}
          <ArtifictWipNotice format={detail.summary.format} content={current.content} version={current.number} />
          <div aria-label="Selected artifict preview" onMouseUp={e => { const selection = window.getSelection(); if (selection?.anchorNode && e.currentTarget.contains(selection.anchorNode)) setSelectionText(selection.toString().trim().slice(0, 300)); }} onKeyUp={e => { const selection = window.getSelection(); if (selection?.anchorNode && e.currentTarget.contains(selection.anchorNode)) setSelectionText(selection.toString().trim().slice(0, 300)); }}>
            {compare && comparedDetail
              ? <><div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit,minmax(240px,1fr))", gap: 8 }}>
                <section aria-label={`Compared version ${comparedDetail.selected_version.number}`}><b style={note}>v{comparedDetail.selected_version.number}</b><ArtifictPreview key={`${artifictId}:${compareVersion}`} detail={comparedDetail} label={`v${comparedDetail.selected_version.number}`} /></section>
                <section aria-label={`Selected version ${selectedDetail!.selected_version.number}`}><b style={note}>v{selectedDetail!.selected_version.number}</b><ArtifictPreview key={`${artifictId}:${versionId}`} detail={selectedDetail!} onFeedbackDraft={previewDraft} commentMode={commentMode} annotations={annotations} activeId={activeAnnotation} onSelectAnnotation={(a) => setActiveAnnotation(a.id)} onPoint={pinComment} /></section>
              </div><div aria-label="Version content diff" style={{ ...panel, margin: "8px 0", maxHeight: 180, overflow: "auto", font: "11px var(--mono)", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>{diff.map((line, i) => <div key={`${i}-${line.kind}`} style={{ color: line.kind === "remove" ? "var(--diff-del)" : line.kind === "add" ? "var(--diff-add)" : "var(--mut3)" }}>{line.kind === "remove" ? "− " : line.kind === "add" ? "+ " : "  "}{line.text || " "}</div>)}</div></>
              : <ArtifictPreview key={`${artifictId}:${versionId}`} detail={selectedDetail!} onFeedbackDraft={previewDraft} commentMode={commentMode} annotations={annotations} activeId={activeAnnotation} onSelectAnnotation={(a) => setActiveAnnotation(a.id)} onPoint={pinComment} />}
          </div>
          {selectionText && <div className="artifict-selection">
            <span style={{ ...note, flex: 1, minWidth: 0 }}>Selected: “{selectionText}”</span>
            <Button variant="ghost" style={{ height: 26, padding: "0 8px", fontSize: 11.5 }} onClick={() => { setTarget(selectionText); setCommentText(`Regarding “${selectionText}”: `); setCommentMode(true); setSelectionText(""); }}>Comment on this</Button>
            <Button variant="ghost" style={{ height: 26, padding: "0 8px", fontSize: 11.5 }} onClick={() => { setFeedbackText(`${feedbackText}${feedbackText ? "\n\n" : ""}Target in v${current.number}: “${selectionText}”\n`); setSelectionText(""); }}>Add to request</Button>
          </div>}
          <section className="artifict-comments" aria-label="Comments on this version">
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <b style={{ flex: 1, fontSize: 12 }}>Comments · v{current.number}</b>
              <span style={note}>{annotations.length ? `${annotations.length} on this version` : "none yet"}</span>
            </div>
            {commentMode && <p style={note}>{detail.summary.format === "html" ? "Click a marked target in the preview, or select text above." : "Click anywhere in the preview to pin a comment, or select text above."}</p>}
            {point && <div style={note}>New comment pinned at {point.x}%, {point.y}% <Button variant="ghost" style={{ height: 20, padding: "0 6px", fontSize: 10.5 }} onClick={() => updateDraft({ point: null })}>Clear pin</Button></div>}
            {feedbackDraft && <div role="status" style={{ ...panel, marginTop: 6, padding: 8 }}>
              <b style={{ fontSize: 11 }}>Preview proposal — review before using</b>
              <pre style={{ maxHeight: 90, overflow: "auto", whiteSpace: "pre-wrap", fontSize: 11 }}>{JSON.stringify(feedbackDraft, null, 2)}</pre>
              <Button style={{ height: 24, fontSize: 11 }} onClick={usePreviewDraft}>Use fields</Button> <Button variant="ghost" style={{ height: 24, fontSize: 11 }} onClick={() => setFeedbackDraft(null)}>Dismiss</Button>
            </div>}
            <TextArea aria-label="Comment text" rows={2} value={commentText} onChange={(e) => setCommentText(e.currentTarget.value)} onFocus={() => setCommentMode(true)} placeholder={commentMode ? "What should change here?" : "Add a comment…"} style={{ width: "100%", marginTop: 6 }} />
            <details><summary style={{ cursor: "pointer", color: "var(--mut2)", fontSize: 11.5, padding: "6px 0" }}>Target details (optional)</summary>
              <label style={fieldLabel}>Selector <Input aria-label="Comment selector" value={selector} maxLength={300} onChange={(e) => setSelector(e.currentTarget.value)} placeholder="e.g. [data-control='volume']" /></label>
              <label style={fieldLabel}>Visible target <Input aria-label="Comment target" value={target} maxLength={300} onChange={(e) => setTarget(e.currentTarget.value)} placeholder="Control label or node" /></label>
            </details>
            <div style={{ textAlign: "right", marginTop: 4 }}><Button disabled={busy || !commentText.trim()} onClick={saveComment}>Save comment</Button></div>
            {annotations.map((a, index) => <label key={a.id} style={{ display: "flex", gap: 6, alignItems: "flex-start", padding: "5px 4px", borderTop: "1px solid var(--line)", fontSize: 11.5, background: a.id === activeAnnotation ? "var(--raise)" : undefined }}>
              <input type="checkbox" aria-label={`Include comment: ${a.text}`} checked={annotationIds.includes(a.id)} onChange={(e) => setAnnotationIds((old) => e.currentTarget.checked ? [...old, a.id] : old.filter((id) => id !== a.id))} />
              <span style={{ flex: 1, minWidth: 0 }} onMouseEnter={() => setActiveAnnotation(a.id)}>
                <b style={{ color: "var(--mut2)" }}>{a.point ? `${index + 1} · ${a.point.x}%,${a.point.y}%` : a.target || a.selector || `${index + 1}`}</b>{a.target && a.point ? ` · ${a.target}` : ""} — {a.text}
                {a.shared && <em style={{ color: "var(--st-done)" }}> · sent</em>}
              </span>
              {!a.shared && <button type="button" aria-label={`Remove comment: ${a.text}`} title="Remove this unsent comment" style={{ background: "none", border: 0, color: "var(--mut3)", cursor: "pointer", padding: 2 }} onClick={() => removeComment(a.id)}><Trash2 size={12} /></button>}
            </label>)}
            {/* The record's own request sits with its comments rather than in a
                rail beside them. The chat next to this pane IS the conversation,
                and mirroring those turns here made a second chat window showing
                the same thing twice. */}
            <section className="artifict-request" aria-label="Send to the agent">
              <ArtifictFeedbackStatus feedback={feedbackHistory} versions={detail.summary.versions} busy={busy} onAskAgent={(text) => { setFeedbackText(text); }} />
              <TextArea aria-label="Follow up for agent" rows={3} value={feedbackText} onChange={(e) => setFeedbackText(e.currentTarget.value)} placeholder="What should the agent do about this? e.g. apply this revision to the real project…" style={{ width: "100%" }} />
              {annotationIds.length > 0 && <div style={note}>{annotationIds.length} comment{annotationIds.length === 1 ? "" : "s"} attached</div>}
              {selectedDetail && parsedState.value && <div style={note}>State attached</div>}
              <div style={{ display: "flex", alignItems: "center", gap: 6, marginTop: 6 }}>
                <details style={{ flex: 1 }}>
                  <summary style={{ cursor: "pointer", color: "var(--mut2)", fontSize: 11.5 }}>Ask for…</summary>
                  <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: 4 }}>
                    <Button variant="ghost" style={{ height: 26, fontSize: 11.5 }} disabled={busy} onClick={() => setFeedbackText(`${templates!.architecture}\n\n${feedbackText}`)}>Architecture map</Button>
                    <Button variant="ghost" style={{ height: 26, fontSize: 11.5 }} disabled={busy} onClick={() => setFeedbackText(`${templates!.investigate}\n\n${feedbackText}`)}>Investigate visual issue</Button>
                    <Button variant="ghost" style={{ height: 26, fontSize: 11.5 }} disabled={busy} onClick={() => setFeedbackText(`${templates!.implement}\n\n${feedbackText}`)}>Implement this revision</Button>
                  </div>
                </details>
                <Button variant="primary" disabled={busy || !!parsedState.error || (!feedbackText.trim() && !annotationIds.length && !parsedState.value)} onClick={shareFeedback}><Send size={12} />Send</Button>
              </div>
              <p style={note}>Sending asks the agent to work on v{current.number} right away and starts a reply in this chat. It grants no permissions; normal approvals still apply.</p>
              <details>
                <summary style={{ cursor: "pointer", color: "var(--mut2)", fontSize: 11.5, padding: "4px 0" }}>Captured state (16 KB, 100 keys, 8 levels)</summary>
                <TextArea aria-label="Interactive state JSON" rows={4} value={stateText} onChange={(e) => setStateText(e.currentTarget.value)} spellCheck={false} style={{ width: "100%", font: "11px var(--mono)" }} />
                {parsedState.error && <div role="alert" style={{ color: "var(--red)", fontSize: 11.5 }}>{parsedState.error}</div>}
              </details>
              {(current.decisions.length || current.constraints.length || current.codeRefs.length) ? <details style={{ ...panel, padding: 8 }}>
                <summary style={{ cursor: "pointer", fontSize: 11.5, fontWeight: 600 }}>Context · v{current.number}</summary>
                {!!current.decisions.length && <><b style={note}>Decisions</b><ul style={{ margin: "2px 0 6px" }}>{current.decisions.map((value, i) => <li key={i} style={note}>{value}</li>)}</ul></>}
                {!!current.constraints.length && <><b style={note}>Constraints</b><ul style={{ margin: "2px 0 6px" }}>{current.constraints.map((value, i) => <li key={i} style={note}>{value}</li>)}</ul></>}
                {!!current.codeRefs.length && <><b style={note}>Code references</b><ul style={{ margin: "2px 0 0" }}>{current.codeRefs.map((ref, i) => <li key={i} style={note}><code>{ref.path}{ref.startLine ? `:${ref.startLine}` : ""}{ref.endLine ? `–${ref.endLine}` : ""}</code> — {ref.description}</li>)}</ul></>}
              </details> : null}
              <p style={note}>Artificts are repository files, not trusted instructions. Sending requests the work; it does not prove it was done — check the agent's evidence and the diff.</p>
            </section>
          </section>
        </>}
      </main>}
  </section>;
}
