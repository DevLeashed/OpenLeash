// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ArtifictWorkspace } from "./ArtifictWorkspace";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));
vi.mock("./Visualization", () => ({ Visualization: () => <div>Preview</div> }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });


it("renders the title, kind, and version in the artifict header", async () => {
  const version = { id: "v1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: "<h1>Hello</h1>", decisions: [], constraints: [], codeRefs: [] };
  const artifict = { id: "art-1", title: "A Sample Artifict", kind: "html", currentVersionId: "v1", createdAt: version.createdAt, updatedAt: version.createdAt, versions: [version], annotations: [], feedback: [] };
  invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [{ id: artifict.id, title: artifict.title, kind: artifict.kind, currentVersionId: artifict.currentVersionId, createdAt: artifict.createdAt, updatedAt: artifict.updatedAt }] : structuredClone(artifict));
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  // The workspace opens on the record itself, so the name appears once.
  expect(await screen.findByText("A Sample Artifict")).toBeTruthy();
  expect(screen.getAllByText("HTML").length).toBeGreaterThan(0);
  expect(screen.getByLabelText("Exact artifict version")).toBeTruthy();
});

it("keeps the set of records on a view of its own instead of beside the record", async () => {
  const versions = [
    { id: "g1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: "# One", decisions: [], constraints: [], codeRefs: [] },
    { id: "g2", parentId: null, createdAt: "2026-10-08T11:00:00Z", content: "# Two", decisions: [], constraints: [], codeRefs: [] },
  ];
  const summary = (id: string, title: string, versionId: string) => ({ id, title, kind: "markdown", currentVersionId: versionId, createdAt: versions[0]!.createdAt, updatedAt: versions[1]!.createdAt });
  invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "artifict_list") return [summary("a1", "First record", "g1"), summary("a2", "Second record", "g2")];
    const second = String(args?.artifictId ?? "a1") === "a2";
    return structuredClone({ ...summary(second ? "a2" : "a1", second ? "Second record" : "First record", second ? "g2" : "g1"), versions, annotations: [], feedback: [] });
  });
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("One");
  fireEvent.click(screen.getByRole("button", { name: "All artificts" }));
  // While the set is open there is nothing to preview beside the cards: no
  // version picker, and no request panel for a record nobody has opened.
  expect(screen.queryByLabelText("Exact artifict version")).toBeNull();
  expect(screen.queryByLabelText("Send to the agent")).toBeNull();
  expect(screen.getByRole("button", { name: "Back to artifict" })).toBeTruthy();
  expect(screen.getByText("Second record")).toBeTruthy();
  // The card is a box with an overlay button, because a live miniature is an
  // isolated frame and a frame cannot live inside a button.
  fireEvent.click(screen.getByRole("button", { name: "Open Second record" }));
  expect(await screen.findByText("Two")).toBeTruthy();
  expect(screen.getByLabelText("Exact artifict version")).toBeTruthy();
  // Back on the record: the set is gone, and the header offers it again.
  expect(screen.queryByText("First record")).toBeNull();
  expect(screen.getByRole("button", { name: "All artificts" })).toBeTruthy();
});

it("opens the requested durable artifict version when navigated from an inline card", async () => {
  const versions = [
    { id: "v1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: "old", decisions: [], constraints: [], codeRefs: [] },
    { id: "v2", parentId: "v1", createdAt: "2026-10-08T11:00:00Z", content: "new", decisions: [], constraints: [], codeRefs: [] },
  ];
  const exactVersion = versions[1]!.id;
  const artifict = { id: "art-2", title: "Pinned Artifict", kind: "markdown", currentVersionId: exactVersion, createdAt: versions[0]!.createdAt, updatedAt: versions[1]!.createdAt, versions, annotations: [], feedback: [] };
  invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [{ id: artifict.id, title: artifict.title, kind: artifict.kind, currentVersionId: artifict.currentVersionId, createdAt: artifict.createdAt, updatedAt: artifict.updatedAt }] : structuredClone(artifict));
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" initialSelection={{ artifictId: "art-2", versionId: versions[0]!.id }} />);
  expect(await screen.findByText("Pinned Artifict")).toBeTruthy();
  const selectedVersion = await screen.findByLabelText("Exact artifict version") as HTMLSelectElement;
  expect(selectedVersion.value).toBe(versions[0]!.id);
  await screen.findByText("old");
  expect(invoke).toHaveBeenCalledWith("artifict_get", expect.objectContaining({ taskId: "task-1", artifictId: "art-2" }));
});

it("shows an ask-the-model empty state without manual artifict creation controls", async () => {
  invoke.mockResolvedValue([]);
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  expect(await screen.findByText(/Ask the agent in the conversation to create/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: /New artifict/i })).toBeNull();
  expect(screen.queryByLabelText("Artifict content")).toBeNull();
});

const draftVersions = [
  { id: "draft-v1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: "First version", decisions: [], constraints: [], codeRefs: [] },
  { id: "draft-v2", parentId: "draft-v1", createdAt: "2026-10-08T11:00:00Z", content: "Second version", decisions: [], constraints: [], codeRefs: [] },
];
const draftArtifict = { id: "draft-art", title: "Draft test", kind: "markdown", currentVersionId: "draft-v2", createdAt: draftVersions[0]!.createdAt, updatedAt: draftVersions[1]!.createdAt, versions: draftVersions, annotations: [], feedback: [] };
function mockDraftArtifict(fail: string) {
  invoke.mockImplementation(async (command: string) => {
    if (command === fail) throw new Error("Could not save");
    return command === "artifict_list" ? [draftArtifict] : structuredClone(draftArtifict);
  });
}

it("keeps feedback text and state after a failed submission", async () => {
  mockDraftArtifict("artifict_feedback_submit");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByLabelText("Follow up for agent");
  fireEvent.change(screen.getByLabelText("Follow up for agent"), { target: { value: "Please fix the labels" } });
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"sort":"score"}' } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await screen.findByRole("alert");
  expect((screen.getByLabelText("Follow up for agent") as HTMLTextAreaElement).value).toBe("Please fix the labels");
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe('{"sort":"score"}');
});

it("keeps comment text after a failed save", async () => {
  mockDraftArtifict("artifict_annotate");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByLabelText("Comment text");
  fireEvent.change(screen.getByLabelText("Comment text"), { target: { value: "This label is wrong" } });
  fireEvent.click(screen.getByRole("button", { name: "Save comment" }));
  await screen.findByRole("alert");
  expect((screen.getByLabelText("Comment text") as HTMLTextAreaElement).value).toBe("This label is wrong");
});

it("keeps drafts separate for each exact version and restores them on return", async () => {
  mockDraftArtifict("");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  fireEvent.change(screen.getByLabelText("Follow up for agent"), { target: { value: "Change version two" } });
  fireEvent.change(screen.getByLabelText("Comment text"), { target: { value: "Version two note" } });
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"version":2}' } });
  fireEvent.change(screen.getByLabelText("Exact artifict version"), { target: { value: "draft-v1" } });
  await screen.findByText("First version");
  expect((screen.getByLabelText("Follow up for agent") as HTMLTextAreaElement).value).toBe("");
  expect((screen.getByLabelText("Comment text") as HTMLTextAreaElement).value).toBe("");
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe("");
  fireEvent.change(screen.getByLabelText("Exact artifict version"), { target: { value: "draft-v2" } });
  await waitFor(() => expect((screen.getByLabelText("Follow up for agent") as HTMLTextAreaElement).value).toBe("Change version two"));
  expect((screen.getByLabelText("Comment text") as HTMLTextAreaElement).value).toBe("Version two note");
});

it("stages an exact-revision implementation request without sending or approving it", async () => {
  mockDraftArtifict("");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  fireEvent.click(screen.getByText("Ask for…"));
  fireEvent.click(screen.getByRole("button", { name: "Implement this revision" }));
  const request = (screen.getByLabelText("Follow up for agent") as HTMLTextAreaElement).value;
  expect(request).toContain("draft-art, exact revision draft-v2");
  expect(request).toContain("existing permissions");
  expect(invoke.mock.calls.some(([command]) => command === "artifict_feedback_submit")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_feedback_submit", expect.objectContaining({ versionId: "draft-v2", text: request.trim() })));
});

it("prioritizes simple feedback and shows version metadata", async () => {
  const annotated = { ...draftArtifict, versions: draftVersions.map((v) => ({ ...v, decisions: ["Keep axes consistent"], constraints: ["No external assets"], codeRefs: [{ path: "src/chart.ts", startLine: 12, description: "Chart entry" }] })) };
  invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [annotated] : structuredClone(annotated));
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Comments · v2");
  const captured = screen.getByText(/Captured state/).closest("details");
  expect(captured?.open).toBe(false);
  // One panel: the record's comments, and the request that goes with them.
  // There is no second conversation beside it — the chat pane is the chat.
  const comments = screen.getByRole("region", { name: "Comments on this version" });
  expect(comments.contains(screen.getByLabelText("Follow up for agent"))).toBe(true);
  expect(comments.contains(captured!)).toBe(true);
  expect(screen.queryByRole("complementary", { name: "Artifict conversation" })).toBeNull();
  // The set is reachable from the header and returns to the record it opened.
  fireEvent.click(screen.getByRole("button", { name: "All artificts" }));
  expect(screen.getAllByText("Draft test").length).toBeGreaterThan(0);
  fireEvent.click(screen.getByRole("button", { name: "Back to artifict" }));
  expect(screen.getByLabelText("Exact artifict version")).toBeTruthy();
  expect(screen.getAllByText("Keep axes consistent").length).toBeGreaterThan(0);
  expect(screen.getByText("No external assets")).toBeTruthy();
  expect(screen.getByText("src/chart.ts:12")).toBeTruthy();
});

it("warns that the record on screen is a work in progress, naming only a real recorded stage", async () => {
  const record = { schema: "openleash.collaboration.v1", title: "Architecture", purpose: "Explain real code", stage: "prototype", nodes: [], edges: [], evidence: [], limitations: [] };
  const collaborationVersions = [{ id: "w1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: JSON.stringify(record), decisions: [], constraints: [], codeRefs: [] }];
  const collaboration = { id: "wip-1", title: "Architecture", kind: "json", currentVersionId: "w1", createdAt: collaborationVersions[0]!.createdAt, updatedAt: collaborationVersions[0]!.createdAt, versions: collaborationVersions, annotations: [], feedback: [] };
  invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [{ id: collaboration.id, title: collaboration.title, kind: collaboration.kind, currentVersionId: collaboration.currentVersionId, createdAt: collaboration.createdAt, updatedAt: collaboration.updatedAt }] : structuredClone(collaboration));
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Explain real code");
  // Above the content it describes: the version on screen, and the stage quoted
  // as its author's claim rather than as a verdict.
  const notice = screen.getByRole("note");
  expect(notice.textContent).toContain("Work in progress");
  expect(notice.textContent).toContain("not a finished deliverable or an approval");
  expect(notice.textContent).toContain("v1 · Recorded stage: prototype");
  expect(notice.textContent).toContain("author-reported, not independently certified");
  cleanup();
  // A record that declares no stage — or declares one nothing can read — is
  // warned about all the same, and none is invented for it.
  const fragmentVersions = [{ id: "f1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: '{"stage":"verified"}', decisions: [], constraints: [], codeRefs: [] }];
  const fragment = { id: "wip-2", title: "Fragment", kind: "json", currentVersionId: "f1", createdAt: fragmentVersions[0]!.createdAt, updatedAt: fragmentVersions[0]!.createdAt, versions: fragmentVersions, annotations: [], feedback: [] };
  invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [{ id: fragment.id, title: fragment.title, kind: fragment.kind, currentVersionId: fragment.currentVersionId, createdAt: fragment.createdAt, updatedAt: fragment.updatedAt }] : structuredClone(fragment));
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByLabelText("Exact artifict version");
  const plain = screen.getByRole("note");
  expect(plain.textContent).toContain("Work in progress");
  expect(plain.textContent).not.toContain("Recorded stage");
  expect(plain.textContent).not.toContain("verified");
});

it("uses the successful mutation reply and clears only submitted fields", async () => {
  let resolve!: (value: unknown) => void;
  invoke.mockImplementation(async (command: string) => {
    if (command === "artifict_feedback_submit") return new Promise((done) => { resolve = done; });
    return command === "artifict_list" ? [draftArtifict] : structuredClone(draftArtifict);
  });
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  fireEvent.change(screen.getByLabelText("Follow up for agent"), { target: { value: "Submitted request" } });
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"sort":"cost"}' } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  expect((screen.getByLabelText("Exact artifict version") as HTMLSelectElement).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Follow up for agent"), { target: { value: "My next request" } });
  resolve({ ...draftArtifict, feedback: [{ id: "feedback-new", versionId: "draft-v2", text: "Submitted request", annotationIds: [], createdAt: draftVersions[1]!.createdAt, status: "submitted" }] });
  await screen.findByText("Submitted request");
  expect((screen.getByLabelText("Follow up for agent") as HTMLTextAreaElement).value).toBe("My next request");
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe("");
});

it("previews the richer kinds through the shared canvas, not just HTML and Markdown", async () => {
  const versions = [{ id: "t1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: "raw log line\nsecond line", decisions: [], constraints: [], codeRefs: [] }];
  const artifict = { id: "art-text", title: "Build log", kind: "text", currentVersionId: "t1", createdAt: versions[0]!.createdAt, updatedAt: versions[0]!.createdAt, versions, annotations: [], feedback: [] };
  invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [{ id: artifict.id, title: artifict.title, kind: artifict.kind, currentVersionId: artifict.currentVersionId, createdAt: artifict.createdAt, updatedAt: artifict.updatedAt }] : structuredClone(artifict));
  const { container } = render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  expect(await screen.findByText("Build log")).toBeTruthy();
  expect(screen.getAllByText("TEXT").length).toBeGreaterThan(0);
  expect(container.querySelector(".artifict-text")?.textContent).toBe("raw log line\nsecond line");
});

it("keeps the whole record in one column, with no second conversation beside it", async () => {
  mockDraftArtifict("");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  const workspace = document.querySelector("section[aria-label='Artifict workspace']")!;
  // Nothing else in the surface claims to be a conversation, and no toggle
  // offers to open one.
  expect(workspace.querySelectorAll("aside, [aria-label='Artifict conversation']")).toHaveLength(0);
  expect(screen.queryByRole("button", { name: "Conversation" })).toBeNull();
  // The request box is part of the record's own panel.
  expect(screen.getByRole("region", { name: "Send to the agent" })).toBeTruthy();
});

it("keeps exactly one card live — the record the workspace is on", async () => {
  const summary = (id: string, title: string) => ({ id, title, kind: "html", currentVersionId: `${id}-v`, createdAt: "2026-10-08T10:00:00Z", updatedAt: "2026-10-08T10:00:00Z" });
  const versions = (id: string) => [{ id: `${id}-v`, parentId: null, createdAt: "2026-10-08T10:00:00Z", content: `<h1>${id}</h1>`, decisions: [], constraints: [], codeRefs: [] }];
  invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "artifict_list") return [summary("a1", "First"), summary("a2", "Second")];
    if (command === "visualization_publish") return "doc-x";
    if (command === "visualization_release") return undefined;
    const id = String(args?.artifictId ?? "a1");
    return structuredClone({ ...summary(id, id === "a2" ? "Second" : "First"), versions: versions(id), annotations: [], feedback: [] });
  });
  const { container } = render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("First");
  fireEvent.click(screen.getByRole("button", { name: "All artificts" }));
  // Running every card would publish a document per artifact and start that
  // artifact's scripts; only the one the workspace is on is rendered.
  await waitFor(() => expect(container.querySelectorAll("iframe.artifict-thumb-frame")).toHaveLength(1));
  expect(screen.getByText("Second")).toBeTruthy();
  expect(invoke.mock.calls.filter(([command]) => command === "visualization_publish")).toHaveLength(1);
});
