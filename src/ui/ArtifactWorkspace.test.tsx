// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ArtifactWorkspace } from "./ArtifactWorkspace";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));
vi.mock("./Visualization", () => ({ Visualization: () => <div>Preview</div> }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });


it("renders the title, kind, and version in the artifact header", async () => {
  const version = { id: "v1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: "<h1>Hello</h1>", decisions: [], constraints: [], codeRefs: [] };
  const artifact = { id: "art-1", title: "A Sample Artifact", kind: "html", currentVersionId: "v1", createdAt: version.createdAt, updatedAt: version.createdAt, versions: [version], annotations: [], feedback: [] };
  invoke.mockImplementation(async (command: string) => command === "artifact_list" ? [{ id: artifact.id, title: artifact.title, kind: artifact.kind, currentVersionId: artifact.currentVersionId, createdAt: artifact.createdAt, updatedAt: artifact.updatedAt }] : structuredClone(artifact));
  render(<ArtifactWorkspace taskId="task-1" taskTitle="Chat" />);
  expect(await screen.findAllByText("A Sample Artifact")).toHaveLength(2);
  expect(screen.getByText("HTML")).toBeTruthy();
  expect(screen.getByLabelText("Exact artifact version")).toBeTruthy();
});

it("opens the requested durable artifact version when navigated from an inline card", async () => {
  const versions = [
    { id: "v1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: "old", decisions: [], constraints: [], codeRefs: [] },
    { id: "v2", parentId: "v1", createdAt: "2026-10-08T11:00:00Z", content: "new", decisions: [], constraints: [], codeRefs: [] },
  ];
  const exactVersion = versions[1]!.id;
  const artifact = { id: "art-2", title: "Pinned Artifact", kind: "markdown", currentVersionId: exactVersion, createdAt: versions[0]!.createdAt, updatedAt: versions[1]!.createdAt, versions, annotations: [], feedback: [] };
  invoke.mockImplementation(async (command: string) => command === "artifact_list" ? [{ id: artifact.id, title: artifact.title, kind: artifact.kind, currentVersionId: artifact.currentVersionId, createdAt: artifact.createdAt, updatedAt: artifact.updatedAt }] : structuredClone(artifact));
  render(<ArtifactWorkspace taskId="task-1" taskTitle="Chat" initialSelection={{ artifactId: "art-2", versionId: versions[0]!.id }} />);
  expect(await screen.findAllByText("Pinned Artifact")).toHaveLength(2);
  const selectedVersion = await screen.findByLabelText("Exact artifact version") as HTMLSelectElement;
  expect(selectedVersion.value).toBe(versions[0]!.id);
  await screen.findByText("old");
  expect(invoke).toHaveBeenCalledWith("artifact_get", expect.objectContaining({ taskId: "task-1", artifactId: "art-2" }));
});

it("shows an ask-the-model empty state without manual artifact creation controls", async () => {
  invoke.mockResolvedValue([]);
  render(<ArtifactWorkspace taskId="task-1" taskTitle="Chat" />);
  expect(await screen.findByText(/Ask the model in the conversation to create/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: /New artifact/i })).toBeNull();
  expect(screen.queryByLabelText("Artifact content")).toBeNull();
});

const draftVersions = [
  { id: "draft-v1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: "First version", decisions: [], constraints: [], codeRefs: [] },
  { id: "draft-v2", parentId: "draft-v1", createdAt: "2026-10-08T11:00:00Z", content: "Second version", decisions: [], constraints: [], codeRefs: [] },
];
const draftArtifact = { id: "draft-art", title: "Draft test", kind: "markdown", currentVersionId: "draft-v2", createdAt: draftVersions[0]!.createdAt, updatedAt: draftVersions[1]!.createdAt, versions: draftVersions, annotations: [], feedback: [] };
function mockDraftArtifact(fail: string) {
  invoke.mockImplementation(async (command: string) => {
    if (command === fail) throw new Error("Could not save");
    return command === "artifact_list" ? [draftArtifact] : structuredClone(draftArtifact);
  });
}

it("keeps feedback text and state after a failed submission", async () => {
  mockDraftArtifact("artifact_feedback_submit");
  render(<ArtifactWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByLabelText("Follow up for agent");
  fireEvent.change(screen.getByLabelText("Follow up for agent"), { target: { value: "Please fix the labels" } });
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"sort":"score"}' } });
  fireEvent.click(screen.getByRole("button", { name: "Send feedback to agent" }));
  await screen.findByRole("alert");
  expect((screen.getByLabelText("Follow up for agent") as HTMLTextAreaElement).value).toBe("Please fix the labels");
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe('{"sort":"score"}');
});

it("keeps annotation text after a failed save", async () => {
  mockDraftArtifact("artifact_annotate");
  render(<ArtifactWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByLabelText("Annotation note");
  fireEvent.change(screen.getByLabelText("Annotation note"), { target: { value: "This label is wrong" } });
  fireEvent.click(screen.getByRole("button", { name: "Save private annotation" }));
  await screen.findByRole("alert");
  expect((screen.getByLabelText("Annotation note") as HTMLTextAreaElement).value).toBe("This label is wrong");
});

it("keeps drafts separate for each exact version and restores them on return", async () => {
  mockDraftArtifact("");
  render(<ArtifactWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  fireEvent.change(screen.getByLabelText("Follow up for agent"), { target: { value: "Change version two" } });
  fireEvent.change(screen.getByLabelText("Annotation note"), { target: { value: "Version two note" } });
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"version":2}' } });
  fireEvent.change(screen.getByLabelText("Exact artifact version"), { target: { value: "draft-v1" } });
  await screen.findByText("First version");
  expect((screen.getByLabelText("Follow up for agent") as HTMLTextAreaElement).value).toBe("");
  expect((screen.getByLabelText("Annotation note") as HTMLTextAreaElement).value).toBe("");
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe("");
  fireEvent.change(screen.getByLabelText("Exact artifact version"), { target: { value: "draft-v2" } });
  await waitFor(() => expect((screen.getByLabelText("Follow up for agent") as HTMLTextAreaElement).value).toBe("Change version two"));
  expect((screen.getByLabelText("Annotation note") as HTMLTextAreaElement).value).toBe("Version two note");
});

it("prioritizes simple feedback, lets the list be hidden, and shows version metadata", async () => {
  const annotated = { ...draftArtifact, versions: draftVersions.map((v) => ({ ...v, decisions: ["Keep axes consistent"], constraints: ["No external assets"], codeRefs: [{ path: "src/chart.ts", startLine: 12, description: "Chart entry" }] })) };
  invoke.mockImplementation(async (command: string) => command === "artifact_list" ? [annotated] : structuredClone(annotated));
  render(<ArtifactWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("What would you like to change?");
  const advanced = screen.getByText("Advanced feedback · annotations and interactive state").closest("details");
  expect(advanced?.open).toBe(false);
  const feedback = screen.getByRole("region", { name: "Review and send feedback" });
  expect(feedback.compareDocumentPosition(advanced!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Hide list" }));
  expect(screen.queryByRole("complementary", { name: "Artifact list" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Show list" }));
  expect(screen.getByRole("complementary", { name: "Artifact list" })).toBeTruthy();
  expect(screen.getByText("Keep axes consistent")).toBeTruthy();
  expect(screen.getByText("No external assets")).toBeTruthy();
  expect(screen.getByText("src/chart.ts:12")).toBeTruthy();
});

it("uses the successful mutation reply and clears only submitted fields", async () => {
  let resolve!: (value: unknown) => void;
  invoke.mockImplementation(async (command: string) => {
    if (command === "artifact_feedback_submit") return new Promise((done) => { resolve = done; });
    return command === "artifact_list" ? [draftArtifact] : structuredClone(draftArtifact);
  });
  render(<ArtifactWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  fireEvent.change(screen.getByLabelText("Follow up for agent"), { target: { value: "Submitted request" } });
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"sort":"cost"}' } });
  fireEvent.click(screen.getByRole("button", { name: "Send feedback to agent" }));
  expect((screen.getByLabelText("Exact artifact version") as HTMLSelectElement).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Follow up for agent"), { target: { value: "My next request" } });
  resolve({ ...draftArtifact, feedback: [{ id: "feedback-new", versionId: "draft-v2", text: "Submitted request", annotationIds: [], createdAt: draftVersions[1]!.createdAt, status: "submitted" }] });
  await screen.findByText("Submitted request");
  expect((screen.getByLabelText("Follow up for agent") as HTMLTextAreaElement).value).toBe("My next request");
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe("");
});
