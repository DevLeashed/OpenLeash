// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));

import { ArtifictWorkspace } from "./ArtifictWorkspace";
import { listenToFrame, parseBridgeMessage } from "./artifictBridge";
import { changedTweaks, parseTweaks, tweakMessages, tweakState } from "./artifictTweaks";
import { artboardLayout } from "./ArtifictStudioStage";
import { buildVisualizationDocument } from "./vizDocument";

afterEach(() => { cleanup(); vi.clearAllMocks(); });

const manifest = `<script type="application/openleash-tweaks+json">[
  {"var":"--accent","label":"Accent","type":"color","value":"#6d5dfc"},
  {"var":"--radius","label":"Corner radius","type":"range","min":0,"max":32,"step":1,"unit":"px","value":12},
  {"var":"--layout","label":"Layout","type":"select","options":["grid","list"],"value":"grid"},
  {"var":"--shadow","label":"Shadows","type":"toggle","on":"0 10px 30px #0004","off":"none","value":true}
]</script>`;
const pick = { type: "openleash:pick", version: 1, selector: "body > main > button:nth-of-type(2)", label: "button “Get started”", x: 40, y: 22.5, ox: 30, oy: 60, vx: 320, vy: 180 };

describe("the frame bridge protocol", () => {
  it("accepts a well-formed pick and refuses anything out of bounds or unknown", () => {
    expect(parseBridgeMessage(pick)).toEqual({ type: "pick", pick: { selector: pick.selector, label: pick.label, x: 40, y: 22.5, ox: 30, oy: 60, vx: 320, vy: 180 } });
    expect(parseBridgeMessage({ ...pick, x: 140 })).toBeNull();
    expect(parseBridgeMessage({ ...pick, vx: Number.POSITIVE_INFINITY })).toBeNull();
    expect(parseBridgeMessage({ ...pick, selector: "x".repeat(301) })).toBeNull();
    expect(parseBridgeMessage({ ...pick, version: 2 })).toBeNull();
    expect(parseBridgeMessage({ type: "openleash:submit", version: 1 })).toBeNull();
    expect(parseBridgeMessage({ type: "openleash:positions", version: 1, positions: Array.from({ length: 257 }, (_, i) => ({ id: `a${i}`, x: 1, y: 1, visible: true })) })).toBeNull();
    expect(parseBridgeMessage({ type: "openleash:positions", version: 1, positions: [{ id: "a1", x: 10, y: 20, visible: true }] })).toEqual({ type: "positions", positions: [{ id: "a1", x: 10, y: 20, visible: true }] });
  });

  it("only listens to its own opaque-origin frame", () => {
    const frame = document.createElement("iframe");
    document.body.appendChild(frame);
    const other = document.createElement("iframe");
    document.body.appendChild(other);
    const onEvent = vi.fn();
    const stop = listenToFrame(frame, onEvent);
    window.dispatchEvent(new MessageEvent("message", { data: pick, origin: "null", source: other.contentWindow }));
    window.dispatchEvent(new MessageEvent("message", { data: pick, origin: "https://example.org", source: frame.contentWindow }));
    expect(onEvent).not.toHaveBeenCalled();
    window.dispatchEvent(new MessageEvent("message", { data: pick, origin: "null", source: frame.contentWindow }));
    expect(onEvent).toHaveBeenCalledTimes(1);
    stop();
    frame.remove(); other.remove();
  });

  it("ships the bridge in every preview document, ahead of the design's own markup", () => {
    const doc = buildVisualizationDocument("<p>design</p>", { bg: "#000" });
    expect(doc.indexOf("openleash:pick")).toBeGreaterThan(0);
    expect(doc.indexOf("openleash:pick")).toBeLessThan(doc.indexOf("<p>design</p>"));
    for (const token of ["openleash:mode", "openleash:anchors", "openleash:positions", "openleash:tweaks", "openleash:tweak", "openleashFeedbackDraft"]) expect(doc).toContain(token);
    // The bridge must not be closable by its own content.
    expect(doc.match(/<\/script>/g)).toHaveLength(1);
  });
});

describe("tweaks", () => {
  it("reads every control kind from the manifest and nothing else", () => {
    const tweaks = parseTweaks(`<main></main>${manifest}`);
    expect(tweaks.map((t) => [t.type, t.name])).toEqual([["color", "--accent"], ["range", "--radius"], ["select", "--layout"], ["toggle", "--shadow"]]);
    expect(parseTweaks("<p>no manifest</p>")).toEqual([]);
    expect(parseTweaks(`<script type="application/openleash-tweaks+json">not json</script>`)).toEqual([]);
    // Bad names, duplicates and unknown kinds are dropped, not guessed at.
    const odd = parseTweaks(`<script type="application/openleash-tweaks+json">{"tweaks":[{"var":"--a b","type":"color"},{"var":"gap","type":"range","min":0,"max":10,"value":40},{"var":"--gap","type":"color"},{"var":"--x","type":"wobble"}]}</script>`);
    expect(odd).toEqual([{ type: "range", name: "--gap", label: "Gap", min: 0, max: 10, step: 0.1, unit: "", value: 10 }]);
    const many = parseTweaks(`<script type="application/openleash-tweaks+json">${JSON.stringify(Array.from({ length: 40 }, (_, i) => ({ var: `--t${i}`, type: "text" })))}</script>`);
    expect(many).toHaveLength(24);
  });

  it("turns moved controls into CSS values for the frame and a request for the agent", () => {
    const tweaks = parseTweaks(manifest);
    const values = { "--radius": 20, "--layout": "list", "--shadow": false };
    expect(changedTweaks(tweaks, values).map((t) => t.name)).toEqual(["--radius", "--layout", "--shadow"]);
    expect(tweakState(tweaks, values)).toEqual({ "--radius": "20px", "--layout": "list", "--shadow": "none" });
    expect(tweakMessages(tweaks, values)).toEqual([
      { name: "--accent", value: "#6d5dfc", raw: "#6d5dfc" },
      { name: "--radius", value: "20px", raw: 20 },
      { name: "--layout", value: "list", attr: "list", raw: "list" },
      { name: "--shadow", value: "none", attr: "false", raw: false },
    ]);
  });
});

describe("device artboards", () => {
  it("fills the pane, or scales a real device size down to fit it", () => {
    expect(artboardLayout("fit", { w: 900, h: 600 })).toEqual({ frameW: 900, frameH: 600, scale: 1 });
    const desktop = artboardLayout("desktop", { w: 680, h: 600 })!;
    expect(desktop.frameW).toBe(1280);
    expect(desktop.scale).toBeCloseTo(640 / 1280);
    expect(Math.round(desktop.frameH * desktop.scale)).toBe(560);
    expect(artboardLayout("phone", { w: 1200, h: 1000 })).toEqual({ frameW: 390, frameH: 844, scale: 1 });
    expect(artboardLayout("phone", { w: 1200, h: 462 })!.scale).toBeCloseTo(422 / 844);
    expect(artboardLayout("tablet", { w: 0, h: 0 })).toBeNull();
  });
});

describe("the studio on an HTML design", () => {
  const v1 = { id: "v1", parentId: null, createdAt: "2026-10-08T10:00:00Z", content: `<main><button>Learn</button><button>Get started</button></main>${manifest}`, decisions: [], constraints: [], codeRefs: [] };
  const design = { id: "art-1", title: "Landing", kind: "html", currentVersionId: "v1", createdAt: v1.createdAt, updatedAt: v1.createdAt, versions: [v1], annotations: [] as unknown[], feedback: [] as unknown[] };
  const mock = (extra: (command: string, args: Record<string, unknown>) => unknown = () => undefined) => invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
    const answer = extra(command, args);
    if (answer !== undefined) return answer;
    if (command === "artifict_list") return [{ id: design.id, title: design.title, kind: design.kind, currentVersionId: "v1", createdAt: v1.createdAt, updatedAt: v1.createdAt }];
    if (command === "visualization_publish") return "doc-1";
    if (command === "visualization_release") return undefined;
    return structuredClone(design);
  });
  const fromFrame = (frame: HTMLIFrameElement, data: unknown) => act(() => { window.dispatchEvent(new MessageEvent("message", { data, origin: "null", source: frame.contentWindow })); });

  it("turns a click picked in comment mode into an element-anchored comment", async () => {
    mock((command) => command === "artifict_annotate" ? { ...design, annotations: [{ id: "a1", versionId: "v1", text: "Make this the hero action", anchor: { selector: pick.selector, target: pick.label, x: 40, y: 22.5, ox: 30, oy: 60 }, createdAt: v1.createdAt, submitted: false }] } : undefined);
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    const frame = await screen.findByTitle("Landing · v1 preview") as HTMLIFrameElement;
    const post = vi.spyOn(frame.contentWindow!, "postMessage");
    fireEvent.click(screen.getByRole("radio", { name: "Comment" }));
    // The frame is told; the host never overlays the frame to steal its clicks.
    expect(post).toHaveBeenCalledWith({ version: 1, type: "openleash:mode", mode: "comment" }, "*");
    expect(screen.getByText(/Click anything to comment/)).toBeTruthy();
    fromFrame(frame, pick);
    const composer = await screen.findByRole("dialog", { name: "New comment" });
    expect(composer.textContent).toContain("button “Get started”");
    fireEvent.change(screen.getByLabelText("Comment text"), { target: { value: "Make this the hero action" } });
    fireEvent.keyDown(screen.getByLabelText("Comment text"), { key: "Enter", ctrlKey: true });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_annotate", expect.objectContaining({
      versionId: "v1", text: "Make this the hero action",
      anchor: { selector: pick.selector, target: pick.label, x: 40, y: 22.5, ox: 30, oy: 60 },
    })));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New comment" })).toBeNull());
    // The saved comment is handed to the frame to be found again and pinned.
    await waitFor(() => expect(post).toHaveBeenCalledWith({ version: 1, type: "openleash:anchors", anchors: [{ id: "a1", selector: pick.selector, x: 40, y: 22.5, ox: 30, oy: 60 }] }, "*"));
    fromFrame(frame, { type: "openleash:positions", version: 1, positions: [{ id: "a1", x: 120, y: 80, visible: true }] });
    const pin = await screen.findByRole("button", { name: "Comment 1: Make this the hero action" });
    // Clicking the pin on the canvas opens the comments card on that comment.
    fireEvent.click(pin);
    expect(screen.getByRole("region", { name: "Comments on this version" }).textContent).toContain("button “Get started”");
    // Esc from inside the frame steps back out of comment mode.
    fromFrame(frame, { type: "openleash:escape", version: 1 });
    expect((screen.getByRole("radio", { name: "Interact" }) as HTMLElement).getAttribute("aria-checked")).toBe("true");
  });

  it("drives the design's declared tweaks live and sends the moved ones as one request", async () => {
    mock((command) => command === "artifict_feedback_submit" ? { ...design, feedback: [{ id: "f1", versionId: "v1", text: "x", annotationIds: [], createdAt: v1.createdAt, status: "submitted" }] } : undefined);
    render(<ArtifictWorkspace taskId="task-1" taskTitle="Chat" />);
    const frame = await screen.findByTitle("Landing · v1 preview") as HTMLIFrameElement;
    const post = vi.spyOn(frame.contentWindow!, "postMessage");
    fireEvent.click(screen.getByRole("button", { name: "Tweaks" }));
    fireEvent.change(screen.getByRole("slider", { name: "Corner radius" }), { target: { value: "20" } });
    await waitFor(() => expect(post).toHaveBeenCalledWith(expect.objectContaining({ type: "openleash:tweaks", values: expect.arrayContaining([{ name: "--radius", value: "20px", raw: 20 }]) }), "*"));
    fireEvent.click(screen.getByRole("radio", { name: "list" }));
    expect(screen.getByText("2 tweaks attached")).toBeTruthy();
    // Moving a knob changed the preview only; the agent has heard nothing.
    expect(invoke.mock.calls.some(([command]) => command === "artifict_feedback_submit")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Apply 2 to design" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_feedback_submit", expect.objectContaining({
      versionId: "v1", interactiveState: { tweaks: { "--radius": "20px", "--layout": "list" } },
    })));
    await waitFor(() => expect(screen.queryByText("2 tweaks attached")).toBeNull());
  });

  it("requires human review before a prototype's proposed state enters a request", async () => {
    mock();
    render(<ArtifictWorkspace taskId="task" taskTitle="Test" />);
    const frame = await screen.findByTitle("Landing · v1 preview") as HTMLIFrameElement;
    fromFrame(frame, { type: "openleash:feedback-draft", version: 1, target: { label: "Save" }, state: { choice: "compact" } });
    fireEvent.click(await screen.findByRole("button", { name: "The prototype proposed a state · review" }));
    const proposal = screen.getAllByRole("status").find((el) => el.textContent?.includes("review before using"))!;
    expect(proposal.textContent).toContain("compact");
    expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe("");
    expect(invoke).not.toHaveBeenCalledWith("artifict_feedback_submit", expect.anything());
    fireEvent.click(screen.getByRole("button", { name: "Use fields" }));
    expect(JSON.parse((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value)).toEqual({ choice: "compact" });
    fireEvent.click(screen.getByRole("button", { name: "Send to agent" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_feedback_submit", expect.objectContaining({ taskId: "task", artifictId: "art-1", versionId: "v1", interactiveState: { choice: "compact" } })));
  });
});
