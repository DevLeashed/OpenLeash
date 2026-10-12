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
});
