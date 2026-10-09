// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { Item } from "../api";
import { hasArtifactCardSlot, InlineArtifactCard } from "./InlineArtifactCard";

vi.mock("./ArtifactWorkspace", () => ({
  ArtifactContentPreview: ({ format, content, autoStart }: { format: string; content: string; autoStart?: boolean }) => <div data-format={format} data-auto-start={String(!!autoStart)}>{content}</div>,
}));

afterEach(() => cleanup());

const toolItem = (name: string, status: string, data: Record<string, unknown> = {}) => ({
  id: "tool-1", kind: "tool", text: "", ts: "2026-10-08T00:00:00Z",
  data: { name, status, input: { title: "Quarterly chart", kind: "html", content: "<h1>Sales</h1>" }, ...data },
}) as unknown as Item;

describe("inline artifact cards", () => {
  it("shows a building placeholder without exposing partial input or rendering it", () => {
    const item = toolItem("artifact_preview", "running", { preview_draft: '{"content":"<script>untrusted partial</script>"}' });
    expect(hasArtifactCardSlot(item)).toBe(true);
    render(<InlineArtifactCard item={item} />);
    expect(screen.getAllByRole("region", { name: "Building Quarterly chart" })).toHaveLength(1);
    expect(screen.getByText("Composing the preview…")).toBeTruthy();
    expect(screen.queryByText(/untrusted partial/)).toBeNull();
    expect(screen.queryByText("<h1>Sales</h1>")).toBeNull();
  });

  it("shows completed one-off previews inline and marks them chat-only", () => {
    const item = toolItem("artifact_preview", "ok", { artifact_card: { title: "Quarterly chart", kind: "html", persistence: "session" } });
    render(<InlineArtifactCard item={item} />);
    expect(screen.getByRole("region", { name: "Artifact: Quarterly chart" })).toBeTruthy();
    expect(screen.getByText("One-off · in this chat")).toBeTruthy();
    expect(screen.getByText("<h1>Sales</h1>").getAttribute("data-auto-start")).toBe("true");
    expect(screen.queryByRole("button", { name: /Open .* in Artifacts/ })).toBeNull();
  });

  it("opens a durable artifact at the exact saved version", () => {
    const onOpenArtifact = vi.fn();
    const item = toolItem("artifact_revise", "ok", { artifact_card: { title: "Quarterly chart", kind: "markdown", persistence: "project", artifact_id: "artifact-7", version_id: "version-3" } });
    render(<InlineArtifactCard item={item} onOpenArtifact={onOpenArtifact} />);
    expect(screen.getByText("Saved to project · version version-")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open Quarterly chart in Artifacts" }));
    expect(onOpenArtifact).toHaveBeenCalledWith({ artifactId: "artifact-7", versionId: "version-3" });
  });

  it("does not render failed or malformed artifact results as cards", () => {
    const failed = toolItem("artifact_preview", "error", { artifact_card: { title: "Do not show", kind: "html", persistence: "session" } });
    const malformed = toolItem("artifact_create", "ok", { artifact_card: { title: "Broken", kind: "html", persistence: "project" } });
    expect(hasArtifactCardSlot(failed)).toBe(false);
    const { rerender } = render(<InlineArtifactCard item={failed} />);
    expect(screen.queryByRole("region", { name: /Artifact:/ })).toBeNull();
    rerender(<InlineArtifactCard item={malformed} />);
    expect(screen.queryByRole("region", { name: /Artifact:/ })).toBeNull();
  });
});
