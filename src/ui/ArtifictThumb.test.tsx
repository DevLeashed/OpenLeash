// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));
import { ArtifictThumb } from "./ArtifictThumb";

afterEach(() => { cleanup(); vi.clearAllMocks(); });

describe("an artifact card's picture", () => {
  it("draws a picture artifact directly, and a document artifact as its own first words", () => {
    const picture = render(<ArtifictThumb format="svg" content={'<svg xmlns="http://www.w3.org/2000/svg"><title>D</title></svg>'} title="Diagram" live={false} />);
    expect(picture.container.querySelector("img.artifict-thumb-media")?.getAttribute("src")).toContain("data:image/svg+xml");
    picture.unmount();

    const document = render(<ArtifictThumb format="markdown" content={"# Plan\n\nShip it on Friday."} title="Plan" live={false} />);
    expect(document.container.querySelector(".artifict-doc-text")?.textContent).toContain("Ship it on Friday.");
  });

  it("draws the record you are on as a live miniature of the artifact itself", async () => {
    invoke.mockResolvedValueOnce("doc-1").mockResolvedValue(undefined);
    const { container, unmount } = render(<ArtifictThumb format="html" content="<h1>Hi</h1>" title="Card" live />);
    const frame = await waitFor(() => {
      const found = container.querySelector("iframe.artifict-thumb-frame");
      expect(found?.getAttribute("src")).toContain("doc-1");
      return found!;
    });
    expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
    // The miniature is part of the card's picture, not a thing to interact with.
    expect(frame.getAttribute("aria-hidden")).toBe("true");
    expect(frame.getAttribute("tabindex")).toBe("-1");
    expect(invoke).toHaveBeenCalledWith("visualization_publish", expect.objectContaining({ source: expect.stringContaining("<h1>Hi</h1>") }));

    // The registry is bounded, so a card that goes away gives its slot back.
    unmount();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("visualization_release", { id: "doc-1" }));
  });

  it("publishes nothing for the cards you are not looking at", () => {
    const { container } = render(<ArtifictThumb format="html" content="<h1>Hi</h1>" title="Card" live={false} />);
    expect(container.querySelector("iframe")).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });
});
