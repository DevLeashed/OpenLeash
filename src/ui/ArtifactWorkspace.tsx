import { useCallback, useEffect, useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { RefreshCw, Send } from "lucide-react";
import { ArtifactAnnotation, ArtifactDetail, ArtifactFeedback, ArtifactFormat, ArtifactSummary, artifactApi, parseArtifactState } from "./artifactApi";
import { Visualization } from "./Visualization";
import { Button, Input, TextArea } from "./primitives";
import { mdComponents, remarkOl } from "./mdx";
import type { VisualizationFeedbackDraft } from "./visualizationFeedback";

const note = { color: "var(--mut3)", fontSize: 11, lineHeight: 1.45 } as const;
const panel = { border: "1px solid var(--line)", background: "var(--panel)", borderRadius: 9, padding: 10 } as const;
type ArtifactDraft = {
  selector: string; target: string; annotationText: string; annotationIds: string[];
  stateText: string; feedbackText: string; feedbackDraft: VisualizationFeedbackDraft | null;
};
const emptyDraft: ArtifactDraft = { selector: "", target: "", annotationText: "", annotationIds: [], stateText: "", feedbackText: "", feedbackDraft: null };
export type DiffLine = { kind: "same" | "remove" | "add"; text: string };
export function artifactDiff(before: string, after: string): DiffLine[] {
  const a = before.split("\n"), b = after.split("\n"); let prefix = 0, suffix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix++;
  while (suffix < a.length - prefix && suffix < b.length - prefix && a[a.length - 1 - suffix] === b[b.length - 1 - suffix]) suffix++;
  return [...a.slice(0, prefix).map((text) => ({ kind: "same" as const, text })), ...a.slice(prefix, a.length - suffix).map((text) => ({ kind: "remove" as const, text })), ...b.slice(prefix, b.length - suffix).map((text) => ({ kind: "add" as const, text })), ...(suffix ? a.slice(a.length - suffix).map((text) => ({ kind: "same" as const, text })) : [])];
}
function ArtifactPreview({ detail, label, onFeedbackDraft }: { detail: ArtifactDetail; label?: string; onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void }) {
  const { content } = detail.selected_version;
  return <ArtifactContentPreview format={detail.summary.format} content={content} label={label ?? detail.summary.title} onFeedbackDraft={onFeedbackDraft} />;
}

export function ArtifactContentPreview({ format, content, label, onFeedbackDraft, autoStart = false }: {
  format: ArtifactFormat;
  content: string;
  label?: string;
  onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void;
  autoStart?: boolean;
}) {
  if (format === "html") return <Visualization source={content} label={label} autoStart={autoStart} onFeedbackDraft={onFeedbackDraft} />;
  if (format === "markdown") return <div className="md" style={{ ...panel, maxHeight: 420, overflow: "auto" }}><ReactMarkdown remarkPlugins={[remarkGfm, remarkOl]} components={mdComponents}>{content}</ReactMarkdown></div>;
  if (format === "svg") return <div style={{ ...panel, maxHeight: 420, overflow: "auto", textAlign: "center" }}><img alt="SVG artifact preview" src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(content)}`} style={{ maxWidth: "100%", maxHeight: 370 }} /></div>;
  return <pre aria-label={label ?? "JSON artifact preview"} style={{ ...panel, maxHeight: 420, overflow: "auto", whiteSpace: "pre-wrap", wordBreak: "break-word" }}><code>{content}</code></pre>;
}

/** Project-scoped, persistent artifact workbench. Preview frames get no app
 * bridge. Annotation targets and state snapshots are host-entered, untrusted
 * data; only the explicit feedback submission makes them agent-readable. */
export function ArtifactWorkspace({ taskId, taskTitle, initialSelection }: { taskId: string; taskTitle: string; initialSelection?: { artifactId: string; versionId: string } | null }) {
  const [rows, setRows] = useState<ArtifactSummary[]>([]);
  const [artifactId, setArtifactId] = useState("");
  const [versionId, setVersionId] = useState("");
  const [detail, setDetail] = useState<ArtifactDetail | null>(null);
  const [compareVersion, setCompareVersion] = useState("");
  const [compareDetail, setCompareDetail] = useState<ArtifactDetail | null>(null);
  const [compare, setCompare] = useState(false);
  const [showList, setShowList] = useState(true);
  // Drafts belong to an exact version, not whichever preview happens to be open.
  // They remain local and never cross the explicit submission boundary.
  const [drafts, setDrafts] = useState<Record<string, ArtifactDraft>>({});
  const draftKey = `${taskId}:${artifactId}:${versionId}`;
  const { selector, target, annotationText, annotationIds, stateText, feedbackText, feedbackDraft } = drafts[draftKey] ?? emptyDraft;
  const updateDraft = (patch: Partial<ArtifactDraft>) => setDrafts((old) => ({ ...old, [draftKey]: { ...(old[draftKey] ?? emptyDraft), ...patch } }));
  const setSelector = (value: string) => updateDraft({ selector: value });
  const setTarget = (value: string) => updateDraft({ target: value });
  const setAnnotationText = (value: string) => updateDraft({ annotationText: value });
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
  const initialArtifactId = initialSelection?.artifactId ?? "";
  const initialVersionId = initialSelection?.versionId ?? "";

  const load = useCallback(async (preferId?: string, preferVersion?: string) => {
    const list = await artifactApi.list(taskId);
    setRows(list);
    const id = preferId && list.some((item) => item.id === preferId) ? preferId : artifactId && list.some((item) => item.id === artifactId) ? artifactId : list[0]?.id ?? "";
    setArtifactId(id);
    const summary = list.find((item) => item.id === id);
    if (!summary) { setDetail(null); setVersionId(""); return; }
    const vid = preferVersion && summary.versions.some((item) => item.id === preferVersion) ? preferVersion : id === artifactId && summary.versions.some((item) => item.id === versionId) ? versionId : summary.latest_version_id;
    setVersionId(vid);
    const next = await artifactApi.get(taskId, id, vid);
    setDetail(next);
    const other = next.summary.versions.find((item) => item.id !== vid); setCompareVersion((old) => old !== vid && next.summary.versions.some((item) => item.id === old) ? old : other?.id ?? "");
  }, [taskId, artifactId, versionId]);

  useEffect(() => {
    let alive = true; setBusy(true); setError("");
    artifactApi.list(taskId).then(async (list) => {
      if (!alive) return;
      setRows(list);
      const id = initialArtifactId && list.some((item) => item.id === initialArtifactId) ? initialArtifactId : list[0]?.id ?? ""; setArtifactId(id);
      const summary = list.find((item) => item.id === id);
      if (summary) {
        const requestedVersion = initialArtifactId === id ? initialVersionId : undefined;
        const selectedVersion = requestedVersion && summary.versions.some((item) => item.id === requestedVersion) ? requestedVersion : summary.latest_version_id;
        const next = await artifactApi.get(taskId, summary.id, selectedVersion);
        if (!alive) return;
        setDetail(next); setVersionId(next.selected_version.id);
        setCompareVersion(next.summary.versions.at(-2)?.id ?? "");
      }
    }).catch((e) => { if (alive) setError(String(e)); }).finally(() => { if (alive) setBusy(false); });
    return () => { alive = false; };
  }, [taskId, initialArtifactId, initialVersionId]);
  useEffect(() => {
    let alive = true;
    if (artifactId && versionId) artifactApi.get(taskId, artifactId, versionId).then((next) => {
      if (!alive) return;
      setDetail(next);
    }).catch((e) => { if (alive) setError(String(e)); });
    else setDetail(null);
    return () => { alive = false; };
  }, [taskId, artifactId, versionId]);
  useEffect(() => {
    let alive = true;
    if (compare && artifactId && compareVersion) artifactApi.get(taskId, artifactId, compareVersion).then((next) => { if (alive) setCompareDetail(next); }).catch((e) => { if (alive) setError(String(e)); });
    else setCompareDetail(null);
    return () => { alive = false; };
  }, [compare, taskId, artifactId, compareVersion]);

  const parsedState = useMemo(() => parseArtifactState(stateText), [stateText]);
  const comparedDetail = compareDetail?.summary.id === artifactId && compareDetail.selected_version.id === compareVersion ? compareDetail : null;
  const previewDraft = useCallback((draft: VisualizationFeedbackDraft) => {
    setDrafts((old) => ({ ...old, [draftKey]: { ...(old[draftKey] ?? emptyDraft), feedbackDraft: draft } }));
  }, [draftKey]);
  const selectedDetail = detail?.summary.id === artifactId && detail.selected_version.id === versionId ? detail : null;
  const current = selectedDetail?.selected_version;
  const usePreviewDraft = () => {
    if (!feedbackDraft) return;
    if (feedbackDraft.state) setStateText(JSON.stringify(feedbackDraft.state, null, 2));
    if (feedbackDraft.target?.selector) setSelector(feedbackDraft.target.selector);
    if (feedbackDraft.target?.label) setTarget(feedbackDraft.target.label);
    setFeedbackDraft(null);
  };
  const annotations: ArtifactAnnotation[] = detail?.annotations.filter((item) => item.version_id === versionId) ?? [];
  const feedbackHistory: ArtifactFeedback[] = detail?.feedback.filter((item) => item.version_id === versionId) ?? [];
  const diff = current && comparedDetail ? artifactDiff(comparedDetail.selected_version.content, current.content) : [];
  const execute = async (fn: () => Promise<ArtifactDetail | undefined>, success: string, id = artifactId, vid?: string, onSaved?: () => void) => {
    if (busy) return;
    setBusy(true); setError(""); setNotice("");
    try {
      const result = await fn();
      onSaved?.();
      if (result) {
        setDetail(result);
        setRows((old) => old.map((row) => row.id === result.summary.id ? result.summary : row));
      } else await load(id, vid);
      if (id) setNotice(success);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const pickArtifact = (id: string) => { const row = rows.find((item) => item.id === id); setArtifactId(id); setVersionId(row?.latest_version_id ?? ""); setError(""); setNotice(""); setCompare(false); };
  const saveAnnotation = () => {
    if (!current || !annotationText.trim()) return setError("Write a note for this exact version first.");
    if (selector.length > 300 || target.length > 300 || annotationText.length > 4000) return setError("Selector and target are limited to 300 characters; note is limited to 4,000.");
    const anchor = { selector: selector.trim() || undefined, target: target.trim() || undefined };
    void execute(() => artifactApi.annotate(taskId, artifactId, { version_id: current.id, text: annotationText.trim(), selector: selector.trim() || undefined, anchor }), `Private annotation saved on v${current.number}.`, artifactId, current.id, () => setDrafts((old) => {
      const draft = old[draftKey] ?? emptyDraft;
      return { ...old, [draftKey]: { ...draft, annotationText: draft.annotationText === annotationText ? "" : draft.annotationText } };
    }));
  };
  const shareFeedback = () => {
    if (!current) return setError("Select an exact artifact version first.");
    if (parsedState.error) return setError(parsedState.error);
    if (!feedbackText.trim() && !annotationIds.length && !parsedState.value) return setError("Add feedback, choose an annotation, or enter state values.");
    void execute(() => artifactApi.submitFeedback(taskId, artifactId, { version_id: current.id, feedback: feedbackText.trim(), annotation_ids: annotationIds, interactive_state: parsedState.value }), `Feedback sent to the agent for artifact v${current.number}.`, artifactId, current.id, () => setDrafts((old) => {
      const draft = old[draftKey] ?? emptyDraft;
      return { ...old, [draftKey]: { ...draft,
        feedbackText: draft.feedbackText === feedbackText ? "" : draft.feedbackText,
        stateText: draft.stateText === stateText ? "" : draft.stateText,
        annotationIds: draft.annotationIds.filter((id) => !annotationIds.includes(id)),
      } };
    }));
  };

  return <section aria-label="Artifact workspace" style={{ height: "100%", minHeight: 0, display: "flex", flexDirection: "column", overflow: "hidden" }}>
    <header style={{ flex: "none", display: "flex", alignItems: "center", flexWrap: "wrap", gap: 8, padding: "10px 12px", borderBottom: "1px solid var(--line)" }}><b style={{ flex: 1 }}>Artifacts · {taskTitle}</b><Button variant="ghost" aria-expanded={showList} onClick={() => setShowList((value) => !value)}>{showList ? "Hide list" : "Show list"}</Button><span style={note}>Saved in this project</span><Button variant="ghost" disabled={busy} onClick={() => void execute(async () => undefined, "Artifacts refreshed.")}><RefreshCw size={13} />Refresh</Button></header>
    <div style={{ flex: 1, minHeight: 0, display: "grid", gridTemplateColumns: showList ? "minmax(120px,180px) minmax(0,1fr)" : "minmax(0,1fr)" }}>
      {showList && <aside aria-label="Artifact list" style={{ overflow: "auto", padding: 8, borderRight: "1px solid var(--line)" }}><div style={note}>Project artifacts · {rows.length}</div>{rows.map((row) => <button key={row.id} type="button" aria-pressed={row.id === artifactId} disabled={busy} onClick={() => pickArtifact(row.id)} style={{ display: "block", width: "100%", textAlign: "left", border: "1px solid", borderColor: row.id === artifactId ? "var(--accent)" : "var(--line)", background: row.id === artifactId ? "var(--raise)" : "transparent", borderRadius: 7, color: "var(--fg)", padding: 8, marginTop: 6, cursor: "pointer" }}><b>{row.title}</b><div style={note}>{row.format.toUpperCase()} · {row.version_count} versions</div></button>)}{!rows.length && <p style={note}>No model-created artifacts yet. Ask the model to create one in the conversation.</p>}</aside>}
      <main style={{ minWidth: 0, overflow: "auto", padding: 12 }}>{busy && <div role="status" style={note}>Loading / saving…</div>}{error && <div role="alert" style={{ color: "var(--red)" }}>{error}</div>}{notice && <div role="status" style={{ color: "var(--st-done)" }}>{notice}</div>}
        {!detail && !busy && <div style={panel}>Artifacts created by the model appear here. Ask the model in the conversation to create a design, prototype, diagram, or decision record.</div>}
        {detail && <>
          <div className="artifact-version-bar" style={{ display: "flex", flexWrap: "wrap", alignItems: "center", gap: 10, marginBottom: 12, padding: "10px 12px", border: "1px solid var(--line)", borderRadius: 11, background: "var(--panel)" }}>
            <div style={{ display: "flex", alignItems: "center", gap: 8, flex: "1 1 220px", minWidth: 0 }}>
              <b title={detail.summary.title} style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: 14, letterSpacing: "-.01em" }}>{detail.summary.title}</b>
              <span style={{ flex: "none", border: "1px solid var(--line)", borderRadius: 999, padding: "3px 8px", color: "var(--mut2)", fontSize: 10, fontWeight: 650, letterSpacing: ".06em" }}>{detail.summary.format.toUpperCase()}</span>
            </div>
            <label style={{ display: "flex", alignItems: "center", gap: 7, color: "var(--mut3)", fontSize: 11 }}><span>Version</span><select aria-label="Exact artifact version" value={versionId} disabled={busy} onChange={(e) => { setVersionId(e.currentTarget.value); setCompareVersion(detail.summary.versions.find((v) => v.id !== e.currentTarget.value)?.id ?? ""); setError(""); setNotice(""); }} style={{ maxWidth: 230, minWidth: 0, height: 28, padding: "0 8px", border: "1px solid var(--line)", borderRadius: 7, background: "var(--raise)", color: "var(--fg)", font: "inherit", fontSize: 11.5 }}>{detail.summary.versions.map((v) => <option key={v.id} value={v.id}>v{v.number} · {new Date(v.created_at).toLocaleString()}</option>)}</select></label>
            {detail.summary.versions.length > 1 && <Button variant={compare ? "primary" : "ghost"} style={{ height: 28, padding: "0 9px", fontSize: 11.5 }} onClick={() => setCompare(!compare)} aria-pressed={compare}>{compare ? "Hide comparison" : "Compare versions"}</Button>}
          </div>
          {compare && detail.summary.versions.length > 1 && <label style={{ ...panel, ...note, display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>Compare selected v{current?.number} with <select aria-label="Compare against version" value={compareVersion} onChange={(e) => setCompareVersion(e.currentTarget.value)}>{detail.summary.versions.filter((v) => v.id !== versionId).map((v) => <option key={v.id} value={v.id}>v{v.number}</option>)}</select></label>}
          {selectedDetail && compare && comparedDetail ? <><div style={{ display: "grid", gridTemplateColumns: "repeat(2,minmax(0,1fr))", gap: 8 }}><section aria-label={`Compared version ${comparedDetail.selected_version.number}`}><b style={note}>Compared · v{comparedDetail.selected_version.number}</b><ArtifactPreview key={`${artifactId}:${compareVersion}`} detail={comparedDetail} label="Compared artifact version" /></section><section aria-label={`Selected version ${selectedDetail.selected_version.number}`}><b style={note}>Selected · v{selectedDetail.selected_version.number}</b><ArtifactPreview key={`${artifactId}:${versionId}`} detail={selectedDetail} onFeedbackDraft={previewDraft} /></section></div><div aria-label="Version content diff" style={{ ...panel, margin: "8px 0", maxHeight: 180, overflow: "auto", font: "11px var(--mono)", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>{diff.map((line, i) => <div key={`${i}-${line.kind}`} style={{ color: line.kind === "remove" ? "var(--diff-del)" : line.kind === "add" ? "var(--diff-add)" : "var(--mut3)" }}>{line.kind === "remove" ? "− " : line.kind === "add" ? "+ " : "  "}{line.text || " "}</div>)}</div></> : selectedDetail && <ArtifactPreview key={`${artifactId}:${versionId}`} detail={selectedDetail} onFeedbackDraft={previewDraft} />}
          {current && <>
            <section aria-label="Review and send feedback" style={{ ...panel, margin: "12px 0" }}>
              <b>What would you like to change?</b><p style={note}>Your request is tied to v{current.number}. Drafts stay private until you send them.</p>
              <TextArea aria-label="Follow up for agent" rows={3} value={feedbackText} onChange={(e) => setFeedbackText(e.currentTarget.value)} placeholder="e.g. Make the charts larger and add a cost comparison…" style={{ width: "100%" }} />
              {(annotationIds.length > 0 || stateText.trim()) && <div aria-label="Feedback review" style={{ ...panel, marginTop: 6 }}><b style={{ fontSize: 11 }}>Also included with this request</b><div style={note}>Selected notes: {annotationIds.length ? annotations.filter((a) => annotationIds.includes(a.id)).map((a) => a.text).join(" · ") : "none"}</div><div style={note}>State: {parsedState.value ? JSON.stringify(parsedState.value) : parsedState.error || "none"}</div></div>}
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8, marginTop: 8 }}><span style={note}>{feedbackDraft ? "A preview proposal is waiting in advanced feedback below." : "Only submitted requests are visible to the agent."}</span><Button variant="primary" disabled={busy || !!parsedState.error || (!feedbackText.trim() && !annotationIds.length && !parsedState.value)} onClick={shareFeedback}><Send size={12} />Send feedback to agent</Button></div>
            </section>
            <details style={{ marginBottom: 12 }}><summary style={{ cursor: "pointer", color: "var(--mut2)", fontSize: 12, padding: "8px 0" }}>Advanced feedback · annotations and interactive state</summary>
            <section aria-label="Annotate exact version" style={{ ...panel, marginBottom: 8 }}><b>Annotate exact version · v{current.number}</b><p style={note}>Optional: identify a visible label or selector. Preview suggestions remain untrusted drafts until you review and send them.</p><label style={note}>Selector <Input aria-label="Annotation selector" value={selector} maxLength={300} onChange={(e) => setSelector(e.currentTarget.value)} placeholder="e.g. [data-control='volume']" /></label><label style={{ ...note, display: "block", marginTop: 4 }}>Visible target / node <Input aria-label="Annotation target" value={target} maxLength={300} onChange={(e) => setTarget(e.currentTarget.value)} placeholder="Control label or diagram node" /></label><label style={{ ...note, display: "block", marginTop: 4 }}>Private note <TextArea aria-label="Annotation note" value={annotationText} rows={2} onChange={(e) => setAnnotationText(e.currentTarget.value)} placeholder="What should change at this exact point?" /></label><div style={{ textAlign: "right", marginTop: 5 }}><Button disabled={busy || !annotationText.trim()} onClick={saveAnnotation}>Save private annotation</Button></div>{annotations.map((a: ArtifactAnnotation) => <label key={a.id} style={{ display: "flex", gap: 6, padding: 5, fontSize: 11.5 }}><input type="checkbox" aria-label={`Share annotation: ${a.text}`} checked={annotationIds.includes(a.id)} onChange={(e) => setAnnotationIds((old) => e.currentTarget.checked ? [...old, a.id] : old.filter((id) => id !== a.id))} /><span>{a.selector || "Target"}{a.anchor && typeof a.anchor === "object" && "target" in a.anchor ? ` · ${String((a.anchor as { target: unknown }).target)}` : ""} — {a.text}{a.shared && <em style={{ color: "var(--st-done)" }}> · shared</em>}</span></label>)}</section>
            <section aria-label="Interactive state snapshot" style={{ ...panel, marginBottom: 8 }}><b>Play state · selected v{current.number}</b><p style={note}>A preview may suggest state values, but they stay an untrusted draft until you review and explicitly send them. You can also enter values manually. Limit: 16 KB, 100 root keys, 8 levels.</p>{feedbackDraft && <div role="status" style={{ ...panel, marginBottom: 6 }}><b>Untrusted preview proposal</b><pre style={{ maxHeight: 100, overflow: "auto", whiteSpace: "pre-wrap" }}>{JSON.stringify(feedbackDraft, null, 2)}</pre><Button onClick={usePreviewDraft}>Review proposal in feedback fields</Button> <Button variant="ghost" onClick={() => setFeedbackDraft(null)}>Dismiss</Button></div>}<TextArea aria-label="Interactive state JSON" rows={4} value={stateText} onChange={(e) => setStateText(e.currentTarget.value)} spellCheck={false} style={{ width: "100%", font: "11px var(--mono)" }} />{parsedState.error && <div role="alert" style={{ color: "var(--red)" }}>{parsedState.error}</div>}{parsedState.value && <pre aria-label="State snapshot preview" style={{ ...panel, maxHeight: 130, overflow: "auto", whiteSpace: "pre-wrap" }}>{JSON.stringify(parsedState.value, null, 2)}</pre>}</section>
            </details>
            {(current.decisions.length > 0 || current.constraints.length > 0 || current.codeRefs.length > 0) && <details style={{ ...panel, marginBottom: 12 }}><summary style={{ cursor: "pointer" }}>Decisions and constraints · v{current.number}</summary>{current.decisions.length > 0 && <><b style={note}>Decisions</b><ul>{current.decisions.map((value, i) => <li key={i} style={note}>{value}</li>)}</ul></>}{current.constraints.length > 0 && <><b style={note}>Constraints</b><ul>{current.constraints.map((value, i) => <li key={i} style={note}>{value}</li>)}</ul></>}{current.codeRefs.length > 0 && <><b style={note}>Code references</b><ul>{current.codeRefs.map((ref, i) => <li key={i} style={note}><code>{ref.path}{ref.startLine ? `:${ref.startLine}` : ""}{ref.endLine ? `–${ref.endLine}` : ""}</code> — {ref.description}</li>)}</ul></>}</details>}
            <section aria-label="Feedback history" style={{ ...panel, marginBottom: 12 }}><b>Feedback history · v{current.number}</b>{!feedbackHistory.length && <p style={note}>No feedback for this version.</p>}{feedbackHistory.map((f: ArtifactFeedback) => <div key={f.id} style={{ borderTop: "1px solid var(--line)", paddingTop: 6, marginTop: 6 }}><div style={{ ...note, color: f.status === "responded" ? "var(--st-done)" : "var(--accent)" }}>{f.status === "submitted" ? "Sent · awaiting agent" : `Agent ${f.decision?.replace(/_/g, " ") ?? "responded"}`} · {new Date(f.created_at).toLocaleString()}</div><div style={{ whiteSpace: "pre-wrap", fontSize: 12 }}>{f.text || "(state or annotation only)"}</div>{f.response && <div style={{ ...note, marginTop: 3 }}>Agent response: {f.response}</div>}</div>)}</section>
          </>}
        </>}
      </main>
    </div>
  </section>;
}
