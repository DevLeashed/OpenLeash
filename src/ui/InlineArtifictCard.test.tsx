// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { Item } from "../api";
import { hasArtifictCardSlot, InlineArtifictCard } from "./InlineArtifictCard";

vi.mock("./ArtifictWorkspace", () => ({
  ArtifictContentPreview: ({ format, content, autoStart }: { format: string; content: string; autoStart?: boolean }) => <div data-format={format} data-auto-start={String(!!autoStart)}>{content}</div>,
}));

afterEach(() => cleanup());

const toolItem = (name: string, status: string, data: Record<string, unknown> = {}) => ({
  id: "tool-1", kind: "tool", text: "", ts: "2026-10-08T00:00:00Z",
  data: { name, status, input: { title: "Quarterly chart", kind: "html", content: "<h1>Sales</h1>" }, ...data },
}) as unknown as Item;

describe("inline artifict cards", () => {
  it("shows a building placeholder without exposing partial input or rendering it", () => {
    const item = toolItem("artifict_preview", "running", { preview_draft: '{"content":"<script>untrusted partial</script>"}' });
    expect(hasArtifictCardSlot(item)).toBe(true);
    render(<InlineArtifictCard item={item} />);
    expect(screen.getAllByRole("region", { name: "Building Quarterly chart" })).toHaveLength(1);
    expect(screen.getByText("Composing the preview…")).toBeTruthy();
    expect(screen.queryByText(/untrusted partial/)).toBeNull();
    expect(screen.queryByText("<h1>Sales</h1>")).toBeNull();
  });

  it("shows completed one-off previews inline and marks them chat-only", () => {
    const item = toolItem("artifict_preview", "ok", { artifict_card: { title: "Quarterly chart", kind: "html", persistence: "session" } });
    render(<InlineArtifictCard item={item} />);
    expect(screen.getByRole("region", { name: "Artifict: Quarterly chart" })).toBeTruthy();
    expect(screen.getByText("One-off · in this chat")).toBeTruthy();
    expect(screen.getByText("<h1>Sales</h1>").getAttribute("data-auto-start")).toBe("true");
    expect(screen.queryByRole("button", { name: /Open .* in Artificts/ })).toBeNull();
  });

  it("opens a durable artifict at the exact saved version", () => {
    const onOpenArtifict = vi.fn();
    const item = toolItem("artifict_revise", "ok", { artifict_card: { title: "Quarterly chart", kind: "markdown", persistence: "project", artifict_id: "artifict-7", version_id: "version-3" } });
    render(<InlineArtifictCard item={item} onOpenArtifict={onOpenArtifict} />);
    expect(screen.getByText("Saved to project · version version-")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open Quarterly chart in Artificts" }));
    expect(onOpenArtifict).toHaveBeenCalledWith({ artifictId: "artifict-7", versionId: "version-3" });
  });

  it("does not render failed or malformed artifict results as cards", () => {
    const failed = toolItem("artifict_preview", "error", { artifict_card: { title: "Do not show", kind: "html", persistence: "session" } });
    const malformed = toolItem("artifict_create", "ok", { artifict_card: { title: "Broken", kind: "html", persistence: "project" } });
    expect(hasArtifictCardSlot(failed)).toBe(false);
    const { rerender } = render(<InlineArtifictCard item={failed} />);
    expect(screen.queryByRole("region", { name: /Artifict:/ })).toBeNull();
    rerender(<InlineArtifictCard item={malformed} />);
    expect(screen.queryByRole("region", { name: /Artifict:/ })).toBeNull();
  });

  it("shows no work-in-progress banner over a card", () => {
    const record = JSON.stringify({ schema: "openleash.collaboration.v1", title: "Architecture", purpose: "Explain real code", stage: "verified", nodes: [], edges: [], evidence: [], limitations: [] });
    const saved = toolItem("artifict_create", "ok", { artifict_card: { title: "Architecture", kind: "json", persistence: "project", artifict_id: "art-1", version_id: "v1" }, input: { title: "Architecture", kind: "json", content: record } });
    const { unmount } = render(<InlineArtifictCard item={saved} onOpenArtifict={vi.fn()} />);
    expect(screen.queryByText(/Work in progress/)).toBeNull();
    unmount();
    render(<InlineArtifictCard item={toolItem("artifict_preview", "ok", { artifict_card: { title: "Quarterly chart", kind: "html", persistence: "session" } })} />);
    expect(screen.queryByText(/Work in progress/)).toBeNull();
  });

  it("treats text and image artificts as first-class previews", () => {
    const text = toolItem("artifict_create", "ok", { artifict_card: { title: "Build log", kind: "text", persistence: "project", artifict_id: "art-9", version_id: "v9" }, input: { title: "Build log", kind: "text", content: "line one" } });
    render(<InlineArtifictCard item={text} onOpenArtifict={vi.fn()} />);
    expect(screen.getByRole("region", { name: "Artifict: Build log" })).toBeTruthy();
    expect(screen.getByText("line one").getAttribute("data-format")).toBe("text");

    cleanup();
    const image = toolItem("artifict_preview", "ok", { artifict_card: { title: "Plot", kind: "image", persistence: "session" }, input: { title: "Plot", kind: "image", content: "https://example.org/plot.png" } });
    render(<InlineArtifictCard item={image} />);
    expect(screen.getByText("https://example.org/plot.png").getAttribute("data-format")).toBe("image");
  });
});

describe("beside an open studio", () => {
  it("collapses a saved artifict to one row, and keeps one-off previews and other chats full", async () => {
    const { set } = await import("../store");
    const saved = toolItem("artifict_revise", "ok", { artifict_card: { title: "Quarterly chart", kind: "html", persistence: "project", artifict_id: "a1", version_id: "v2" } });
    set({ view: "session", task: "t1", tasks: { t1: { id: "t1", artifict_open: true } as never } });
    const onOpen = vi.fn();
    const first = render(<InlineArtifictCard item={saved} taskId="t1" onOpenArtifict={onOpen} />);
    // The design is next door in the studio, so the chat does not draw it again.
    expect(screen.queryByText("<h1>Sales</h1>")).toBeNull();
    expect(screen.getByText("Revised · in the studio")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Show Quarterly chart in the studio" }));
    expect(onOpen).toHaveBeenCalledWith({ artifictId: "a1", versionId: "v2" });
    first.unmount();
    // A one-off preview is not in the studio, so it still shows in full.
    const oneOff = toolItem("artifict_preview", "ok", { artifict_card: { title: "Quarterly chart", kind: "html", persistence: "session" } });
    const second = render(<InlineArtifictCard item={oneOff} taskId="t1" />);
    expect(screen.getByText("<h1>Sales</h1>")).toBeTruthy();
    second.unmount();
    // On the chat view the saved card is the full preview again.
    set({ tasks: { t1: { id: "t1", artifict_open: false } as never } });
    render(<InlineArtifictCard item={saved} taskId="t1" />);
    expect(screen.getByText("<h1>Sales</h1>")).toBeTruthy();
  });
});
