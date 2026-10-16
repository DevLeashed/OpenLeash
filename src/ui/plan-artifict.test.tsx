// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import type { Item, TaskSummary } from "../api";
import type { ArtifictWorkspaceSelection } from "./InlineArtifictCard";
import { FeedRows } from "./Session";

const task = { id: "plans", title: "File the plan", status: "waiting", subs: [], cwd: "D:/p" } as unknown as TaskSummary;

const planItem = (data: Record<string, unknown> = {}): Item => ({
  id: "approval-1", kind: "approval", text: "Ready to implement this plan?", ts: "2026-10-15T00:00:00Z",
  data: {
    kind: "plan", title: "Ready to implement this plan?", detail: "# File the plan\n\nSave it.\n",
    reason: "Approving turns plan mode off.", ...data,
  },
});

describe("a proposed plan is a project artifict, not just a card", () => {
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  it("points at the filed record and opens that exact version", () => {
    const opened: ArtifictWorkspaceSelection[] = [];
    render(<FeedRows items={[planItem({ plan_artifict: { id: "a1", version_id: "v1", title: "File the plan" } })]} task={task} showThinking={false} onOpenArtifict={(s) => opened.push(s)} />);
    // The plan itself is still the thing being approved, verbatim…
    expect(screen.getByText("Plan ready for review")).toBeTruthy();
    expect(screen.getByText(/Save it\./)).toBeTruthy();
    // …and the card names the record it was filed as.
    const filed = screen.getByText(/Filed in this project/);
    expect(filed.textContent).toContain("File the plan");

    fireEvent.click(screen.getByRole("button", { name: "Open in Artificts" }));
    expect(opened).toEqual([{ artifictId: "a1", versionId: "v1" }]);
  });

  it("still renders the plan when it could not be filed, without a dead link", () => {
    render(<FeedRows items={[planItem()]} task={task} showThinking={false} onOpenArtifict={() => { throw new Error("nothing to open"); }} />);
    expect(screen.getByText("Plan ready for review")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Open in Artificts" })).toBeNull();
    expect(screen.queryByText(/Filed in this project/)).toBeNull();
  });
});
