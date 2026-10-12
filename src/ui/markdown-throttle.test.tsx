// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import type { ComponentProps } from "react";
import type ReactMarkdown from "react-markdown";

const { parse } = vi.hoisted(() => ({ parse: vi.fn() }));
vi.mock("react-markdown", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react-markdown")>();
  return { ...actual, default: (props: ComponentProps<typeof ReactMarkdown>) => {
    parse(props.children);
    return actual.default(props);
  } };
});
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));

import type { Item, TaskSummary } from "../api";
import { FeedRows } from "./Session";

const task = { id: "markdown-test", status: "running", subs: [], cwd: "D:/temporary" } as unknown as TaskSummary;
const item = (text: string, interactive = true): Item => ({
  id: "reply", kind: interactive ? "text" : "user", text,
  ts: "2026-10-12T00:00:00Z", data: interactive ? {} : { brief: true },
});

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-12T00:00:00Z"));
  parse.mockClear();
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("transcript markdown throttling", () => {
  it.each([true, false])("only parses published text (interactive=%s)", (interactive) => {
    const rows = (text: string) => <FeedRows items={[item(text, interactive)]} task={task} showThinking={false} streamingItemId="reply" />;
    const view = render(rows("**Hello**"));
    expect(parse).toHaveBeenCalledTimes(1);
    expect(view.container.querySelector(".md strong")?.textContent).toBe("Hello");

    // The first update publishes immediately; subsequent deltas share its
    // 120ms window. Count actual react-markdown invocations, not row renders.
    view.rerender(rows("**Hello** world"));
    expect(parse).toHaveBeenCalledTimes(2);
    for (const suffix of ["!", "! More", "! More text"]) {
      act(() => { vi.advanceTimersByTime(20); });
      view.rerender(rows("**Hello** world" + suffix));
      expect(parse).toHaveBeenCalledTimes(2);
      expect(view.container.querySelector(".md")?.textContent).toBe("Hello world");
    }
    act(() => { vi.advanceTimersByTime(59); });
    expect(parse).toHaveBeenCalledTimes(2);
    act(() => { vi.advanceTimersByTime(1); });
    expect(parse).toHaveBeenCalledTimes(3);
    expect(parse).toHaveBeenLastCalledWith("**Hello** world! More text");
    expect(view.container.querySelector(".md")?.textContent).toBe("Hello world! More text");
    act(() => { vi.advanceTimersByTime(1000); });
    expect(parse).toHaveBeenCalledTimes(3);
  });

  it("updates incomplete visualization messaging when streaming ends without changing text", () => {
    const text = "before\n\n```openleash-viz\n<p>unfinished</p>";
    const rows = (streaming: boolean) => <FeedRows items={[item(text)]} task={task} showThinking={false} streamingItemId={streaming ? "reply" : undefined} />;
    const view = render(rows(true));
    expect(view.container.querySelector('[role="status"]')?.textContent).toBe("Working on the interactive preview…");
    view.rerender(rows(false));
    expect(view.container.querySelector('[role="status"]')?.textContent).toBe("This interactive visualization is incomplete.");
  });
});
