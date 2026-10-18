import { invoke } from "@tauri-apps/api/core";
import { parseImageSpec } from "./ImageExplanation";
import { svgImageUrl } from "./hostTheme";

/** UI DTOs. Wire JSON from Tauri is camelCase; keeping mapping here makes the
 * workspace independent of Rust serde details. Artifict records stay data, not
 * trusted instructions, and are never added to the task prompt automatically. */
export type ArtifictFormat = "html" | "svg" | "json" | "markdown" | "text" | "image";
export interface ArtifictCodeRef { path: string; startLine?: number; endLine?: number; description: string }
export interface ArtifictVersionRef { id: string; parent_id: string | null; number: number; created_at: string;
  /** `user` when the user wrote this revision in the studio (a canvas or source
   *  edit, or a restore); absent for the agent's own. */
  author?: "user";
  /** The short note such a revision carries, e.g. "Restored from v2". */
  note?: string }
export interface ArtifictSummary {
  id: string; title: string; format: ArtifictFormat; latest_version_id: string;
  created_at: string; updated_at: string; version_count: number; versions: ArtifictVersionRef[];
  currentVersionId?: string;
  pendingFeedback?: number;
  thumbnail?: string;
  recordStage?: string;
  excerpt?: string;
  /** The current revision's source. The list already reads every version to build
   *  the manifest, so the gallery can draw a miniature of the record itself
   *  rather than a placeholder, at no extra round trip. */
  preview?: string;
}
export interface ArtifictVersion extends ArtifictVersionRef {
  content: string; decisions: string[]; constraints: string[]; codeRefs: ArtifictCodeRef[];
}
export interface ArtifictAnnotation {
  id: string; version_id: string; text: string; selector: string | null; anchor: unknown | null; created_at: string; shared: boolean;
  /** Percentages of the reviewed document (HTML) or preview surface (other
   *  kinds), when the comment was pinned to a point. */
  point: { x: number; y: number } | null;
  /** Where inside the picked element the click landed, as percentages of that
   *  element's box, so an HTML pin sits on the exact spot even as layout moves. */
  offset: { x: number; y: number } | null;
  target: string | null;
}
export type ArtifictFeedbackStatus = "submitted" | "responded";
export type ArtifictFeedbackDecision = "addressed" | "needs_clarification";
export interface ArtifictFeedback {
  id: string; artifict_id: string; version_id: string; text: string; annotation_ids: string[];
  interactive_state: Record<string, unknown> | null; status: ArtifictFeedbackStatus;
  decision: ArtifictFeedbackDecision | null; response: string | null; created_at: string; resolved_at: string | null;
}
export interface ArtifictDetail { summary: ArtifictSummary; selected_version: ArtifictVersion; annotations: ArtifictAnnotation[]; feedback: ArtifictFeedback[] }
export interface ArtifictCreateInput { title: string; format: ArtifictFormat; content: string; decisions: string[]; constraints: string[]; codeRefs: ArtifictCodeRef[] }
export interface ArtifictReviseInput { title: string; format: ArtifictFormat; content: string; decisions: string[]; constraints: string[]; codeRefs: ArtifictCodeRef[] }

interface WireSummary { id: string; title: string; kind: ArtifictFormat; currentVersionId: string; createdAt: string; updatedAt: string }
interface WireVersion { id: string; parentId: string | null; createdAt: string; content: string; decisions: string[]; constraints: string[]; codeRefs: ArtifictCodeRef[]; author?: string; note?: string }
interface WireAnnotation { id: string; versionId: string; text: string; anchor?: unknown; createdAt: string; submitted: boolean }
interface WireFeedback {
  id: string; versionId: string; text: string; annotationIds: string[]; interactiveState?: unknown;
  createdAt: string; status: "submitted" | "responded"; decision?: ArtifictFeedbackDecision; response?: string; responseAt?: string;
}
interface WireArtifict extends WireSummary { versions: WireVersion[]; annotations: WireAnnotation[]; feedback: WireFeedback[] }
const versionRef = (v: WireVersion, i: number): ArtifictVersionRef => ({ id: v.id, parent_id: v.parentId, number: i + 1, created_at: v.createdAt,
  ...(v.author === "user" ? { author: "user" as const } : {}), ...(typeof v.note === "string" && v.note ? { note: v.note } : {}) });
const summaryFrom = (w: WireSummary, versions: WireVersion[]): ArtifictSummary => ({
  id: w.id, title: w.title, format: w.kind, latest_version_id: w.currentVersionId, created_at: w.createdAt, updated_at: w.updatedAt,
  version_count: versions.length, versions: versions.map(versionRef),
});
function overviewFrom(w: WireArtifict): ArtifictSummary {
  const current = w.versions.find(v => v.id === w.currentVersionId);
  let recordStage: string | undefined;
  let image: string | undefined;
  if (current) {
    if (w.kind === "json") {
      try { const record = JSON.parse(current.content); if (record.schema === "openleash.collaboration.v1") { recordStage = ["concept", "prototype", "implemented", "verified"].includes(record.stage) ? record.stage : undefined; if (typeof record.image === "string") image = JSON.parse(record.image).src; } } catch { /* Generic JSON still has a text overview. */ }
    } else if (w.kind === "image") {
      // A picture artifict's own content is the thumbnail, whether it is a bare
      // source or the bounded explanation object, so the list shows the picture
      // itself rather than a format placeholder.
      image = parseImageSpec(current.content)?.src ?? current.content.trim();
    }
  }
  return { ...summaryFrom(w, w.versions), pendingFeedback: (w.feedback ?? []).filter(f => f.status === "submitted" || f.decision === "needs_clarification").length,
    excerpt: (current?.decisions?.[0] ?? "Saved project artifict").slice(0, 180), recordStage, preview: current?.content,
    thumbnail: w.kind === "svg" && current ? svgImageUrl(current.content) : typeof image === "string" && /^data:image\/(png|jpeg|webp);base64,/i.test(image) && image.length <= 200000 ? image : undefined };
}
function versionFrom(w: WireVersion, all: WireVersion[]): ArtifictVersion {
  const ref = versionRef(w, all.findIndex((v) => v.id === w.id));
  return { ...ref, content: w.content, decisions: w.decisions ?? [], constraints: w.constraints ?? [], codeRefs: w.codeRefs ?? [] };
}
function feedbackFrom(w: WireFeedback, artifictId: string): ArtifictFeedback {
  const state = w.interactiveState;
  return { id: w.id, artifict_id: artifictId, version_id: w.versionId, text: w.text, annotation_ids: w.annotationIds ?? [],
    interactive_state: state && typeof state === "object" && !Array.isArray(state) ? state as Record<string, unknown> : null,
    status: w.status, decision: w.decision ?? null, response: w.response ?? null, created_at: w.createdAt, resolved_at: w.responseAt ?? null };
}
function detailFrom(w: WireArtifict, versionId?: string): ArtifictDetail {
  const versions = w.versions ?? [];
  const chosen = versions.find((v) => v.id === (versionId ?? w.currentVersionId)) ?? versions.at(-1);
  if (!chosen) throw new Error("Artifict has no saved versions.");
  return { summary: summaryFrom(w, versions), selected_version: versionFrom(chosen, versions),
    annotations: (w.annotations ?? []).map((a) => {
      const anchor = a.anchor ?? null; const fields = anchor && typeof anchor === "object" ? anchor as Record<string, unknown> : {};
      const point = typeof fields.x === "number" && typeof fields.y === "number" ? { x: fields.x, y: fields.y } : null;
      const offset = typeof fields.ox === "number" && typeof fields.oy === "number" ? { x: fields.ox, y: fields.oy } : null;
      return { id: a.id, version_id: a.versionId, text: a.text, selector: typeof fields.selector === "string" ? fields.selector : null, anchor, created_at: a.createdAt, shared: a.submitted, point, offset, target: typeof fields.target === "string" ? fields.target : null };
    }), feedback: (w.feedback ?? []).map((f) => feedbackFrom(f, w.id)) };
}
export const artifictApi = {
  list: async (taskId: string): Promise<ArtifictSummary[]> => {
    const rows = await invoke<WireSummary[]>("artifict_list", { taskId });
    // list rows intentionally omit versions; load manifests so selection/history are accurate.
    return Promise.all(rows.map(async (row) => { const full = await invoke<WireArtifict>("artifict_get", { taskId, artifictId: row.id }); return overviewFrom(full); }));
  },
  get: async (taskId: string, artifictId: string, versionId?: string) => detailFrom(await invoke<WireArtifict>("artifict_get", { taskId, artifictId }), versionId),
  create: async (taskId: string, input: ArtifictCreateInput): Promise<ArtifictSummary> => {
    const row = await invoke<WireSummary>("artifict_create", { taskId, input: { title: input.title, kind: input.format, content: input.content, decisions: input.decisions, constraints: input.constraints, codeRefs: input.codeRefs } });
    return summaryFrom(row, (await invoke<WireArtifict>("artifict_get", { taskId, artifictId: row.id })).versions);
  },
  revise: async (taskId: string, artifictId: string, parentVersionId: string, input: ArtifictReviseInput): Promise<ArtifictSummary> => {
    const row = await invoke<WireSummary>("artifict_revise", { taskId, artifictId, parentVersionId, input: { title: input.title, kind: input.format, content: input.content, decisions: input.decisions, constraints: input.constraints, codeRefs: input.codeRefs } });
    return summaryFrom(row, (await invoke<WireArtifict>("artifict_get", { taskId, artifictId })).versions);
  },
  updateMetadata: async (taskId: string, artifictId: string, input: { title: string; format: ArtifictFormat; decisions: string[]; constraints: string[]; codeRefs: ArtifictCodeRef[] }): Promise<ArtifictSummary> => {
    const w = await invoke<WireArtifict>("artifict_get", { taskId, artifictId }); const current = w.versions.find((v) => v.id === w.currentVersionId);
    if (!current) throw new Error("Artifict has no current version.");
    return artifictApi.revise(taskId, artifictId, current.id, { ...input, content: current.content });
  },
  annotate: async (taskId: string, artifictId: string, input: { version_id: string; text: string; selector?: string; target?: string; point?: { x: number; y: number } | null; offset?: { x: number; y: number } | null; anchor?: unknown }): Promise<ArtifictDetail> => {
    const merged = input.anchor ?? {
      ...(input.selector ? { selector: input.selector } : {}),
      ...(input.target ? { target: input.target } : {}),
      ...(input.point ? { x: input.point.x, y: input.point.y } : {}),
      ...(input.offset ? { ox: input.offset.x, oy: input.offset.y } : {}),
    };
    const w = await invoke<WireArtifict>("artifict_annotate", { taskId, artifictId, versionId: input.version_id, text: input.text, anchor: Object.keys(merged as object).length ? merged : null });
    return detailFrom(w, input.version_id);
  },
  removeAnnotation: async (taskId: string, artifictId: string, annotationId: string, versionId?: string): Promise<ArtifictDetail> => {
    const w = await invoke<WireArtifict>("artifict_annotate_remove", { taskId, artifictId, annotationId });
    return detailFrom(w, versionId);
  },
  submitFeedback: (taskId: string, artifictId: string, input: { version_id: string; feedback: string; annotation_ids: string[]; interactive_state: Record<string, unknown> | null }) =>
    invoke<WireArtifict>("artifict_feedback_submit", { taskId, artifictId, versionId: input.version_id, text: input.feedback, annotationIds: input.annotation_ids, interactiveState: input.interactive_state }).then((w) => detailFrom(w, input.version_id)),
  feedbackList: async (taskId: string, artifictId?: string) => (await invoke<WireFeedback[]>("artifict_feedback_list", { taskId, artifictId: artifictId ?? null })).map((w) => feedbackFrom(w, artifictId ?? "")),
  feedbackRespond: (taskId: string, feedbackId: string, decision: ArtifictFeedbackDecision, response: string) => invoke<void>("artifict_feedback_respond", { taskId, feedbackId, decision, response }),
  /** Put an earlier version back as a new revision on top of `parentVersionId`
   *  (the latest); history is never rewritten. Resolves to the new latest. */
  restore: async (taskId: string, artifictId: string, parentVersionId: string, fromVersionId: string): Promise<ArtifictDetail> =>
    detailFrom(await invoke<WireArtifict>("artifict_restore", { taskId, artifictId, parentVersionId, fromVersionId })),
  /** Save the user's own edit (canvas or source) as a new revision on top of
   *  `parentVersionId`. Resolves to the new latest. */
  edit: async (taskId: string, artifictId: string, parentVersionId: string, content: string, note: string): Promise<ArtifictDetail> =>
    detailFrom(await invoke<WireArtifict>("artifict_edit", { taskId, artifictId, parentVersionId, content, note })),
  /** Write one exact version to a path the user chose in the save dialog. The
   *  backend builds the file from the stored version; `theme` only fills in the
   *  app tokens a standalone HTML page would otherwise lack. */
  export: (taskId: string, artifictId: string, versionId: string, path: string, theme: Record<string, string> | null) =>
    invoke<string>("artifict_export", { taskId, artifictId, versionId, path, theme }),
};

/** The file an artifict version exports as, or null when it cannot be (a
 *  picture that is only a link). */
export function exportTarget(format: ArtifictFormat, content: string): { ext: string; name: string } | null {
  switch (format) {
    case "html": return { ext: "html", name: "HTML page" };
    case "svg": return { ext: "svg", name: "SVG image" };
    case "markdown": return { ext: "md", name: "Markdown" };
    case "text": return { ext: "txt", name: "Text" };
    case "json": return { ext: "json", name: "JSON" };
    case "image": {
      const src = parseImageSpec(content)?.src ?? content.trim();
      const m = /^data:image\/(png|jpeg|webp);base64,/i.exec(src);
      if (!m) return null;
      const kind = m[1]!.toLowerCase();
      return { ext: kind === "jpeg" ? "jpg" : kind, name: `${kind.toUpperCase()} image` };
    }
  }
}

/** A file name for an exported version: the title, filesystem-safe. */
export function exportFileName(title: string, number: number, ext: string): string {
  const base = title.replace(/[^\w\- ]+/g, "").trim().replace(/\s+/g, "-").slice(0, 60) || "artifict";
  return `${base}-v${number}.${ext}`;
}

export const MAX_ARTIFICT_CONTENT_BYTES = 256 * 1024;
export const MAX_ARTIFICT_STATE_BYTES = 16 * 1024;
export const MAX_ARTIFICT_STATE_FIELDS = 100;
export const MAX_ARTIFICT_STATE_DEPTH = 8;
function stateDepth(value: unknown, depth = 0): number {
  if (!value || typeof value !== "object") return depth;
  const children = Array.isArray(value) ? value : Object.values(value as Record<string, unknown>);
  return children.reduce((max, child) => Math.max(max, stateDepth(child, depth + 1)), depth);
}
export function parseArtifictState(raw: string): { value: Record<string, unknown> | null; error: string } {
  if (!raw.trim()) return { value: null, error: "" };
  if (new TextEncoder().encode(raw).byteLength > MAX_ARTIFICT_STATE_BYTES) return { value: null, error: "State JSON must be 16 KB or smaller." };
  try {
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object" || Array.isArray(value)) return { value: null, error: "State must be a JSON object." };
    if (Object.keys(value).length > MAX_ARTIFICT_STATE_FIELDS) return { value: null, error: `State can contain at most ${MAX_ARTIFICT_STATE_FIELDS} top-level fields.` };
    if (stateDepth(value) > MAX_ARTIFICT_STATE_DEPTH) return { value: null, error: `State values can be at most ${MAX_ARTIFICT_STATE_DEPTH} levels deep.` };
    return { value: value as Record<string, unknown>, error: "" };
  } catch { return { value: null, error: "Enter valid JSON, or leave the state empty." }; }
}
export const artifictContentError = (content: string) => new TextEncoder().encode(content).byteLength > MAX_ARTIFICT_CONTENT_BYTES ? "Artifict content must be 256 KB or smaller." : "";
