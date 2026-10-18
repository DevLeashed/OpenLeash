// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ArtifictWorkspace } from "./ArtifictWorkspace";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));
vi.mock("./Visualization", () => ({ Visualization: () => <div>Preview</div> }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

/** Comment the way a user does: turn on comment mode and click the sheet. jsdom
 *  has no layout, so the sheet is given a box for the click to land in. */
function pinCommentOnSheet(label: RegExp | string, at = { clientX: 50, clientY: 20 }) {
  fireEvent.click(screen.getByRole("radio", { name: "Comment" }));
  const surface = screen.getByLabelText(label);
  surface.getBoundingClientRect = () => ({ left: 0, top: 0, width: 200, height: 100, right: 200, bottom: 100, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
  fireEvent.click(surface, at);
}
/** Comments, Tweaks, Details: floating cards opened from the studio toolbar. */
const openTab = (name: string) => fireEvent.click(screen.getByRole("button", { name }));
/** Pick a version from the studio's themed version menu by its number. */
function pickVersion(number: number) {
  fireEvent.click(screen.getByLabelText("Exact artifict version"));
  const option = screen.getAllByRole("option").find((o) => o.querySelector(".dd-option-label")?.textContent === `v${number}`);
  fireEvent.click(option!);
}
const shownVersion = () => screen.getByLabelText("Exact artifict version").querySelector(".ddv")?.textContent;


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
  await screen.findByLabelText("Exact artifict version");
  expect(shownVersion()).toBe("v1");
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

it("keeps captured state after a failed submission", async () => {
  mockDraftArtifict("artifict_feedback_submit");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  openTab("Details");
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"sort":"score"}' } });
  fireEvent.click(screen.getByRole("button", { name: "Send to agent" }));
  await screen.findByRole("alert");
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe('{"sort":"score"}');
});

it("keeps comment text after a failed save", async () => {
  mockDraftArtifict("artifict_annotate");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  pinCommentOnSheet("Draft test · v2");
  fireEvent.change(await screen.findByLabelText("Comment text"), { target: { value: "This label is wrong" } });
  fireEvent.click(screen.getByRole("button", { name: "Add comment" }));
  await screen.findByRole("alert");
  expect((screen.getByLabelText("Comment text") as HTMLTextAreaElement).value).toBe("This label is wrong");
});

it("keeps drafts separate for each exact version and restores them on return", async () => {
  mockDraftArtifict("");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  pinCommentOnSheet("Draft test · v2");
  fireEvent.change(await screen.findByLabelText("Comment text"), { target: { value: "Version two note" } });
  openTab("Details");
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"version":2}' } });
  pickVersion(1);
  await screen.findByText("First version");
  // The comment being written belongs to v2, so v1 has no composer open.
  expect(screen.queryByLabelText("Comment text")).toBeNull();
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe("");
  pickVersion(2);
  await waitFor(() => expect((screen.getByLabelText("Comment text") as HTMLTextAreaElement).value).toBe("Version two note"));
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe('{"version":2}');
});

it("has no second chat: nothing to type a request into, and no send bar until something is queued", async () => {
  mockDraftArtifict("");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  expect(screen.queryByLabelText("Follow up for agent")).toBeNull();
  expect(screen.queryByText("Ask for…")).toBeNull();
  expect(screen.queryByRole("region", { name: "Send to the agent" })).toBeNull();
  openTab("Details");
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"a":1}' } });
  expect(screen.getByRole("region", { name: "Send to the agent" }).textContent).toContain("State attached");
});

it("keeps the record's notes in cards that float over the canvas, closed until asked for", async () => {
  const annotated = { ...draftArtifict, versions: draftVersions.map((v) => ({ ...v, decisions: ["Keep axes consistent"], constraints: ["No external assets"], codeRefs: [{ path: "src/chart.ts", startLine: 12, description: "Chart entry" }] })) };
  invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [annotated] : structuredClone(annotated));
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  // No column takes the design's width until a card is opened.
  expect(screen.queryByRole("region", { name: "Review" })).toBeNull();
  openTab("Comments");
  expect(screen.getByRole("region", { name: "Comments on this version" }).textContent).toContain("click anything in the design");
  expect(screen.queryByRole("complementary", { name: "Artifict conversation" })).toBeNull();
  openTab("Details");
  expect(screen.queryByRole("region", { name: "Comments on this version" })).toBeNull();
  expect(screen.getByText("Keep axes consistent")).toBeTruthy();
  expect(screen.getByText("No external assets")).toBeTruthy();
  expect(screen.getByText("src/chart.ts:12")).toBeTruthy();
  // The set is reachable from the header and returns to the record it opened.
  fireEvent.click(screen.getByRole("button", { name: "All artificts" }));
  expect(screen.getAllByText("Draft test").length).toBeGreaterThan(0);
  fireEvent.click(screen.getByRole("button", { name: "Back to artifict" }));
  expect(screen.getByLabelText("Exact artifict version")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Close panel" }));
  expect(screen.queryByRole("region", { name: "Review" })).toBeNull();
});

it("opens the real new-chat screen in Artificts mode, about the artifict on screen", async () => {
  const { get: getStore, set: setStore } = await import("../store");
  setStore({ view: "session", task: "task-1", tasks: { "task-1": { id: "task-1", project: "D:/proj" } as never } });
  mockDraftArtifict("");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  fireEvent.click(screen.getByRole("button", { name: "New chat about this artifict" }));
  // No card of its own: the new-task screen, its composer and model picker,
  // with this revision beside it through the chat it came from.
  expect(getStore().view).toBe("home");
  expect(getStore().home.artificts).toBe(true);
  expect(getStore().home.studio).toEqual({ taskId: "task-1", artifictId: "draft-art", versionId: "draft-v2", title: "Draft test", number: 2, kind: "markdown" });
  expect(invoke.mock.calls.some(([c]) => c === "task_create")).toBe(false);
  setStore((s) => ({ home: { ...s.home, artificts: false, studio: null } }));
});

it("borrowed onto the new-task screen, requests start a new chat instead of writing to the old one", async () => {
  mockDraftArtifict("");
  const { get: getStore, set: setStore } = await import("../store");
  const base = invoke.getMockImplementation()!;
  invoke.mockImplementation(async (command: string, ...rest: unknown[]) => {
    if (command === "task_create") throw new Error("stop here");
    return base(command, ...rest);
  });
  setStore((s) => ({ settings: { ...(s.settings ?? {}), project: "D:/proj" } as never }));
  render(<ArtifictWorkspace taskId="task-1" taskTitle="New chat" browseOnly />);
  await screen.findByText("Second version");
  // The whole editor is there, comments and requests included.
  expect(screen.getByRole("radio", { name: "Comment" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Comments" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "New chat about this artifict" })).toBeTruthy();
  openTab("Ask the agent");
  fireEvent.click(screen.getByText("Review this design"));
  await waitFor(() => expect(invoke.mock.calls.some(([c]) => c === "task_create")).toBe(true));
  expect(invoke.mock.calls.some(([c]) => c === "artifict_feedback_submit")).toBe(false);
  expect(getStore().home.studio?.artifictId).toBe("draft-art");
  setStore((s) => ({ home: { ...s.home, artificts: false, studio: null }, draft: "" }));
});

it("can open on the gallery of the project's artificts", async () => {
  mockDraftArtifict("");
  render(<ArtifictWorkspace taskId="task-1" taskTitle="New chat" browseOnly startInGallery />);
  expect(await screen.findByLabelText("Filter artificts")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Open Draft test" })).toBeTruthy();
});

it("shows no work-in-progress banner in the studio", async () => {
  const record = { schema: "openleash.collaboration.v1", title: "Architecture", purpose: "Explain real code", stage: "prototype", nodes: [], edges: [], evidence: [], limitations: [] };
  const collaborationVersions = [{ id: "w1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: JSON.stringify(record), decisions: [], constraints: [], codeRefs: [] }];
  const collaboration = { id: "wip-1", title: "Architecture", kind: "json", currentVersionId: "w1", createdAt: collaborationVersions[0]!.createdAt, updatedAt: collaborationVersions[0]!.createdAt, versions: collaborationVersions, annotations: [], feedback: [] };
  invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [{ id: collaboration.id, title: collaboration.title, kind: collaboration.kind, currentVersionId: collaboration.currentVersionId, createdAt: collaboration.createdAt, updatedAt: collaboration.updatedAt }] : structuredClone(collaboration));
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Explain real code");
  expect(screen.queryByText(/Work in progress/)).toBeNull();
  expect(screen.queryByText("Draft · prototype")).toBeNull();
});

it("uses the successful mutation reply and clears only submitted fields", async () => {
  let resolve!: (value: unknown) => void;
  invoke.mockImplementation(async (command: string) => {
    if (command === "artifict_feedback_submit") return new Promise((done) => { resolve = done; });
    return command === "artifict_list" ? [draftArtifict] : structuredClone(draftArtifict);
  });
  render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
  await screen.findByText("Second version");
  openTab("Details");
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"sort":"cost"}' } });
  fireEvent.click(screen.getByRole("button", { name: "Send to agent" }));
  const picker = screen.getByLabelText("Exact artifict version");
  expect(picker.getAttribute("aria-disabled")).toBe("true");
  // Disabled means it does not open, not merely that it looks dimmed.
  fireEvent.click(picker);
  expect(screen.queryByRole("listbox")).toBeNull();
  fireEvent.change(screen.getByLabelText("Interactive state JSON"), { target: { value: '{"sort":"name"}' } });
  resolve({ ...draftArtifict, feedback: [{ id: "feedback-new", versionId: "draft-v2", text: "", annotationIds: [], createdAt: draftVersions[1]!.createdAt, status: "submitted" }] });
  // The agent's state is named on the canvas; the reply itself is in the chat.
  expect(await screen.findByText("Sent — waiting for the agent")).toBeTruthy();
  // A value edited after Send was not what was sent, so it is kept.
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe('{"sort":"name"}');
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
  // The open record runs on the stage; that is the studio, not a card.
  await waitFor(() => expect(invoke.mock.calls.filter(([command]) => command === "visualization_publish")).toHaveLength(1));
  fireEvent.click(screen.getByRole("button", { name: "All artificts" }));
  // Running every card would publish a document per artifact and start that
  // artifact's scripts; only the one the studio is on is rendered.
  await waitFor(() => expect(container.querySelectorAll("iframe.artifict-thumb-frame")).toHaveLength(1));
  expect(screen.getByText("Second")).toBeTruthy();
  expect(invoke.mock.calls.filter(([command]) => command === "visualization_publish")).toHaveLength(2);
});
