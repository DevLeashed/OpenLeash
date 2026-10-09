// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { createElement } from "react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import type { Bg, Item, TaskSummary } from "../../api";
import { FeedRows } from "../Session";

const task = { id: "t1", model: "m", cwd: "D:/p", subs: [] } as unknown as TaskSummary;
const tool = (id: string, name: string, input: Record<string, unknown>, extra = {}): Item => ({
  id, kind: "tool", text: "", ts: "2026-10-01T00:00:00Z",
  data: { name, status: "ok", input, ...extra },
});
const items = [
  tool("read", "read_file", { path: "src/main.ts" }),
  tool("command", "bash", { command: "npm test", run_in_background: true }, { bg_id: "bg_test" }),
  { id: "thinking", kind: "thinking", text: "Checking the results", ts: "2026-10-01T00:00:00Z" } as Item,
  tool("search", "grep", { pattern: "regression" }),
];
const job = (running: boolean, exit = "exit 0"): Bg => ({
  id: "bg_test", cmd: "npm test", started: "2026-10-01T00:00:00Z",
  last_line: "test result", running, exit: running ? null : exit,
});
const line = () => document.querySelector<HTMLElement>(".toolgroup > .tg-line")!;

afterEach(cleanup);

describe("background command completions in action groups", () => {
  it("folds an exited command together with neighboring actions and retains its result", async () => {
    render(createElement(FeedRows, { items, task, showThinking: false, bgs: [job(false)] }));
    expect(document.querySelectorAll(".toolgroup")).toHaveLength(1);
    expect(line().textContent).toContain("Read main.ts, ran a command, searched for regression");
    expect(line().textContent).not.toContain("Done");
    expect(document.querySelector(".bg-exit")).toBeNull();
    await act(async () => { line().click(); });
    expect(document.querySelectorAll(".tool")).toHaveLength(3);
    expect(document.querySelector(".bg-exit")?.textContent).toBe("exit 0 · test result");
  });

  it("does not split or close an open action fold when a background job exits", async () => {
    const view = render(createElement(FeedRows, { items, task, showThinking: false, bgs: [job(true)] }));
    await act(async () => { line().click(); });
    expect(document.querySelector(".bg-exit")).toBeNull();
    view.rerender(createElement(FeedRows, { items, task, showThinking: false, bgs: [job(false)] }));
    expect(document.querySelectorAll(".toolgroup")).toHaveLength(1);
    expect(line().getAttribute("aria-expanded")).toBe("true");
    expect(document.querySelectorAll(".tool")).toHaveLength(3);
    expect(document.querySelector(".bg-exit")?.textContent).toContain("exit 0");
  });

  it.each(["exit 1", "exit -1", "killed"])("keeps %s visible as a failure on the collapsed action line", async (exit) => {
    render(createElement(FeedRows, { items, task, showThinking: false, bgs: [job(false, exit)] }));
    expect(document.querySelectorAll(".toolgroup")).toHaveLength(1);
    expect(line().textContent).toContain("1 failed");
    await act(async () => { line().click(); });
    expect(document.querySelector(".bg-exit")?.textContent).toContain(exit);
  });

  it("keeps the live tail visible alongside a completed command's failure", () => {
    const live = tool("live", "read_file", { path: "src/next.ts" }, { status: "running" });
    render(createElement(FeedRows, { items: [...items, live], task, showThinking: false, bgs: [job(false, "exit 1")] }));
    expect(document.querySelectorAll(".toolgroup")).toHaveLength(1);
    expect(line().textContent).toContain("ran a command");
    expect(line().textContent).toContain("1 failed");
    expect(document.querySelector(".tg-now")?.textContent).toContain("reading next.ts");
  });

  // Some providers (free routed models especially) emit an empty text block between
  // parallel calls. It renders as nothing, so it must not cut the run in two.
  it.each(["text", "thinking"] as const)("is not split by a blank %s item between calls", (kind) => {
    const blank = (id: string): Item => ({ id, kind, text: "  \n", ts: "2026-10-01T00:00:00Z", data: {} });
    const run = [
      tool("a", "read_file", { path: "a.cs" }), blank("b1"),
      tool("b", "read_file", { path: "b.cs" }), blank("b2"),
      tool("c", "grep", { pattern: "Ui" }),
    ];
    render(createElement(FeedRows, { items: run, task, showThinking: true, bgs: [] }));
    expect(document.querySelectorAll(".toolgroup")).toHaveLength(1);
    expect(line().textContent).toContain("Read 2 files, searched for Ui");
  });

  it("does not claim that an untracked command completed", async () => {
    render(createElement(FeedRows, { items, task, showThinking: false, bgs: [] }));
    expect(line().textContent).toContain("started a command");
    expect(line().textContent).not.toContain("failed");
    await act(async () => { line().click(); });
    expect(document.querySelector(".bg-exit")).toBeNull();
  });
});
