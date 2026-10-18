// @vitest-environment jsdom
// The transcript re-rendered every visible row on every chat event: the summary
// is a new object on each status, step, usage and subagent update, and every
// row was handed it and compared it by identity. These pin the comparators that
// replaced that, and the two places a fresh object per pass defeated a memo
// anyway (the swarm tree's nodes, a streaming reply's markdown).
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
const { strip } = vi.hoisted(() => ({ strip: vi.fn() }));
vi.mock("./SelectionNote", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./SelectionNote")>();
  return { ...actual, stripHarnessBlocks: (...args: Parameters<typeof actual.stripHarnessBlocks>) => { strip(); return actual.stripHarnessBlocks(...args); } };
});
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import type { Item, SubInfo, TaskSummary } from "../api";
import { buildTree, FeedRows, feedTaskEqual, sameItems } from "./Session";

const task = (extra: Partial<TaskSummary> = {}): TaskSummary =>
  ({ id: "t1", title: "chat", status: "done", model: "m", cwd: "D:/p", subs: [], pending: {}, usage: { cost: 0 }, step: "", ...extra }) as unknown as TaskSummary;
const user = (id: string, text: string): Item => ({ id, kind: "user", text, data: {}, ts: "2026-10-12T00:00:00Z" });
const reply = (id: string, text: string): Item => ({ id, kind: "text", text, data: {}, ts: "2026-10-12T00:00:01Z" });
const subItem = (id: string, subId: string): Item => ({ id, kind: "sub", text: "work", data: { sub_id: subId, status: "running", name: "w" }, ts: "2026-10-12T00:00:01Z" });
const sub = (id: string, extra: Partial<SubInfo> = {}): SubInfo =>
  ({ id, role: id, task: `work on ${id}`, status: "running", meta: "", model: "", serving: "", started: null, report: "", background: false, ...extra }) as SubInfo;

beforeEach(() => { parse.mockClear(); strip.mockClear(); });
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("what a transcript row compares on the chat summary", () => {
  it("ignores the fields no row shows", () => {
    const a = task();
    expect(feedTaskEqual(a, { ...a, step: "Reading foo.rs", usage: { cost: 3 } } as TaskSummary, [user("u", "hi")])).toBe(true);
  });

  it("sees the fields rows do show", () => {
    const a = task();
    const items = [user("u", "hi")];
    expect(feedTaskEqual(a, { ...a, status: "running" }, items), "working hides rewind").toBe(false);
    expect(feedTaskEqual(a, { ...a, cwd: "D:/q" }, items), "paths are relative to it").toBe(false);
    expect(feedTaskEqual(a, { ...a, paused: { reason: "x", kind: "manual", since: "" } }, items), "the freeze glyphs").toBe(false);
    expect(feedTaskEqual(a, { ...a, id: "t2" }, items)).toBe(false);
  });

  it("looks at a subagent's model and step only when a pill for it is in the rows", () => {
    const a = task({ subs: [sub("s1")] });
    const b = task({ subs: [sub("s1", { meta: "step 2" })] });
    expect(feedTaskEqual(a, b, [user("u", "hi")])).toBe(true);
    expect(feedTaskEqual(a, b, [subItem("p", "s1")])).toBe(false);
    // The pill's colour is the chat's model for an agent without its own.
    expect(feedTaskEqual(a, task({ subs: [sub("s1")], model: "other" }), [subItem("p", "s1")])).toBe(false);
    expect(feedTaskEqual(a, task({ subs: [sub("s1")] }), [subItem("p", "s1")]), "a republished, identical list").toBe(true);
  });

  it("compares item lists call by call", () => {
    const x = user("u", "hi"), y = reply("r", "yo");
    expect(sameItems([x, y], [x, y])).toBe(true);
    expect(sameItems([x, y], [x, { ...y }])).toBe(false);
    expect(sameItems([x], [x, y])).toBe(false);
  });
});

describe("the transcript under chat events", () => {
  it("does not re-render a user message for a step, cost or status event it does not show", () => {
    const items = [user("u", "hello"), reply("r", "answer")];
    const t = task();
    const view = render(<FeedRows items={items} task={t} showThinking={false} bgs={[]} />);
    const bubbles = strip.mock.calls.length;
    expect(bubbles).toBeGreaterThan(0);
    const parsed = parse.mock.calls.length;
    // The summary is a new object on every event, and `Feed` slices new arrays.
    view.rerender(<FeedRows items={[...items]} task={{ ...t, step: "Reading", usage: { cost: 2 } } as TaskSummary} showThinking={false} bgs={[]} />);
    view.rerender(<FeedRows items={[...items]} task={{ ...t, status: "stopped" }} showThinking={false} bgs={[]} />);
    expect(strip.mock.calls.length).toBe(bubbles);
    expect(parse.mock.calls.length).toBe(parsed);
    // A change the bubble does show still reaches it: working hides rewind.
    expect(view.container.querySelector(".rw")).toBeTruthy();
    view.rerender(<FeedRows items={[...items]} task={{ ...t, status: "running" }} showThinking={false} bgs={[]} />);
    expect(view.container.querySelector(".rw")).toBeNull();
  });

  it("re-renders only the streaming reply as it grows", () => {
    vi.useFakeTimers();
    const t = task({ status: "running" });
    const u = user("u", "hello");
    const rows = (text: string) => <FeedRows items={[u, reply("r", text)]} task={t} showThinking={false} bgs={[]} streamingItemId="r" />;
    const view = render(rows("first"));
    const bubbles = strip.mock.calls.length;
    for (const text of ["first block", "first block\n\nsecond", "first block\n\nsecond block\n\nthird"]) {
      act(() => { vi.advanceTimersByTime(500); });
      view.rerender(rows(text));
      act(() => { vi.advanceTimersByTime(500); });
    }
    expect(strip.mock.calls.length, "the user's bubble is left alone").toBe(bubbles);
    expect(view.container.querySelector(".md")?.textContent?.replace(/\s+/g, "")).toBe("firstblocksecondblockthird");
  });
});

describe("a streaming reply's markdown", () => {
  it("parses only its growing tail once earlier blocks are finished", () => {
    vi.useFakeTimers();
    const t = task({ status: "running" });
    const rows = (text: string) => <FeedRows items={[reply("r", text)]} task={t} showThinking={false} bgs={[]} streamingItemId="r" />;
    const head = "# Heading\n\nA long first paragraph.\n\n";
    const view = render(rows(head + "tail\n"));
    act(() => { vi.advanceTimersByTime(500); });
    parse.mockClear();
    for (const more of ["grows\n", "grows\nmore\n", "grows\nmore\nstill"]) {
      view.rerender(rows(head + "tail\n" + more));
      act(() => { vi.advanceTimersByTime(500); });
    }
    // Every parse after the first blocks closed was of the tail alone.
    expect(parse.mock.calls.length).toBeGreaterThan(0);
    for (const [source] of parse.mock.calls) expect(String(source).startsWith("tail")).toBe(true);
    expect(view.container.querySelector(".md h1")?.textContent).toBe("Heading");
  });

  it("is parsed whole once it has finished streaming", () => {
    vi.useFakeTimers();
    const t = task({ status: "running" });
    const text = "One.\n\nTwo.\n\nThree.";
    const view = render(<FeedRows items={[reply("r", text)]} task={t} showThinking={false} bgs={[]} streamingItemId="r" />);
    parse.mockClear();
    view.rerender(<FeedRows items={[reply("r", text)]} task={{ ...t, status: "done" }} showThinking={false} bgs={[]} />);
    act(() => { vi.advanceTimersByTime(500); });
    expect(parse).toHaveBeenLastCalledWith(text);
    expect(view.container.querySelectorAll(".md p")).toHaveLength(3);
  });
});

describe("the swarm tree's nodes", () => {
  it("hands back the same node for every agent whose row is unchanged", () => {
    const first = buildTree([sub("a"), sub("b", { parent: "a", depth: 2 }), sub("c")]);
    // The backend republishes the whole list: every `sub` is a new object.
    const second = buildTree([sub("a"), sub("b", { parent: "a", depth: 2, meta: "step 2" }), sub("c")], first.nodes);
    const [a1, c1] = first.live, [a2, c2] = second.live;
    expect(c2, "an untouched agent keeps its node").toBe(c1);
    expect(a2, "a parent whose child moved is redrawn").not.toBe(a1);
    expect(a2!.kids[0]!.sub.meta).toBe("step 2");
    const third = buildTree([sub("a"), sub("b", { parent: "a", depth: 2, meta: "step 2" }), sub("c")], second.nodes);
    expect(third.live[0]).toBe(a2);
    expect(third.live[0]!.kids[0]).toBe(a2!.kids[0]);
  });
});
