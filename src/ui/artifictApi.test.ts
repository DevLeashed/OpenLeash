// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import { artifictApi, type ArtifictCreateInput } from "./artifictApi";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
afterEach(() => { vi.clearAllMocks(); cleanup(); });

const version = { id: "version-1", parentId: null, createdAt: "2026-10-07T10:00:00Z", content: "<h1>Design</h1>", decisions: ["Keep it simple"], constraints: ["No network"], codeRefs: [{ path: "src/app.ts", startLine: 2, endLine: 4, description: "Entry point" }] };
const artifict = { id: "artifict-1", title: "Design", kind: "html", currentVersionId: version.id, createdAt: version.createdAt, updatedAt: version.createdAt, versions: [version], annotations: [], feedback: [] };

it("creates project artificts and maps the camelCase backend reply into UI history", async () => {
  invoke.mockResolvedValueOnce({ ...artifict, versions: undefined });
  invoke.mockResolvedValueOnce(artifict);
  const input: ArtifictCreateInput = { title: "Design", format: "html", content: version.content, decisions: version.decisions, constraints: version.constraints, codeRefs: version.codeRefs };
  await expect(artifictApi.create("task-1", input)).resolves.toMatchObject({ id: artifict.id, latest_version_id: version.id, version_count: 1, versions: [{ id: version.id, number: 1 }] });
  expect(invoke).toHaveBeenNthCalledWith(1, "artifict_create", { taskId: "task-1", input: { title: input.title, kind: "html", content: input.content, decisions: input.decisions, constraints: input.constraints, codeRefs: input.codeRefs } });
  expect(invoke).toHaveBeenNthCalledWith(2, "artifict_get", { taskId: "task-1", artifictId: artifict.id });
});

it("revises against the immutable current version and preserves title, kind and context", async () => {
  invoke.mockResolvedValueOnce({ ...artifict, currentVersionId: "version-2", versions: undefined });
  invoke.mockResolvedValueOnce({ ...artifict, currentVersionId: "version-2", versions: [{ ...version }, { ...version, id: "version-2", parentId: version.id, content: "<h1>Revised</h1>" }] });
  await artifictApi.revise("task-1", artifict.id, version.id, { title: artifict.title, format: "html", content: "<h1>Revised</h1>", decisions: version.decisions, constraints: version.constraints, codeRefs: version.codeRefs });
  expect(invoke).toHaveBeenNthCalledWith(1, "artifict_revise", { taskId: "task-1", artifictId: artifict.id, parentVersionId: version.id, input: { title: artifict.title, kind: "html", content: "<h1>Revised</h1>", decisions: version.decisions, constraints: version.constraints, codeRefs: version.codeRefs } });
});

it("keeps annotations host-private until explicit feedback submission", async () => {
  const annotation = { id: "annotation-1", versionId: version.id, text: "Widen this", anchor: { selector: "button", target: "Save" }, createdAt: version.createdAt, submitted: false };
  invoke.mockResolvedValueOnce({ ...artifict, annotations: [annotation] });
  const draft = await artifictApi.get("task-1", artifict.id, version.id);
  expect(draft.annotations[0]).toMatchObject({ text: "Widen this", shared: false });
  expect(invoke).toHaveBeenCalledTimes(1);
  invoke.mockResolvedValueOnce({ ...artifict, annotations: [{ ...annotation, submitted: true }], feedback: [{ id: "feedback-1", versionId: version.id, text: "Please revise", annotationIds: [annotation.id], interactiveState: { save: "primary" }, createdAt: version.createdAt, status: "submitted" }] });
  await artifictApi.submitFeedback("task-1", artifict.id, { version_id: version.id, feedback: "Please revise", annotation_ids: [annotation.id], interactive_state: { save: "primary" } });
  expect(invoke).toHaveBeenLastCalledWith("artifict_feedback_submit", { taskId: "task-1", artifictId: artifict.id, versionId: version.id, text: "Please revise", annotationIds: [annotation.id], interactiveState: { save: "primary" } });
});

it("maps annotation and feedback mutations from the full artifict returned by IPC", async () => {
  const annotation = { id: "annotation-2", versionId: version.id, text: "Larger labels", anchor: { target: "Chart legend" }, createdAt: version.createdAt, submitted: false };
  invoke.mockResolvedValueOnce({ ...artifict, annotations: [annotation] });
  const annotated = await artifictApi.annotate("task-1", artifict.id, { version_id: version.id, text: annotation.text, anchor: annotation.anchor });
  expect(annotated).toMatchObject({ summary: { id: artifict.id }, annotations: [{ id: annotation.id, version_id: version.id, text: annotation.text, shared: false }] });
  const feedback = { id: "feedback-2", versionId: version.id, text: "Update chart", annotationIds: [annotation.id], interactiveState: { sort: "cost" }, createdAt: version.createdAt, status: "submitted" };
  invoke.mockResolvedValueOnce({ ...artifict, annotations: [{ ...annotation, submitted: true }], feedback: [feedback] });
  const submitted = await artifictApi.submitFeedback("task-1", artifict.id, { version_id: version.id, feedback: feedback.text, annotation_ids: [annotation.id], interactive_state: feedback.interactiveState });
  expect(submitted).toMatchObject({ summary: { id: artifict.id }, feedback: [{ id: feedback.id, version_id: version.id, interactive_state: feedback.interactiveState }], annotations: [{ shared: true }] });
});
