// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import { artifactApi, type ArtifactCreateInput } from "./artifactApi";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
afterEach(() => { vi.clearAllMocks(); cleanup(); });

const version = { id: "version-1", parentId: null, createdAt: "2026-10-07T10:00:00Z", content: "<h1>Design</h1>", decisions: ["Keep it simple"], constraints: ["No network"], codeRefs: [{ path: "src/app.ts", startLine: 2, endLine: 4, description: "Entry point" }] };
const artifact = { id: "artifact-1", title: "Design", kind: "html", currentVersionId: version.id, createdAt: version.createdAt, updatedAt: version.createdAt, versions: [version], annotations: [], feedback: [] };

it("creates project artifacts and maps the camelCase backend reply into UI history", async () => {
  invoke.mockResolvedValueOnce({ ...artifact, versions: undefined });
  invoke.mockResolvedValueOnce(artifact);
  const input: ArtifactCreateInput = { title: "Design", format: "html", content: version.content, decisions: version.decisions, constraints: version.constraints, codeRefs: version.codeRefs };
  await expect(artifactApi.create("task-1", input)).resolves.toMatchObject({ id: artifact.id, latest_version_id: version.id, version_count: 1, versions: [{ id: version.id, number: 1 }] });
  expect(invoke).toHaveBeenNthCalledWith(1, "artifact_create", { taskId: "task-1", input: { title: input.title, kind: "html", content: input.content, decisions: input.decisions, constraints: input.constraints, codeRefs: input.codeRefs } });
  expect(invoke).toHaveBeenNthCalledWith(2, "artifact_get", { taskId: "task-1", artifactId: artifact.id });
});

it("revises against the immutable current version and preserves title, kind and context", async () => {
  invoke.mockResolvedValueOnce({ ...artifact, currentVersionId: "version-2", versions: undefined });
  invoke.mockResolvedValueOnce({ ...artifact, currentVersionId: "version-2", versions: [{ ...version }, { ...version, id: "version-2", parentId: version.id, content: "<h1>Revised</h1>" }] });
  await artifactApi.revise("task-1", artifact.id, version.id, { title: artifact.title, format: "html", content: "<h1>Revised</h1>", decisions: version.decisions, constraints: version.constraints, codeRefs: version.codeRefs });
  expect(invoke).toHaveBeenNthCalledWith(1, "artifact_revise", { taskId: "task-1", artifactId: artifact.id, parentVersionId: version.id, input: { title: artifact.title, kind: "html", content: "<h1>Revised</h1>", decisions: version.decisions, constraints: version.constraints, codeRefs: version.codeRefs } });
});

it("keeps annotations host-private until explicit feedback submission", async () => {
  const annotation = { id: "annotation-1", versionId: version.id, text: "Widen this", anchor: { selector: "button", target: "Save" }, createdAt: version.createdAt, submitted: false };
  invoke.mockResolvedValueOnce({ ...artifact, annotations: [annotation] });
  const draft = await artifactApi.get("task-1", artifact.id, version.id);
  expect(draft.annotations[0]).toMatchObject({ text: "Widen this", shared: false });
  expect(invoke).toHaveBeenCalledTimes(1);
  invoke.mockResolvedValueOnce({ ...artifact, annotations: [{ ...annotation, submitted: true }], feedback: [{ id: "feedback-1", versionId: version.id, text: "Please revise", annotationIds: [annotation.id], interactiveState: { save: "primary" }, createdAt: version.createdAt, status: "submitted" }] });
  await artifactApi.submitFeedback("task-1", artifact.id, { version_id: version.id, feedback: "Please revise", annotation_ids: [annotation.id], interactive_state: { save: "primary" } });
  expect(invoke).toHaveBeenLastCalledWith("artifact_feedback_submit", { taskId: "task-1", artifactId: artifact.id, versionId: version.id, text: "Please revise", annotationIds: [annotation.id], interactiveState: { save: "primary" } });
});

it("maps annotation and feedback mutations from the full artifact returned by IPC", async () => {
  const annotation = { id: "annotation-2", versionId: version.id, text: "Larger labels", anchor: { target: "Chart legend" }, createdAt: version.createdAt, submitted: false };
  invoke.mockResolvedValueOnce({ ...artifact, annotations: [annotation] });
  const annotated = await artifactApi.annotate("task-1", artifact.id, { version_id: version.id, text: annotation.text, anchor: annotation.anchor });
  expect(annotated).toMatchObject({ summary: { id: artifact.id }, annotations: [{ id: annotation.id, version_id: version.id, text: annotation.text, shared: false }] });
  const feedback = { id: "feedback-2", versionId: version.id, text: "Update chart", annotationIds: [annotation.id], interactiveState: { sort: "cost" }, createdAt: version.createdAt, status: "submitted" };
  invoke.mockResolvedValueOnce({ ...artifact, annotations: [{ ...annotation, submitted: true }], feedback: [feedback] });
  const submitted = await artifactApi.submitFeedback("task-1", artifact.id, { version_id: version.id, feedback: feedback.text, annotation_ids: [annotation.id], interactive_state: feedback.interactiveState });
  expect(submitted).toMatchObject({ summary: { id: artifact.id }, feedback: [{ id: feedback.id, version_id: version.id, interactive_state: feedback.interactiveState }], annotations: [{ shared: true }] });
});
