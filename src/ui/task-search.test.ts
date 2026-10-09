import { describe, expect, it } from "vitest";
import type { TaskSummary } from "../api";
import { matchesTaskQuery, taskSearchDetail } from "./task-search";

const task = (extra: Partial<TaskSummary> = {}): TaskSummary => ({
  id: "task-42", title: "Fix search palette", status: "waiting", waiting_kind: "approval",
  project: "D:/work/OpenLeash", cwd: "D:/work/OpenLeash", branch: "feature/search",
  base_branch: "main", model: "anthropic/claude-sonnet", step: "Review changes",
  ...extra,
} as TaskSummary);

describe("task palette search", () => {
  it("matches task metadata as well as the title", () => {
    expect(matchesTaskQuery(task(), "openleash")).toBe(true);
    expect(matchesTaskQuery(task(), "feature/search")).toBe(true);
    expect(matchesTaskQuery(task(), "claude-sonnet")).toBe(true);
    expect(matchesTaskQuery(task(), "task-42")).toBe(true);
    expect(matchesTaskQuery(task(), "approval")).toBe(true);
  });

  it("requires every query word, regardless of order or casing", () => {
    expect(matchesTaskQuery(task(), "SEARCH FEATURE")).toBe(true);
    expect(matchesTaskQuery(task(), "search other")).toBe(false);
    expect(matchesTaskQuery(task(), "   ")).toBe(true);
  });

  it("shows project, non-default branch, and actionable status", () => {
    expect(taskSearchDetail(task())).toBe("D:/work/OpenLeash · feature/search · Needs approval");
    expect(taskSearchDetail(task({ status: "running", waiting_kind: null, branch: "main" })))
      .toBe("D:/work/OpenLeash · Running");
  });
});
