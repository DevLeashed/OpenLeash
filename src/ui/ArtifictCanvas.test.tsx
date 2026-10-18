// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ArtifictOverview } from "./ArtifictCanvas";

afterEach(cleanup);

const draw = (format: "svg" | "json" | "markdown" | "text" | "image", content: string, label = "Sample") =>
  render(<ArtifictOverview format={format} content={content} label={label} />);

describe("the shared artifict canvas", () => {
  it("labels a canvas with its kind, so a preview is identifiable without the list", () => {
    const { container } = draw("text", "hello");
    expect(screen.getByText("TEXT")).toBeTruthy();
    expect(container.querySelector(".artifict-canvas-bar .artifict-badge")).toBeTruthy();
  });

  it("draws plain text exactly as written instead of interpreting it as Markdown", () => {
    const { container } = draw("text", "# not a heading\n\n| not | a table |");
    expect(container.querySelector(".artifict-text")?.textContent).toBe("# not a heading\n\n| not | a table |");
    // A log or a data fragment must not come back as a formatted document.
    expect(screen.queryByRole("heading")).toBeNull();
  });

  it("draws Markdown as a document", () => {
    draw("markdown", "# Heading\n\nSome prose.");
    expect(screen.getByRole("heading", { name: "Heading" })).toBeTruthy();
  });

  it("pretty-prints generic JSON and still offers the untouched source", () => {
    const { container } = draw("json", '{"b":1,"a":2}');
    expect(container.querySelector(".artifict-code")?.textContent).toBe('{\n  "b": 1,\n  "a": 2\n}');
    fireEvent.click(screen.getByRole("button", { name: "Source" }));
    expect(screen.getByLabelText("Sample source").textContent).toBe('{"b":1,"a":2}');
    // The tabs swap the body: the artifact and its source are never both up.
    expect(container.querySelector(".artifict-code")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    expect(screen.queryByLabelText("Sample source")).toBeNull();
    expect(container.querySelector(".artifict-code")).toBeTruthy();
  });

  it("frames the artifact at a device width on the stage, not by narrowing the frame", () => {
    const { container } = draw("markdown", "# Heading");
    const stage = container.querySelector(".artifict-canvas-stage")!;
    expect(stage.getAttribute("data-viewport")).toBe("full");
    fireEvent.click(screen.getByLabelText("Preview width"));
    fireEvent.click(screen.getByRole("option", { name: "Phone" }));
    expect(stage.getAttribute("data-viewport")).toBe("phone");
    // The artifact is still the artifact, seen the way a phone would see it.
    expect(screen.getByRole("heading", { name: "Heading" })).toBeTruthy();
  });

  it("keeps an unparsable JSON artifict readable as it was written", () => {
    const { container } = draw("json", '{"half":');
    expect(container.querySelector(".artifict-code")?.textContent).toBe('{"half":');
  });

  it("draws an SVG as an inert image rather than a live document", () => {
    const { container } = draw("svg", '<svg xmlns="http://www.w3.org/2000/svg"><title>Diagram</title></svg>', "Diagram");
    const image = container.querySelector("img.artifict-svg") as HTMLImageElement;
    expect(image.getAttribute("src")).toContain("data:image/svg+xml");
    expect(image.getAttribute("alt")).toBe("Diagram");
  });

  it("shows an image artifict, and the explorer when its content is an explanation", () => {
    const { unmount } = draw("image", "https://example.org/plot.png", "Plot");
    expect(screen.getByAltText("Plot")).toBeTruthy();
    unmount();
    draw("image", JSON.stringify({
      title: "Button alignment", src: "https://example.org/a.png",
      marks: [{ x: 10, y: 20, label: "Off by a pixel" }],
    }), "Comparison");
    expect(screen.getByRole("region", { name: "Button alignment" })).toBeTruthy();
    expect(screen.getByText("Off by a pixel")).toBeTruthy();
  });

  it("renders a collaboration record as the explorer, not as raw JSON", () => {
    const record = {
      schema: "openleash.collaboration.v1", title: "Architecture", purpose: "Explain real code", stage: "prototype",
      nodes: [{ id: "ui", title: "UI", description: "Renders", refs: [{ path: "src/app.ts", line: 2 }] }],
      edges: [], evidence: [], limitations: [],
    };
    const { container } = draw("json", JSON.stringify(record), "Record");
    expect(screen.getByRole("region", { name: "Collaboration record" })).toBeTruthy();
    expect(screen.getByText("src/app.ts:2")).toBeTruthy();
    expect(container.querySelector(".artifict-code")).toBeNull();
  });
});
