// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it } from "vitest";
import type { Item } from "../../api";
import { WorktreeNotice } from "../Session";

afterEach(() => cleanup());

describe("worktree notice", () => {
  it("keeps its path and branch behind a short expandable summary", () => {
    const it = {
      id: "worktree", kind: "notice", text: "Created worktree",
      data: { level: "worktree", path: "C:/project/worktrees/worker", branch: "agent/worker" }, ts: "",
    } as Item;
    render(createElement(WorktreeNotice, { it }));
    expect(screen.getByText("Created worktree")).toBeTruthy();
    expect(screen.queryByText(/C:\/project/)).toBeNull();
    expect(screen.getByRole("button").getAttribute("aria-expanded")).toBe("false");

    fireEvent.click(screen.getByRole("button"));
    expect(screen.getByRole("button").getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("Path:").textContent).toContain("C:/project/worktrees/worker");
    expect(screen.getByText("Branch:").textContent).toContain("agent/worker");
  });

  it("extracts path and branch from older worktree notices", () => {
    const it = {
      id: "legacy", kind: "notice", text: "Created a separate worker worktree at C:/project/worktrees/old (agent/old).",
      data: { level: "worktree" }, ts: "",
    } as Item;
    render(createElement(WorktreeNotice, { it }));
    fireEvent.click(screen.getByRole("button"));
    expect(screen.getByText("Path:").textContent).toContain("C:/project/worktrees/old");
    expect(screen.getByText("Branch:").textContent).toContain("agent/old");
  });
});
