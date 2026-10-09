import { invoke } from "@tauri-apps/api/core";

/** UI DTOs. Wire JSON from Tauri is camelCase; keeping mapping here makes the
 * workspace independent of Rust serde details. Artifact records stay data, not
 * trusted instructions, and are never added to the task prompt automatically. */
export type ArtifactFormat = "html" | "svg" | "json" | "markdown";
export interface ArtifactCodeRef { path: string; startLine?: number; endLine?: number; description: string }
export interface ArtifactVersionRef { id: string; parent_id: string | null; number: number; created_at: string }
export interface ArtifactSummary {
  id: string; title: string; format: ArtifactFormat; latest_version_id: string;
  created_at: string; updated_at: string; version_count: number; versions: ArtifactVersionRef[];
  currentVersionId?: string;
}
export interface ArtifactVersion extends ArtifactVersionRef {
  content: string; decisions: string[]; constraints: string[]; codeRefs: ArtifactCodeRef[];
}
export interface ArtifactAnnotation {
  id: string; version_id: string; text: string; selector: string | null; anchor: unknown | null; created_at: string; shared: boolean;
}
export type ArtifactFeedbackStatus = "submitted" | "responded";
export type ArtifactFeedbackDecision = "addressed" | "needs_clarification";
export interface ArtifactFeedback {
  id: string; artifact_id: string; version_id: string; text: string; annotation_ids: string[];
  interactive_state: Record<string, unknown> | null; status: ArtifactFeedbackStatus;
  decision: ArtifactFeedbackDecision | null; response: string | null; created_at: string; resolved_at: string | null;
}
export interface ArtifactDetail { summary: ArtifactSummary; selected_version: ArtifactVersion; annotations: ArtifactAnnotation[]; feedback: ArtifactFeedback[] }
export interface ArtifactCreateInput { title: string; format: ArtifactFormat; content: string; decisions: string[]; constraints: string[]; codeRefs: ArtifactCodeRef[] }
export interface ArtifactReviseInput { title: string; format: ArtifactFormat; content: string; decisions: string[]; constraints: string[]; codeRefs: ArtifactCodeRef[] }

interface WireSummary { id: string; title: string; kind: ArtifactFormat; currentVersionId: string; createdAt: string; updatedAt: string }
interface WireVersion { id: string; parentId: string | null; createdAt: string; content: string; decisions: string[]; constraints: string[]; codeRefs: ArtifactCodeRef[] }
interface WireAnnotation { id: string; versionId: string; text: string; anchor?: unknown; createdAt: string; submitted: boolean }
interface WireFeedback {
  id: string; versionId: string; text: string; annotationIds: string[]; interactiveState?: unknown;
  createdAt: string; status: "submitted" | "responded"; decision?: ArtifactFeedbackDecision; response?: string; responseAt?: string;
}
interface WireArtifact extends WireSummary { versions: WireVersion[]; annotations: WireAnnotation[]; feedback: WireFeedback[] }
const summaryFrom = (w: WireSummary, versions: WireVersion[]): ArtifactSummary => ({
  id: w.id, title: w.title, format: w.kind, latest_version_id: w.currentVersionId, created_at: w.createdAt, updated_at: w.updatedAt,
  version_count: versions.length, versions: versions.map((v, i) => ({ id: v.id, parent_id: v.parentId, number: i + 1, created_at: v.createdAt })),
});
function versionFrom(w: WireVersion, all: WireVersion[]): ArtifactVersion {
  const refs = summaryFrom({ id: "", title: "", kind: "html", currentVersionId: "", createdAt: "", updatedAt: "" }, all).versions;
  return { id: w.id, parent_id: w.parentId, number: refs.find((v) => v.id === w.id)?.number ?? 0, created_at: w.createdAt,
    content: w.content, decisions: w.decisions ?? [], constraints: w.constraints ?? [], codeRefs: w.codeRefs ?? [] };
}
function feedbackFrom(w: WireFeedback, artifactId: string): ArtifactFeedback {
  const state = w.interactiveState;
  return { id: w.id, artifact_id: artifactId, version_id: w.versionId, text: w.text, annotation_ids: w.annotationIds ?? [],
    interactive_state: state && typeof state === "object" && !Array.isArray(state) ? state as Record<string, unknown> : null,
    status: w.status, decision: w.decision ?? null, response: w.response ?? null, created_at: w.createdAt, resolved_at: w.responseAt ?? null };
}
function detailFrom(w: WireArtifact, versionId?: string): ArtifactDetail {
  const versions = w.versions ?? [];
  const chosen = versions.find((v) => v.id === (versionId ?? w.currentVersionId)) ?? versions.at(-1);
  if (!chosen) throw new Error("Artifact has no saved versions.");
  return { summary: summaryFrom(w, versions), selected_version: versionFrom(chosen, versions),
    annotations: (w.annotations ?? []).map((a) => {
      const anchor = a.anchor ?? null; const fields = anchor && typeof anchor === "object" ? anchor as Record<string, unknown> : {};
      return { id: a.id, version_id: a.versionId, text: a.text, selector: typeof fields.selector === "string" ? fields.selector : null, anchor, created_at: a.createdAt, shared: a.submitted };
    }), feedback: (w.feedback ?? []).map((f) => feedbackFrom(f, w.id)) };
}
export const artifactApi = {
  list: async (taskId: string): Promise<ArtifactSummary[]> => {
    const rows = await invoke<WireSummary[]>("artifact_list", { taskId });
    // list rows intentionally omit versions; load manifests so selection/history are accurate.
    return Promise.all(rows.map(async (row) => { const full = await invoke<WireArtifact>("artifact_get", { taskId, artifactId: row.id }); return summaryFrom(row, full.versions); }));
  },
  get: async (taskId: string, artifactId: string, versionId?: string) => detailFrom(await invoke<WireArtifact>("artifact_get", { taskId, artifactId }), versionId),
  create: async (taskId: string, input: ArtifactCreateInput): Promise<ArtifactSummary> => {
    const row = await invoke<WireSummary>("artifact_create", { taskId, input: { title: input.title, kind: input.format, content: input.content, decisions: input.decisions, constraints: input.constraints, codeRefs: input.codeRefs } });
    return summaryFrom(row, (await invoke<WireArtifact>("artifact_get", { taskId, artifactId: row.id })).versions);
  },
  revise: async (taskId: string, artifactId: string, parentVersionId: string, input: ArtifactReviseInput): Promise<ArtifactSummary> => {
    const row = await invoke<WireSummary>("artifact_revise", { taskId, artifactId, parentVersionId, input: { title: input.title, kind: input.format, content: input.content, decisions: input.decisions, constraints: input.constraints, codeRefs: input.codeRefs } });
    return summaryFrom(row, (await invoke<WireArtifact>("artifact_get", { taskId, artifactId })).versions);
  },
  updateMetadata: async (taskId: string, artifactId: string, input: { title: string; format: ArtifactFormat; decisions: string[]; constraints: string[]; codeRefs: ArtifactCodeRef[] }): Promise<ArtifactSummary> => {
    const w = await invoke<WireArtifact>("artifact_get", { taskId, artifactId }); const current = w.versions.find((v) => v.id === w.currentVersionId);
    if (!current) throw new Error("Artifact has no current version.");
    return artifactApi.revise(taskId, artifactId, current.id, { ...input, content: current.content });
  },
  annotate: async (taskId: string, artifactId: string, input: { version_id: string; text: string; selector?: string; anchor?: unknown }): Promise<ArtifactDetail> => {
    const w = await invoke<WireArtifact>("artifact_annotate", { taskId, artifactId, versionId: input.version_id, text: input.text, anchor: input.anchor ?? (input.selector ? { selector: input.selector } : null) });
    return detailFrom(w, input.version_id);
  },
  submitFeedback: (taskId: string, artifactId: string, input: { version_id: string; feedback: string; annotation_ids: string[]; interactive_state: Record<string, unknown> | null }) =>
    invoke<WireArtifact>("artifact_feedback_submit", { taskId, artifactId, versionId: input.version_id, text: input.feedback, annotationIds: input.annotation_ids, interactiveState: input.interactive_state }).then((w) => detailFrom(w, input.version_id)),
  feedbackList: async (taskId: string, artifactId?: string) => (await invoke<WireFeedback[]>("artifact_feedback_list", { taskId, artifactId: artifactId ?? null })).map((w) => feedbackFrom(w, artifactId ?? "")),
  feedbackRespond: (taskId: string, feedbackId: string, decision: ArtifactFeedbackDecision, response: string) => invoke<void>("artifact_feedback_respond", { taskId, feedbackId, decision, response }),
};

export const MAX_ARTIFACT_CONTENT_BYTES = 256 * 1024;
export const MAX_ARTIFACT_STATE_BYTES = 16 * 1024;
export const MAX_ARTIFACT_STATE_FIELDS = 100;
export const MAX_ARTIFACT_STATE_DEPTH = 8;
function stateDepth(value: unknown, depth = 0): number {
  if (!value || typeof value !== "object") return depth;
  const children = Array.isArray(value) ? value : Object.values(value as Record<string, unknown>);
  return children.reduce((max, child) => Math.max(max, stateDepth(child, depth + 1)), depth);
}
export function parseArtifactState(raw: string): { value: Record<string, unknown> | null; error: string } {
  if (!raw.trim()) return { value: null, error: "" };
  if (new TextEncoder().encode(raw).byteLength > MAX_ARTIFACT_STATE_BYTES) return { value: null, error: "State JSON must be 16 KB or smaller." };
  try {
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object" || Array.isArray(value)) return { value: null, error: "State must be a JSON object." };
    if (Object.keys(value).length > MAX_ARTIFACT_STATE_FIELDS) return { value: null, error: `State can contain at most ${MAX_ARTIFACT_STATE_FIELDS} top-level fields.` };
    if (stateDepth(value) > MAX_ARTIFACT_STATE_DEPTH) return { value: null, error: `State values can be at most ${MAX_ARTIFACT_STATE_DEPTH} levels deep.` };
    return { value: value as Record<string, unknown>, error: "" };
  } catch { return { value: null, error: "Enter valid JSON, or leave the state empty." }; }
}
export const artifactContentError = (content: string) => new TextEncoder().encode(content).byteLength > MAX_ARTIFACT_CONTENT_BYTES ? "Artifact content must be 256 KB or smaller." : "";
