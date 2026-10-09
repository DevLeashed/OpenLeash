// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import ReactMarkdown from "react-markdown";
import { AssistantMarkdown, buildVisualizationDocument, MAX_VIZ_BYTES, visualizationParts, Visualization } from "./Visualization";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
const source = "<button onclick='this.textContent++'>1</button><script>window.foo=1</script>";
const fence = (s = source) => `\`\`\`openleash-viz\n${s}\n\`\`\``;
describe("interactive visualizations", () => {
  it("requires exact fences and hides exact-tagged source when bounds reject execution", () => {
    for (const text of ["```html\n<script>1</script>\n```", "```openleash-viz extra\na\n```", "```openleash-viz \na\n```", "```js\n" + fence() + "\n```"]) expect(visualizationParts(text).every(p => p.kind === "markdown")).toBe(true);
    expect(visualizationParts(fence("é".repeat(MAX_VIZ_BYTES))).map(p => p.kind)).toEqual(["placeholder"]);
    expect(visualizationParts("```openleash-viz\n\n```").map(p => p.kind)).toEqual(["placeholder"]);
    expect(visualizationParts("```openleash-viz\n<script>1</script>").map(p => p.kind)).toEqual(["placeholder"]);
    const nine = visualizationParts(Array.from({length: 9}, () => fence()).join("\n"));
    expect(nine.filter(p => p.kind === "visualization")).toHaveLength(8);
    expect(nine.filter(p => p.kind === "placeholder")).toHaveLength(1);
  });
  it("keeps oversized and excess exact-tagged code out of the reply until Source is chosen", async () => {
    const tooLarge = "<script>" + "x".repeat(MAX_VIZ_BYTES) + "</script>";
    const nine = Array.from({length: 9}, (_, i) => fence(`<p>piece-${i}</p>`)).join("\n");
    render(<AssistantMarkdown text={fence(tooLarge)} />);
    expect(screen.queryByText(tooLarge)).toBeNull();
    expect(screen.queryByRole("button", { name: "Preview" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Source" }));
    expect(screen.getByText(tooLarge)).toBeTruthy();
    cleanup(); render(<AssistantMarkdown text={nine} />);
    expect(screen.queryByText("piece-8")).toBeNull();
    expect(screen.queryByRole("button", { name: "Preview" })).toBeNull();
    fireEvent.click(screen.getAllByRole("button", { name: "Source" })[8]!);
    await waitFor(() => expect(screen.getByText("<p>piece-8</p>")).toBeTruthy());
  });
  it("ordinary/user Markdown and raw html never offer a runtime", () => {
    render(<ReactMarkdown>{fence()}</ReactMarkdown>);
    expect(screen.queryByRole("button", { name: "Source" })).toBeNull();
    cleanup(); render(<AssistantMarkdown text={"<script>alert(1)</script>\n```html\na\n```\n```openleash-viz-js\nb\n```\n```openleash-viz extra\nc\n```"} />);
    expect(screen.queryByRole("button", { name: "Source" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Preview" })).toBeNull();
  });
  it("automatically publishes a closed assistant visualization without revealing its source", async () => {
    invoke.mockResolvedValueOnce("one").mockResolvedValue(undefined);
    render(<AssistantMarkdown text={fence()} />);
    await waitFor(() => expect(document.querySelector("iframe")?.getAttribute("src")).toContain("one"));
    expect(invoke).toHaveBeenCalledWith("visualization_publish", expect.objectContaining({ source: expect.stringContaining(source) }));
    expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
    expect(document.querySelector("iframe")?.getAttribute("sandbox")).toBe("allow-scripts");
    expect(document.querySelector("iframe")?.getAttribute("srcdoc")).toBeNull();
    expect(screen.queryByText(source)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Source" }));
    expect(screen.getByText(source)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Hide source" }));
    expect(screen.queryByText(source)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(document.querySelector("iframe")).toBeNull();
    expect(invoke).toHaveBeenCalledWith("visualization_release", { id: "one" });
  });
  it("shows a friendly placeholder only while an exact fence is streaming", () => {
    const text = `before\n\n\`\`\`openleash-viz\n${source}`;
    render(<AssistantMarkdown text={text} streaming />);
    expect(screen.getByRole("status").textContent).toBe("Working on the interactive preview…");
    expect(screen.queryByText(source)).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
    cleanup(); render(<AssistantMarkdown text={text} />);
    expect(screen.getByRole("status").textContent).toBe("This interactive visualization is incomplete.");
    expect(screen.queryByText(source)).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });
  it("does not start an artifact preview until requested; source and stop work", async () => {
    invoke.mockResolvedValueOnce("one").mockResolvedValue(undefined);
    render(<Visualization source={source} />);
    expect(invoke).not.toHaveBeenCalled(); expect(document.querySelector("iframe")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Source" })); expect(screen.getByText(source)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    await waitFor(() => expect(document.querySelector("iframe")?.getAttribute("src")).toContain("one"));
    expect(document.querySelector("iframe")?.getAttribute("sandbox")).toBe("allow-scripts");
    expect(document.querySelector("iframe")?.getAttribute("srcdoc")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Stop" })); expect(document.querySelector("iframe")).toBeNull();
    expect(invoke).toHaveBeenCalledWith("visualization_release", { id: "one" });
  });
  it("expands and collapses the same isolated iframe without publishing or releasing again", async () => {
    invoke.mockResolvedValueOnce("one").mockResolvedValue(undefined);
    render(<Visualization source={source} autoStart />);
    const iframe = await screen.findByTitle("Interactive visualization preview");
    const previewWindow = (iframe as HTMLIFrameElement).contentWindow;
    const previewUrl = iframe.getAttribute("src");
    const expand = screen.getByRole("button", { name: "Expand preview" });
    expand.focus(); fireEvent.click(expand);
    expect(screen.getByRole("dialog", { name: "Interactive visualization preview" })).toBeTruthy();
    expect(document.querySelector("iframe")).toBe(iframe);
    expect((iframe as HTMLIFrameElement).contentWindow).toBe(previewWindow);
    expect(iframe.getAttribute("src")).toBe(previewUrl);
    expect(iframe.getAttribute("sandbox")).toBe("allow-scripts");
    expect(iframe.getAttribute("referrerpolicy")).toBe("no-referrer");
    expect(iframe.getAttribute("srcdoc")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Collapse preview" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.querySelector("iframe")).toBe(iframe);
    expect(document.activeElement).toBe(expand);
    expect(invoke.mock.calls.filter(([command]) => command === "visualization_publish")).toHaveLength(1);
    expect(invoke.mock.calls.filter(([command]) => command === "visualization_release")).toHaveLength(0);
  });
  it("consumes host Escape and restores focus without stopping the preview", async () => {
    invoke.mockResolvedValueOnce("one").mockResolvedValue(undefined);
    render(<Visualization source={source} autoStart />);
    await screen.findByTitle("Interactive visualization preview");
    const expand = screen.getByRole("button", { name: "Expand preview" });
    expand.focus(); fireEvent.click(expand);
    const globalKey = vi.fn();
    window.addEventListener("keydown", globalKey);
    try {
      const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
      screen.getByRole("button", { name: "Collapse preview" }).dispatchEvent(escape);
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
      expect(escape.defaultPrevented).toBe(true);
      expect(globalKey).not.toHaveBeenCalled();
      expect(document.activeElement).toBe(expand);
      expect(screen.getByTitle("Interactive visualization preview")).toBeTruthy();
    } finally { window.removeEventListener("keydown", globalKey); }
  });
  it("uses meaningful labels in the preview section, title, toolbar and iframe", async () => {
    invoke.mockResolvedValueOnce("one").mockResolvedValue(undefined);
    render(<Visualization source={source} autoStart label="Revenue chart · v2" />);
    expect(screen.getByRole("region", { name: "Revenue chart · v2" })).toBeTruthy();
    expect(screen.getByText("Revenue chart · v2")).toBeTruthy();
    expect(screen.getByRole("toolbar", { name: "Revenue chart · v2 controls" })).toBeTruthy();
    await screen.findByTitle("Revenue chart · v2 preview");
    fireEvent.click(screen.getByRole("button", { name: "Expand preview" }));
    expect(screen.getByRole("dialog", { name: "Revenue chart · v2 preview" })).toBeTruthy();
    expect(screen.queryByText("Interactive visualization")).toBeNull();
  });
  it("retains source-change, pending publication and unmount cleanup while expanded", async () => {
    let resolve!: (id: string) => void;
    let publications = 0;
    invoke.mockImplementation((command: string) => {
      if (command !== "visualization_publish") return Promise.resolve();
      if (++publications === 1) return Promise.resolve("one");
      return new Promise<string>(r => { resolve = r; });
    });
    const view = render(<Visualization source={source} autoStart />);
    await screen.findByTitle("Interactive visualization preview");
    fireEvent.click(screen.getByRole("button", { name: "Expand preview" }));
    view.rerender(<Visualization source="<p>updated</p>" autoStart />);
    expect(invoke).toHaveBeenCalledWith("visualization_release", { id: "one" });
    fireEvent.click(screen.getByRole("button", { name: "Collapse preview" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    view.unmount(); resolve("late");
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("visualization_release", { id: "late" }));
    expect(invoke.mock.calls.filter(([command]) => command === "visualization_publish")).toHaveLength(2);
    expect(invoke.mock.calls.filter(([command]) => command === "visualization_release")).toEqual([
      ["visualization_release", { id: "one" }], ["visualization_release", { id: "late" }],
    ]);
  });
  it("keeps keyboard focus in the host viewer and removes the trap on collapse", async () => {
    invoke.mockResolvedValueOnce("one").mockResolvedValue(undefined);
    render(<><button>Outside preview</button><Visualization source={source} autoStart /></>);
    const iframe = await screen.findByTitle("Interactive visualization preview");
    const expand = screen.getByRole("button", { name: "Expand preview" });
    fireEvent.click(expand);
    expect(document.activeElement).toBe(expand);
    fireEvent.keyDown(expand, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(iframe);
    fireEvent.keyDown(iframe, { key: "Tab" });
    expect(document.activeElement).toBe(expand);
    screen.getByRole("button", { name: "Outside preview" }).focus();
    expect(document.activeElement).toBe(expand);
    fireEvent.click(screen.getByRole("button", { name: "Collapse preview" }));
    screen.getByRole("button", { name: "Outside preview" }).focus();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Outside preview" }));
  });
  it("stops an expanded preview and releases its token exactly once", async () => {
    invoke.mockResolvedValueOnce("one").mockResolvedValue(undefined);
    const view = render(<Visualization source={source} autoStart />);
    await screen.findByTitle("Interactive visualization preview");
    fireEvent.click(screen.getByRole("button", { name: "Expand preview" }));
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.querySelector("iframe")).toBeNull();
    view.unmount();
    expect(invoke.mock.calls.filter(([command]) => command === "visualization_release")).toEqual([["visualization_release", { id: "one" }]]);
  });
  it("releases pending publication after unmount", async () => {
    let resolve!: (id: string) => void; invoke.mockImplementationOnce(() => new Promise<string>(r => { resolve = r; })).mockResolvedValue(undefined);
    const view = render(<Visualization autoStart source={source} />); view.unmount(); resolve("late");
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("visualization_release", {id: "late"}));
  });
  it("document builder preserves interactive code and provides a draft-only feedback hook", () => {
    const doc = buildVisualizationDocument(source, {bg: "#fff;}</style><script>bad</script>", fg: "black"});
    expect(doc).toContain(source); expect(doc).toContain("--fg:black;"); expect(doc).not.toContain("</style><script>bad");
    expect(doc.indexOf("--fg")).toBeLessThan(doc.indexOf(source));
    expect(doc).toContain("openleashFeedbackDraft"); expect(doc).toContain("openleash:feedback-draft");
    expect(doc).toContain("data-openleash-target"); expect(doc).toContain("parent.postMessage");
  });
  it("uses compact themed controls in the preview toolbar", () => {
    render(<Visualization source={source} />);
    expect((screen.getByRole("button", { name: "Preview" }) as HTMLButtonElement).style.height).toBe("27px");
    expect((screen.getByRole("button", { name: "Source" }) as HTMLButtonElement).style.fontSize).toBe("11.5px");
    expect((screen.getByRole("button", { name: "Copy source" }) as HTMLButtonElement).style.height).toBe("27px");
  });
  it("does not offer manual artifact creation from a preview", () => {
    render(<Visualization source={source} />);
    expect(screen.queryByRole("button", { name: "Save to project workspace" })).toBeNull();
  });
});
