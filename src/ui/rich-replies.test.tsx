// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { AssistantMarkdown, MAX_VIZ_BYTES, visualizationParts } from "./Visualization";
import { LightboxProvider } from "./primitives/Lightbox";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => id }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
const svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><title>Pipeline</title><text x="5" y="20">Mesh</text><script>alert(1)</script></svg>';
const fence = (source = svg) => `\`\`\`openleash-svg\n${source}\n\`\`\``;
describe("rich replies", () => {
  it("renders complete SVG as an inert zoomable image between paragraphs", () => {
    render(<LightboxProvider><AssistantMarkdown text={`Before\n\n${fence()}\n\nAfter`} /></LightboxProvider>);
    const image = screen.getByRole("button", { name: "Pipeline" });
    expect(image.tagName).toBe("IMG");
    expect(image.getAttribute("src")).toContain("data:image/svg+xml");
    expect(document.querySelector("svg script")).toBeNull();
    expect(document.querySelector("iframe")).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
    expect(screen.getByText("Before")).toBeTruthy(); expect(screen.getByText("After")).toBeTruthy();
    fireEvent.keyDown(image, { key: "Enter" });
    expect(screen.getByRole("dialog")).toBeTruthy();
  });
  it("keeps incomplete, oversized, nested and nonexact SVG fences inert", () => {
    expect(visualizationParts('```openleash-svg\n<svg>').map(p => p.kind)).toEqual(["placeholder"]);
    expect(visualizationParts(fence("x".repeat(MAX_VIZ_BYTES + 1))).map(p => p.kind)).toEqual(["placeholder"]);
    for (const text of ['```svg\n<svg/>\n```', '```openleash-svg extra\n<svg/>\n```', `\`\`\`\`text\n${fence()}\n\`\`\`\``]) expect(visualizationParts(text).every(p => p.kind === "markdown")).toBe(true);
    expect(visualizationParts(Array.from({ length: 9 }, () => fence()).join("\n")).filter(p => p.kind === "diagram")).toHaveLength(8);
  });
  it("reads an unfenced SVG block as the same diagram, because models do forget the fence", () => {
    // Seen in the field: a reply wrote its diagram as a bare `<svg>` block, the
    // fence never arrived, and the chat showed the XML as a wall of text. The
    // block a Markdown parser calls raw HTML is unambiguous when it is one whole
    // SVG document — and it renders by exactly the same inert image path.
    render(<LightboxProvider><AssistantMarkdown text={`Before\n\n${svg}\n\nAfter`} /></LightboxProvider>);
    const image = screen.getByRole("button", { name: "Pipeline" });
    expect(image.tagName).toBe("IMG");
    expect(image.getAttribute("src")).toContain("data:image/svg+xml");
    expect(document.querySelector("svg script")).toBeNull();
    expect(document.querySelector("iframe")).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
    expect(screen.getByText("Before")).toBeTruthy(); expect(screen.getByText("After")).toBeTruthy();
  });
  it("reads a diagram written across blank lines too, which is how they arrive", () => {
    // The shape of the reply that reported this: the model separated the SVG's
    // groups with blank lines. Markdown splits one element into several raw-HTML
    // blocks at each of those, which is why the whole diagram landed in the chat
    // as text — so the scan for the closing tag must not stop at a blank line.
    const spread = svg.replace("<text", "\n\n<text").replace("<script>", "\n\n<script>");
    expect(visualizationParts(`Prose\n\n${spread}\n\nMore prose`).map(p => p.kind)).toEqual(["markdown", "diagram", "markdown"]);
  });
  it("leaves SVG that is not one whole block alone", () => {
    // Everything here is text a reader is meant to read, and the rule has to be
    // narrow enough not to swallow it: markup inside other raw HTML, an element
    // opened mid-sentence, a block that is not a well-formed document, trailing
    // markup after the close, an empty self-closing element, and a block still
    // being written.
    const open = svg.replace("</svg>", "");
    for (const [why, text] of [
      ["nested in other raw html", `<div>\n${svg}\n</div>`],
      ["not the first thing on its line", `Prose first\n${svg}\n`],
      ["continues the paragraph above it", `Look at this:\n<svg xmlns="http://www.w3.org/2000/svg">\n<rect/>\n</svg>\n`],
      ["not a well-formed document", `<svg xmlns="http://www.w3.org/2000/svg">\nhere is why this works\n</svg>`],
      ["interrupted by prose before it closes", `<svg xmlns="http://www.w3.org/2000/svg">\n<rect/>\n\nThat is the shape.\n</svg>`],
      ["trailing markup after the close", `${svg} and a note`],
      ["no closing element at all", "Before\n\n<svg/>\n\nAfter"],
      ["still streaming", `Before\n\n${open}`],
    ] as const) expect(visualizationParts(text).every(p => p.kind === "markdown"), why).toBe(true);
  });
  it("holds unfenced diagrams to the same count and size limits", () => {
    const one = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10"/></svg>';
    const nine = visualizationParts(Array.from({ length: 9 }, () => one).join("\n\n"));
    expect(nine.filter(p => p.kind === "diagram")).toHaveLength(8);
    expect(nine.filter(p => p.kind === "placeholder")).toHaveLength(1);
    expect(visualizationParts(one.replace("<rect", `<rect data-fill="${"x".repeat(MAX_VIZ_BYTES)}"`)).map(p => p.kind)).toEqual(["placeholder"]);
  });
  it("reports diagram display failures while preserving source controls", () => {
    render(<AssistantMarkdown text={fence()} />);
    fireEvent.error(screen.getByRole("button", { name: "Pipeline" }));
    expect(screen.getByRole("status").textContent).toContain("could not be displayed");
    fireEvent.click(screen.getByRole("button", { name: "Source" }));
    expect(screen.getByText(svg)).toBeTruthy();
  });
  it("renders remote pictures with caption, no-referrer, lazy loading and failure text", () => {
    render(<AssistantMarkdown text={'Before\n\n![Shader example](https://example.com/shader.png "Reference picture")\n\nAfter'} />);
    const image = screen.getByRole("button", { name: "Shader example" });
    expect(image.getAttribute("referrerpolicy")).toBe("no-referrer");
    expect(image.getAttribute("loading")).toBe("lazy");
    expect(screen.getByText("Reference picture")).toBeTruthy();
    expect(screen.getByRole("link", { name: "Image source" }).getAttribute("href")).toBe("https://example.com/shader.png");
    fireEvent.load(image); expect(screen.queryByText("Loading image…")).toBeNull();
    fireEvent.error(image); expect(screen.getByRole("status").textContent).toContain("Image unavailable: Shader example");
    fireEvent.click(screen.getByRole("button", { name: "Retry image" }));
    const retried = screen.getByRole("button", { name: "Shader example" });
    expect(retried.getAttribute("src")).toBe("https://example.com/shader.png");
    expect(retried.getAttribute("referrerpolicy")).toBe("no-referrer");
    fireEvent.load(retried);
    expect(screen.queryByText("Loading image…")).toBeNull();
  });
  it("shows an inline data: picture between paragraphs without asking the backend", () => {
    const png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    render(<AssistantMarkdown text={`Here is the plot:\n\n![Plot](${png})\n\nAnd then more prose.`} />);
    expect(screen.getByRole("button", { name: "Plot" }).getAttribute("src")).toBe(png);
    expect(screen.getByText("Here is the plot:")).toBeTruthy();
    expect(screen.getByText("And then more prose.")).toBeTruthy();
    expect(invoke).not.toHaveBeenCalled();
  });
  it("refuses non-image schemes instead of loading them", () => {
    for (const src of ["javascript:alert(1)", "http://example.com/a.png", "data:text/html;base64,PGI+", "data:image/svg+xml;base64,PHN2Zy8+"]) {
      cleanup();
      render(<AssistantMarkdown text={`![Bad](${src})`} />);
      expect(screen.queryByRole("button", { name: "Bad" })).toBeNull();
      expect(screen.getByText("Bad")).toBeTruthy();
    }
    expect(invoke).not.toHaveBeenCalled();
  });
  it("resolves a project-relative picture through the confined backend command, once", async () => {
    const png = "data:image/png;base64,iVBORw0KGgo=";
    invoke.mockImplementation((cmd: string) => cmd === "reply_image" ? Promise.resolve(png) : Promise.reject(new Error(cmd)));
    const { rerender } = render(<AssistantMarkdown text={"Rendered:\n\n![Chart](<out/my chart.png>)\n\nMore"} streaming />);
    const image = await screen.findByRole("button", { name: "Chart" });
    expect(image.getAttribute("src")).toBe(png);
    expect(invoke).toHaveBeenCalledWith("reply_image", expect.objectContaining({ path: "out/my chart.png" }));
    // The reply keeps streaming: the picture stays put and the file is not re-read.
    rerender(<AssistantMarkdown text={"Rendered:\n\n![Chart](<out/my chart.png>)\n\nMore text arriving"} streaming />);
    expect(screen.getByRole("button", { name: "Chart" })).toBe(image);
    expect(invoke.mock.calls.filter(([c]) => c === "reply_image")).toHaveLength(1);
  });
  it("says why a local picture couldn't be shown, and retries for real", async () => {
    invoke.mockImplementation(() => Promise.reject("../x.png is outside this task's working directory"));
    render(<AssistantMarkdown text={"![Escape](../x.png)"} />);
    expect(await screen.findByText(/outside this task's working directory/)).toBeTruthy();
    invoke.mockImplementation(() => Promise.resolve("data:image/png;base64,iVBORw0KGgo="));
    fireEvent.click(screen.getByRole("button", { name: "Retry image" }));
    expect((await screen.findByRole("button", { name: "Escape" })).getAttribute("src")).toContain("data:image/png");
  });
  it("shows an openleash-image fence whose source is a screenshot path, not base64", async () => {
    invoke.mockImplementation((cmd: string, args: { path: string }) => cmd === "reply_image" ? Promise.resolve(`data:image/jpeg;base64,${args.path.length}`) : Promise.reject(new Error(cmd)));
    const fenceText = "Mid-reply:\n\n```openleash-image\n" + JSON.stringify({ title: "New bleb in-game", src: "C:/Temp/bleb.jpg" }) + "\n```\n\nAfter.";
    render(<AssistantMarkdown text={fenceText} />);
    expect(await screen.findByRole("img", { name: "New bleb in-game" })).toBeTruthy();
    expect(invoke).toHaveBeenCalledWith("reply_image", expect.objectContaining({ path: "C:/Temp/bleb.jpg" }));
    expect(screen.getByText("After.")).toBeTruthy();
  });
});
