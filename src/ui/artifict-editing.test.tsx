// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const { invoke, save } = vi.hoisted(() => ({ invoke: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null), save }));

import { ArtifictWorkspace, ASK_REQUESTS } from "./ArtifictWorkspace";
import { parseBridgeMessage } from "./artifictBridge";
import { buildVisualizationDocument } from "./vizDocument";
import { resetStudioViews } from "./studioViewState";

afterEach(() => { cleanup(); vi.clearAllMocks(); resetStudioViews(); });

const at = "2026-10-08T10:00:00Z";
const version = (id: string, content: string, parentId: string | null, extra: Record<string, unknown> = {}) => ({ id, parentId, createdAt: at, content, decisions: ["Bold headline"], constraints: [], codeRefs: [], ...extra });
const record = (kind: string, versions: ReturnType<typeof version>[], title = "Landing") => ({ id: "art-1", title, kind, currentVersionId: versions.at(-1)!.id, createdAt: at, updatedAt: at, versions, annotations: [] as unknown[], feedback: [] as unknown[] });

/** Answers the studio's IPC from `current`; `extra` can answer a command first. */
function serve(current: () => ReturnType<typeof record>, extra: (command: string, args: Record<string, unknown>) => unknown = () => undefined) {
  invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
    const answer = extra(command, args);
    if (answer !== undefined) return answer;
    const r = current();
    if (command === "artifict_list") return [{ id: r.id, title: r.title, kind: r.kind, currentVersionId: r.currentVersionId, createdAt: at, updatedAt: at }];
    if (command === "visualization_publish") return "doc-1";
    if (command === "visualization_release") return undefined;
    return structuredClone(r);
  });
}
const fromFrame = (frame: HTMLIFrameElement, data: unknown) => act(() => { window.dispatchEvent(new MessageEvent("message", { data, origin: "null", source: frame.contentWindow })); });
const inspect = { type: "openleash:inspect", version: 1, selector: "body > main > h1", label: "heading “Pricing”", text: "Pricing", styles: { color: "rgb(17, 24, 39)", "font-size": "32px", "font-weight": "700", padding: "0px", "background-color": "rgba(0, 0, 0, 0)", "border-radius": "0px" } };

describe("the edit-mode bridge message", () => {
  it("accepts an inspected element within bounds and keeps only the known styles", () => {
    expect(parseBridgeMessage({ ...inspect, styles: { ...inspect.styles, position: "fixed" } })).toEqual({ type: "inspect", pick: { selector: inspect.selector, label: inspect.label, text: "Pricing", styles: inspect.styles } });
    expect(parseBridgeMessage({ ...inspect, text: null })).toEqual({ type: "inspect", pick: expect.objectContaining({ text: null }) });
    expect(parseBridgeMessage({ ...inspect, text: "x".repeat(2001) })).toBeNull();
    expect(parseBridgeMessage({ ...inspect, selector: "" })).toBeNull();
    expect(parseBridgeMessage({ ...inspect, styles: "color:red" })).toBeNull();
  });

  it("ships the edit protocol in the preview document", () => {
    const doc = buildVisualizationDocument("<p>x</p>", {});
    for (const token of ["openleash:inspect", "openleash:edits", "data-openleash-live-edits"]) expect(doc).toContain(token);
    expect(doc.match(/<\/script>/g)).toHaveLength(1);
  });
});

describe("direct edits on an HTML design", () => {
  const source = "<main><h1>Pricing</h1><p>Plans for teams</p></main>";

  it("lets the user drag the panel by its header, and double-click puts it back", async () => {
    serve(() => record("html", [version("v1", source, null)]));
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    await screen.findByTitle("Landing · v1 preview");
    fireEvent.click(screen.getByRole("radio", { name: "Edit" }));
    const panel = screen.getByRole("region", { name: "Review" });
    const head = panel.querySelector(".studio-panel-head")!;
    fireEvent.pointerDown(head, { clientX: 100, clientY: 100, pointerId: 1 });
    fireEvent.pointerMove(head, { clientX: 40, clientY: 130, pointerId: 1 });
    fireEvent.pointerUp(head, { pointerId: 1 });
    expect(panel.style.translate).toBe("-60px 30px");
    // Moves after release must not drag it any further.
    fireEvent.pointerMove(head, { clientX: 0, clientY: 0, pointerId: 1 });
    expect(panel.style.translate).toBe("-60px 30px");
    // Pressing the close button is not a drag.
    fireEvent.pointerDown(screen.getByRole("button", { name: "Close panel" }), { clientX: 0, clientY: 0, pointerId: 2 });
    fireEvent.pointerMove(head, { clientX: 50, clientY: 50, pointerId: 2 });
    expect(panel.style.translate).toBe("-60px 30px");
    fireEvent.doubleClick(head);
    expect(panel.style.translate).toBe("0px 0px");
  });

  it("edits text and style live, and saves them as the user's own revision", async () => {
    let current = record("html", [version("v1", source, null)]);
    serve(() => current, (command, args) => {
      if (command !== "artifict_edit") return undefined;
      current = record("html", [version("v1", source, null), version("v2", args.content as string, "v1", { author: "user", note: args.note })]);
      return structuredClone(current);
    });
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    const frame = await screen.findByTitle("Landing · v1 preview") as HTMLIFrameElement;
    const post = vi.spyOn(frame.contentWindow!, "postMessage");
    fireEvent.click(screen.getByRole("radio", { name: "Edit" }));
    expect(post).toHaveBeenCalledWith({ version: 1, type: "openleash:mode", mode: "edit" }, "*");
    expect(screen.getByRole("region", { name: "Review" }).textContent).toContain("Click any element");
    fromFrame(frame, inspect);
    const text = await screen.findByLabelText("Element text") as HTMLTextAreaElement;
    expect(text.value).toBe("Pricing");
    expect((screen.getByLabelText("Text colour") as HTMLInputElement).value).toBe("#111827");
    fireEvent.change(text, { target: { value: "Simple pricing" } });
    fireEvent.change(screen.getByLabelText("Font size (px)"), { target: { value: "44" } });
    // The preview shows the change; nothing is saved and the agent hears nothing.
    await waitFor(() => expect(post).toHaveBeenCalledWith({ version: 1, type: "openleash:edits", rules: [{ selector: "body > main > h1", styles: { "font-size": "44px" }, text: "Simple pricing" }] }, "*"));
    expect(invoke.mock.calls.some(([c]) => c === "artifict_edit" || c === "artifict_feedback_submit")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Save as v2 · 1" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_edit", expect.objectContaining({
      taskId: "task-1", artifictId: "art-1", parentVersionId: "v1",
      content: expect.stringContaining("<h1>Simple pricing</h1>"),
      note: "Edited on the canvas: 1 text change, 1 style change (heading “Pricing”)",
    })));
    const saved = invoke.mock.calls.find(([c]) => c === "artifict_edit")![1].content as string;
    expect(saved).toContain("body > main > h1 { font-size: 44px !important; }");
    // The studio moves to the new revision, marked as the user's.
    await screen.findByTitle("Landing · v2 preview");
    const picker = screen.getByLabelText("Exact artifict version");
    expect(picker.querySelector(".ddv")?.textContent).toBe("v2");
    expect(picker.querySelector(".ddh")?.textContent).toMatch(/· yours$/);
    fireEvent.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByText(/Your revision — Edited on the canvas/)).toBeTruthy();
  });

  it("refuses to save a text change it cannot place exactly once", async () => {
    const twice = "<title>Pricing</title><main><h1>Pricing</h1></main>";
    serve(() => record("html", [version("v1", twice, null)]));
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    const frame = await screen.findByTitle("Landing · v1 preview") as HTMLIFrameElement;
    fireEvent.click(screen.getByRole("radio", { name: "Edit" }));
    fromFrame(frame, inspect);
    fireEvent.change(await screen.findByLabelText("Element text"), { target: { value: "Plans" } });
    fireEvent.click(screen.getByRole("button", { name: "Save as v2 · 1" }));
    expect((await screen.findByRole("alert")).textContent).toContain("appears 2 times");
    expect(invoke.mock.calls.some(([c]) => c === "artifict_edit")).toBe(false);
  });

  it("only edits on the latest version, and restores an older one as a new revision", async () => {
    const v1 = version("v1", "<h1>One</h1>", null), v2 = version("v2", "<h1>Two</h1>", "v1");
    let current = record("html", [v1, v2]);
    serve(() => current, (command) => {
      if (command !== "artifict_restore") return undefined;
      current = record("html", [v1, v2, version("v3", "<h1>One</h1>", "v2", { author: "user", note: "Restored from v1" })]);
      return structuredClone(current);
    });
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    await screen.findByTitle("Landing · v2 preview");
    // On the tip there is nothing to restore.
    expect(screen.queryByRole("button", { name: /Restore/ })).toBeNull();
    fireEvent.click(screen.getByLabelText("Exact artifict version"));
    fireEvent.click(screen.getAllByRole("option").find((o) => o.querySelector(".dd-option-label")?.textContent === "v1")!);
    await screen.findByTitle("Landing · v1 preview");
    fireEvent.click(screen.getByRole("radio", { name: "Edit" }));
    expect(screen.getByText("Edits build on the latest version, so they are made there.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Restore v1 as v3" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_restore", { taskId: "task-1", artifictId: "art-1", parentVersionId: "v2", fromVersionId: "v1" }));
    await screen.findByTitle("Landing · v3 preview");
  });
});

describe("source, export and requests", () => {
  it("edits a document's source in place and saves it as a new revision", async () => {
    serve(() => record("markdown", [version("v1", "# Plan\n\nShip it.", null)], "Plan"), (command) => command === "artifict_edit" ? record("markdown", [version("v1", "# Plan\n\nShip it.", null), version("v2", "# Plan\n\nShip it on Friday.", "v1", { author: "user", note: "Edited the source in the studio" })], "Plan") : undefined);
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    await screen.findByText("Ship it.");
    // Edit mode is an HTML design's; a document is edited through its source.
    expect(screen.queryByRole("radio", { name: "Edit" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "View source" }));
    fireEvent.click(screen.getByRole("button", { name: "Edit source" }));
    const box = screen.getByLabelText("Edit Plan · v1 source");
    fireEvent.change(box, { target: { value: "# Plan\n\nShip it on Friday." } });
    fireEvent.click(screen.getByRole("button", { name: "Save as new version" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_edit", { taskId: "task-1", artifictId: "art-1", parentVersionId: "v1", content: "# Plan\n\nShip it on Friday.", note: "Edited the source in the studio" }));
  });

  it("exports the exact version to the path the user picks, with the app theme for a page", async () => {
    serve(() => record("html", [version("v1", "<h1>Hi</h1>", null)], "Launch page!"), (command) => command === "artifict_export" ? "C:/out/Launch-page-v1.html" : undefined);
    save.mockResolvedValue("C:/out/Launch-page-v1.html");
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    await screen.findByTitle("Launch page! · v1 preview");
    fireEvent.click(screen.getByRole("button", { name: "Export v1 as HTML page" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith({ defaultPath: "Launch-page-v1.html", filters: [{ name: "HTML page", extensions: ["html"] }] }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_export", expect.objectContaining({ taskId: "task-1", artifictId: "art-1", versionId: "v1", path: "C:/out/Launch-page-v1.html", theme: expect.objectContaining({ bg: expect.any(String), "font-sans": expect.any(String) }) })));
    expect(await screen.findByText("Exported v1 to C:/out/Launch-page-v1.html")).toBeTruthy();
  });

  it("cannot export a picture that is only a link", async () => {
    serve(() => record("image", [version("v1", "https://example.org/shot.png", null)], "Shot"));
    save.mockResolvedValue(null);
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    const button = await screen.findByRole("button", { name: "Only an embedded picture can be exported" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  it("sends a one-click request against the exact revision on screen", async () => {
    serve(() => record("html", [version("v1", "<h1>Hi</h1>", null)]), (command) => command === "artifict_feedback_submit" ? { ...record("html", [version("v1", "<h1>Hi</h1>", null)]), feedback: [{ id: "f1", versionId: "v1", text: "x", annotationIds: [], createdAt: at, status: "submitted" }] } : undefined);
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    await screen.findByTitle("Landing · v1 preview");
    fireEvent.click(screen.getByRole("button", { name: "Ask the agent" }));
    expect(screen.getByRole("button", { name: /Set up a design system/ })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Review this design/ }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_feedback_submit", { taskId: "task-1", artifictId: "art-1", versionId: "v1", text: ASK_REQUESTS.review, annotationIds: [], interactiveState: null }));
  });
});
