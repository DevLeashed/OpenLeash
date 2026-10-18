// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));
vi.mock("./Visualization", () => ({ Visualization: () => <div>Preview</div> }));

import { ArtifictWorkspace } from "./ArtifictWorkspace";
import { ArtifictPinLayer, findQuoteRange, normalizeQuote, pointFromEvent, quoteOf } from "./ArtifictPinLayer";
import { InlineArtifictCard } from "./InlineArtifictCard";
import { set } from "../store";
import type { Item } from "../api";

afterEach(() => { cleanup(); vi.clearAllMocks(); set({ artificts: {} }); });

const v1 = { id: "v1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: "# First", decisions: [], constraints: [], codeRefs: [] };
const v2 = { id: "v2", parentId: "v1", createdAt: "2026-10-08T11:00:00Z", content: "# Second", decisions: [], constraints: [], codeRefs: [] };
const artifict = { id: "art-1", title: "Plan", kind: "markdown", currentVersionId: v1.id, createdAt: v1.createdAt, updatedAt: v1.createdAt, versions: [v1], annotations: [], feedback: [] };

const summary = (a: { id: string; title: string; kind: string; currentVersionId: string; createdAt: string; updatedAt: string }) => ({ id: a.id, title: a.title, kind: a.kind, currentVersionId: a.currentVersionId, createdAt: a.createdAt, updatedAt: a.updatedAt });

/** Count how many times the editor asked for the artifict, so a live reload is
 *  distinguishable from the initial mount and from an unrelated re-render. */
const reads = () => invoke.mock.calls.filter(([command]) => command === "artifict_get").length;
/** The version the studio's picker shows, by its label (`v<number>`). */
const shownVersion = () => screen.getByLabelText("Exact artifict version").querySelector(".ddv")?.textContent;

describe("the artifict editor follows the agent", () => {
  it("follows the tip to a revision the agent writes while it is open", async () => {
    invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [summary(artifict)] : structuredClone(artifict));
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    await screen.findByLabelText("Exact artifict version");
    expect(await screen.findByText("First")).toBeTruthy();

    // The agent revises the artifict mid-turn. The backend publishes ids only,
    // so the editor has to fetch the content rather than render the event; and
    // because the user was reading the tip, the new revision is what they see.
    const revised = { ...artifict, currentVersionId: v2.id, versions: [v1, v2] };
    invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [summary(revised)] : structuredClone(revised));
    act(() => { set({ artificts: { "task-1": { "art-1": v2.id } } }); });

    expect(await screen.findByText("Second")).toBeTruthy();
    const picker = await screen.findByLabelText("Exact artifict version");
    await waitFor(() => expect(shownVersion()).toBe("v2"));
    // The picker gained the new revision rather than replacing history.
    fireEvent.click(picker);
    expect(screen.getAllByRole("option").map((o) => o.querySelector(".dd-option-label")?.textContent)).toEqual(["v1", "v2"]);
  });

  it("opens the agent's new revision even when the user was looking at an older one", async () => {
    const older = { ...artifict, currentVersionId: v2.id, versions: [v1, v2] };
    invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [summary(older)] : structuredClone(older));
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" initialSelection={{ artifictId: "art-1", versionId: "v1" }} />);
    await screen.findByLabelText("Exact artifict version");
    await waitFor(() => expect(shownVersion()).toBe("v1"));

    const v3 = { ...v2, id: "v3", parentId: "v2", content: "# Third" };
    const newer = { ...older, currentVersionId: "v3", versions: [v1, v2, v3] };
    invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [summary(newer)] : structuredClone(newer));
    act(() => { set({ artificts: { "task-1": { "art-1": "v3" } } }); });

    expect(await screen.findByText("Third")).toBeTruthy();
    await waitFor(() => expect(shownVersion()).toBe("v3"));
  });

  it("switches to an artifict the agent just created", async () => {
    const other = { ...artifict, id: "art-2", title: "Fresh", currentVersionId: "n1", versions: [{ ...v1, id: "n1", content: "# Fresh one" }] };
    invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "artifict_list") return [summary(artifict), summary(other)];
      return structuredClone(args?.artifictId === "art-2" ? other : artifict);
    });
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    await screen.findByText("First");
    act(() => { set({ artificts: { "task-1": { "art-2": "n1" } } }); });
    expect(await screen.findByText("Fresh one")).toBeTruthy();
  });

  it("opens the first artifict the agent makes in a studio that had none", async () => {
    const fresh = { ...artifict, id: "art-new", title: "Brand new", currentVersionId: "b1", versions: [{ ...v1, id: "b1", content: "# Brand new one" }] };
    let made = false;
    invoke.mockImplementation(async (command: string) => {
      if (command === "artifict_list") return made ? [summary(fresh)] : [];
      return structuredClone(fresh);
    });
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    expect(await screen.findByText(/No artificts yet/)).toBeTruthy();
    made = true;
    act(() => { set({ artificts: { "task-1": { "art-new": "b1" } } }); });
    expect(await screen.findByText("Brand new one")).toBeTruthy();
  });

  it("ignores an artifict event for another task", async () => {
    invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [summary(artifict)] : structuredClone(artifict));
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    await screen.findByLabelText("Exact artifict version");
    await screen.findByText("First");
    const before = reads();
    act(() => { set({ artificts: { "other-task": { "art-1": v2.id } } }); });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(reads()).toBe(before);
  });
});

describe("comment pins", () => {
  it("turns a click into a percentage of the preview, so a pin survives a resize", () => {
    const box = { getBoundingClientRect: () => ({ left: 100, top: 50, width: 400, height: 200 }) } as unknown as HTMLElement;
    // The middle of the box, and the corners, are what a pin has to round-trip.
    expect(pointFromEvent({ clientX: 300, clientY: 150 }, box)).toEqual({ x: 50, y: 50 });
    expect(pointFromEvent({ clientX: 100, clientY: 50 }, box)).toEqual({ x: 0, y: 0 });
    expect(pointFromEvent({ clientX: 500, clientY: 250 }, box)).toEqual({ x: 100, y: 100 });
    // A dragged-out click outside the box is clamped rather than dropped off it.
    expect(pointFromEvent({ clientX: 900, clientY: 900 }, box)).toEqual({ x: 100, y: 100 });
    // A collapsed box has no meaningful coordinate space to point into.
    expect(pointFromEvent({ clientX: 1, clientY: 1 }, { getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0 }) } as unknown as HTMLElement)).toBeNull();
  });

  it("reports a draft point on click and draws saved comments as numbered pins", async () => {
    const onPoint = vi.fn();
    const onSelect = vi.fn();
    const comments = [
      { id: "a1", version_id: "v1", text: "Tighten this", selector: null, anchor: null, created_at: "2026-10-08T10:00:00Z", shared: false, point: { x: 25, y: 40 }, offset: null, target: null },
      { id: "a2", version_id: "v1", text: "And this", selector: null, anchor: null, created_at: "2026-10-08T10:00:00Z", shared: true, point: { x: 60, y: 10 }, offset: null, target: null },
    ];
    const { container } = render(<ArtifictPinLayer label="Preview" annotations={comments} onPoint={onPoint} onSelect={onSelect} commentMode activeId="a1"><p>body</p></ArtifictPinLayer>);
    const surface = container.firstElementChild as HTMLElement;
    surface.getBoundingClientRect = () => ({ left: 0, top: 0, width: 200, height: 100 }) as DOMRect;
    // A saved pin is a labelled, focusable control, not a decorative dot.
    const pins = screen.getAllByRole("button");
    expect(pins.map((p) => p.textContent)).toEqual(["1", "2"]);
    expect(screen.getByRole("button", { name: /Comment 1: Tighten this/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Comment 2: And this/ }).getAttribute("aria-pressed")).toBe("false");
    expect(screen.getByRole("button", { name: /Comment 1: Tighten this/ }).getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: /Comment 2: And this/ }));
    expect(onSelect).toHaveBeenCalledWith(comments[1]);
    // A click on the surface itself only ever produces an unsaved draft point,
    // named by what was under it when that is something readable.
    fireEvent.click(surface, { clientX: 100, clientY: 50 });
    expect(onPoint).toHaveBeenCalledWith({ x: 50, y: 50 }, "");
    fireEvent.click(screen.getByText("body"), { clientX: 50, clientY: 25 });
    expect(onPoint).toHaveBeenLastCalledWith({ x: 25, y: 25 }, "paragraph “body”");
  });

  it("does not mine points out of a preview in plain reading mode", () => {
    const onPoint = vi.fn();
    const { container } = render(<ArtifictPinLayer label="Preview" annotations={[]} onPoint={onPoint}><p>body</p></ArtifictPinLayer>);
    fireEvent.click(container.firstElementChild as HTMLElement, { clientX: 10, clientY: 10 });
    expect(onPoint).not.toHaveBeenCalled();
  });

  it("removes an unsent comment but never one that was already sent", async () => {
    const sent = { id: "a2", versionId: "v1", text: "Already sent", anchor: null, createdAt: "2026-10-08T10:00:00Z", submitted: true };
    invoke.mockImplementation(async (command: string) => {
      if (command === "artifict_list") return [summary(artifict)];
      if (command === "artifict_annotate_remove") return { ...artifict, annotations: [] };
      return structuredClone(artifict);
    });
    // The sent comment cannot be listed for the agent again or removed — the
    // backend refuses both, and the card must not offer what it will refuse.
    invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [summary(artifict)] : { ...artifict, annotations: [sent] });
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    fireEvent.click(await screen.findByRole("button", { name: "Comments" }));
    await screen.findByText("Already sent");
    // A sent comment is history: no checkbox to send it again, no remove.
    expect(screen.queryByLabelText(/Include comment: Already sent/)).toBeNull();
    expect(screen.queryByRole("button", { name: /Remove comment: Already sent/ })).toBeNull();
  });

  it("removes a comment that has not been sent", async () => {
    const open = { id: "a1", versionId: "v1", text: "Fix the label", anchor: { x: 25, y: 40 }, createdAt: "2026-10-08T10:00:00Z", submitted: false };
    let removed = false;
    invoke.mockImplementation(async (command: string) => {
      if (command === "artifict_list") return [summary(artifict)];
      if (command === "artifict_annotate_remove") { removed = true; return { ...artifict, annotations: [] }; }
      return structuredClone({ ...artifict, annotations: [open] });
    });
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    fireEvent.click(await screen.findByRole("button", { name: "Comments" }));
    await screen.findByLabelText(/Include comment: Fix the label/);
    fireEvent.click(screen.getByRole("button", { name: /Remove comment: Fix the label/ }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_annotate_remove", expect.objectContaining({ taskId: "task-1", artifictId: "art-1", annotationId: "a1" })));
    expect(removed).toBe(true);
  });

  it("saves a pinned comment against the exact revision, and only then shares it", async () => {
    const saved = { id: "a1", versionId: "v1", text: "Move it left", anchor: { target: "heading “First”", x: 25, y: 40 }, createdAt: "2026-10-08T10:00:00Z", submitted: false };
    invoke.mockImplementation(async (command: string) => {
      if (command === "artifict_list") return [summary(artifict)];
      if (command === "artifict_annotate") return { ...artifict, annotations: [saved] };
      if (command === "artifict_feedback_submit") return { ...artifict, annotations: [{ ...saved, submitted: true }], feedback: [{ id: "f1", versionId: "v1", text: "", annotationIds: ["a1"], createdAt: "2026-10-08T10:00:00Z", status: "submitted" }] };
      return structuredClone(artifict);
    });
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    const heading = await screen.findByText("First");
    fireEvent.click(screen.getByRole("radio", { name: "Comment" }));
    const surface = screen.getByLabelText("Plan · v1");
    surface.getBoundingClientRect = () => ({ left: 0, top: 0, width: 200, height: 100 }) as DOMRect;
    fireEvent.click(heading, { clientX: 50, clientY: 40 });
    // The composer opens where the user pointed, naming what they pointed at.
    const composer = await screen.findByRole("dialog", { name: "New comment" });
    expect(composer.textContent).toContain("heading “First”");
    fireEvent.change(screen.getByLabelText("Comment text"), { target: { value: "Move it left" } });
    fireEvent.click(screen.getByRole("button", { name: "Add comment" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_annotate", expect.objectContaining({
      taskId: "task-1", artifictId: "art-1", versionId: "v1", text: "Move it left", anchor: { target: "heading “First”", x: 25, y: 40 },
    })));
    // Saving a comment is not sending a request: nothing reached the agent.
    expect(invoke.mock.calls.some(([command]) => command === "artifict_feedback_submit")).toBe(false);
    // The comment is queued for the next Send, which carries it by default.
    const send = await screen.findByRole("button", { name: "Send to agent · 1" });
    expect(screen.getByText("1 comment attached")).toBeTruthy();
    fireEvent.click(send);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_feedback_submit", expect.objectContaining({ versionId: "v1", annotationIds: ["a1"] })));
  });

  it("holds back a comment the user unticks", async () => {
    const open = { id: "a1", versionId: "v1", text: "Not yet", anchor: { x: 10, y: 10 }, createdAt: "2026-10-08T10:00:00Z", submitted: false };
    invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [summary(artifict)] : structuredClone({ ...artifict, annotations: [open] }));
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    fireEvent.click(await screen.findByRole("button", { name: "Comments" }));
    fireEvent.click(await screen.findByLabelText(/Include comment: Not yet/));
    expect(screen.getByText("1 held back")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Send to agent" }) as HTMLButtonElement).disabled).toBe(true);
    // Ticking it back queues it again.
    fireEvent.click(screen.getByLabelText(/Include comment: Not yet/));
    fireEvent.click(screen.getByRole("button", { name: "Send to agent · 1" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_feedback_submit", expect.objectContaining({ text: "", annotationIds: ["a1"] })));
  });
});

describe("the inline card's round-trip", () => {
  const cardItem = (): Item => ({
    id: "saved-1", kind: "tool", text: "", ts: "2026-10-08T00:00:00Z",
    data: { name: "artifict_create", status: "ok", input: { title: "Plan", kind: "markdown", content: "# Plan" }, artifict_card: { title: "Plan", kind: "markdown", persistence: "project", artifict_id: "art-1", version_id: "v1" } },
  }) as unknown as Item;

  it("asks against the exact saved version and shows the agent's reply", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "artifict_feedback_list") return [];
      if (command === "artifict_feedback_submit") return { ...artifict, feedback: [{ id: "f1", versionId: "v1", text: "Make it shorter", annotationIds: [], createdAt: "2026-10-08T10:00:00Z", status: "submitted", decision: "addressed", response: "Trimmed it." }] };
      return structuredClone(artifict);
    });
    render(<InlineArtifictCard item={cardItem()} taskId="task-1" />);
    fireEvent.click(screen.getByRole("button", { name: "Ask for a change…" }));
    fireEvent.change(screen.getByLabelText("Request about Plan"), { target: { value: "Make it shorter" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_feedback_submit", expect.objectContaining({ taskId: "task-1", artifictId: "art-1", versionId: "v1", text: "Make it shorter" })));
    expect(await screen.findByText("Trimmed it.")).toBeTruthy();
  });

  it("stays a static preview without a task to attach a request to", () => {
    render(<InlineArtifictCard item={cardItem()} onOpenArtifict={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Ask for a change…" })).toBeNull();
    expect(screen.getByText("Open in Artificts")).toBeTruthy();
  });
});

describe("commenting on selected text", () => {
  /** jsdom has no layout: report a selection of `text` inside `node`, ending at
   *  a stated point, the way a browser would after a drag. */
  const select = (node: Node, text: string, at = { right: 120, bottom: 40 }) => {
    const range = { getBoundingClientRect: () => ({ ...at, left: 0, top: 0, width: 0, height: 0 }) };
    vi.spyOn(window, "getSelection").mockReturnValue({ isCollapsed: false, rangeCount: 1, anchorNode: node, focusNode: node, toString: () => text, getRangeAt: () => range } as unknown as Selection);
  };
  const box = (el: HTMLElement) => { el.getBoundingClientRect = () => ({ left: 0, top: 0, width: 200, height: 100 }) as DOMRect; };

  it("normalizes a selection into a bounded quote and reads it back from a target", () => {
    expect(normalizeQuote("  Pick\n  the service   that deploys ")).toBe("Pick the service that deploys");
    expect(normalizeQuote("x".repeat(400))).toHaveLength(280);
    expect(quoteOf("“Pick the service”")).toBe("Pick the service");
    expect(quoteOf("heading “Plan”")).toBeNull();
  });

  it("finds a quote across element boundaries for the highlight", () => {
    const root = document.createElement("div");
    root.innerHTML = "<p>Pick the <b>service</b>\n that deploys</p>";
    const range = findQuoteRange(root, "the service that");
    expect(range?.toString()).toBe("the service\n that");
    expect(findQuoteRange(root, "not there")).toBeNull();
  });

  it("opens the composer on a selection in comment mode, quoting the passage", () => {
    const onQuote = vi.fn(), onPoint = vi.fn();
    const { container } = render(<ArtifictPinLayer label="Sheet" annotations={[]} onPoint={onPoint} onQuote={onQuote} commentMode><p>Pick the service that deploys</p></ArtifictPinLayer>);
    const surface = container.firstElementChild as HTMLElement;
    box(surface);
    const paragraph = screen.getByText("Pick the service that deploys");
    select(paragraph.firstChild!, "the service");
    fireEvent.mouseUp(paragraph);
    expect(onQuote).toHaveBeenCalledWith({ x: 60, y: 40 }, "the service");
    // The click that ends the drag is the same gesture, not a second pin.
    fireEvent.click(paragraph, { clientX: 10, clientY: 10 });
    expect(onPoint).not.toHaveBeenCalled();
    vi.restoreAllMocks();
  });

  it("offers a Comment button beside a selection outside comment mode, and only then quotes it", () => {
    const onQuote = vi.fn();
    const { container } = render(<ArtifictPinLayer label="Sheet" annotations={[]} onQuote={onQuote}><p>Pick the service that deploys</p></ArtifictPinLayer>);
    box(container.firstElementChild as HTMLElement);
    const paragraph = screen.getByText("Pick the service that deploys");
    select(paragraph.firstChild!, "that deploys");
    fireEvent.mouseUp(paragraph);
    // Selecting to copy still just selects: nothing opened yet.
    expect(onQuote).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Comment" }));
    expect(onQuote).toHaveBeenCalledWith({ x: 60, y: 40 }, "that deploys");
    expect(screen.queryByRole("button", { name: "Comment" })).toBeNull();
    vi.restoreAllMocks();
  });

  it("saves a quoted comment with the passage as its target", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "artifict_list") return [summary(artifict)];
      if (command === "artifict_annotate") return { ...artifict, annotations: [] };
      return structuredClone(artifict);
    });
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    const heading = await screen.findByText("First");
    fireEvent.click(screen.getByRole("radio", { name: "Comment" }));
    box(screen.getByLabelText("Plan · v1"));
    select(heading.firstChild!, "First", { right: 50, bottom: 20 });
    fireEvent.mouseUp(heading);
    expect((await screen.findByRole("dialog", { name: "New comment" })).textContent).toContain("“First”");
    fireEvent.change(screen.getByLabelText("Comment text"), { target: { value: "Rename this" } });
    fireEvent.click(screen.getByRole("button", { name: "Add comment" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_annotate", expect.objectContaining({ text: "Rename this", anchor: { target: "“First”", x: 25, y: 20 } })));
    vi.restoreAllMocks();
  });
});
