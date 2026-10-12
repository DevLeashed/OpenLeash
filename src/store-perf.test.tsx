// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import type { Item } from "./api";
import { bufferDelta, flushDeltas, get, set, subscribe, useStore } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

afterEach(() => { cleanup(); flushDeltas(); });
const item = (id: string, text = ""): Item => ({ id, text, kind: "text", ts: "", data: {} });

describe("store update cost", () => {
  it("does not notify or replace state for unchanged patches", () => {
    const before = get();
    const notify = vi.fn();
    const off = subscribe(notify);
    try {
      set({});
      set({ draft: before.draft });
      set((s) => ({ items: s.items }));
      expect(get()).toBe(before);
      expect(notify).not.toHaveBeenCalled();
    } finally { off(); }
  });

  it("keeps controlled inputs synchronous and selectors fresh", () => {
    set({ draft: "before" });
    function Probe({ field }: { field: "draft" | "toast" }) {
      const value = useStore((s) => s[field]);
      return <span data-testid="value">{value}</span>;
    }
    const view = render(<Probe field="draft" />);
    act(() => { set({ draft: "after" }); });
    expect(view.getByTestId("value").textContent).toBe("after");
    act(() => { set({ toast: "notice" }); });
    view.rerender(<Probe field="toast" />);
    expect(view.getByTestId("value").textContent).toBe("notice");
  });
});

describe("streamed transcript updates", () => {
  it("does not inspect finished history when only the tail is streaming", () => {
    let reads = 0;
    const history = Array.from({ length: 10_000 }, (_, i) => {
      const entry = item(`old-${i}`);
      Object.defineProperty(entry, "id", { get: () => { reads++; return `old-${i}`; } });
      return entry;
    });
    const tail = item("live", "hello");
    set({ items: { perf: [...history, tail] }, subItems: {} });
    bufferDelta("perf", null, "live", " world");
    flushDeltas();
    expect(get().items.perf?.at(-1)?.text).toBe("hello world");
    expect(get().items.perf?.[0]).toBe(history[0]);
    expect(tail.text).toBe("hello");
    expect(reads).toBe(0);
  });

  it("keeps transcript references for missing or empty deltas", () => {
    set({ items: { perf: [item("live")] }, subItems: {} });
    const before = get();
    bufferDelta("perf", null, "missing", "ignored");
    bufferDelta("perf", null, "live", "");
    flushDeltas();
    expect(get()).toBe(before);
  });

  it("coalesces multiple main and subagent chunks without changing other entries", () => {
    const untouched = item("untouched");
    set({ items: { perf: [item("a", "A"), untouched, item("b", "B")] },
      subItems: { perf: { sub: [item("c", "C")], other: [untouched] } } });
    const other = get().subItems.perf?.other;
    bufferDelta("perf", null, "a", "1");
    bufferDelta("perf", null, "a", "2");
    bufferDelta("perf", null, "b", "3");
    bufferDelta("perf", "sub", "c", "4");
    flushDeltas();
    expect(get().items.perf?.map((x) => x.text)).toEqual(["A12", "", "B3"]);
    expect(get().items.perf?.[1]).toBe(untouched);
    expect(get().subItems.perf?.sub?.[0]?.text).toBe("C4");
    expect(get().subItems.perf?.other).toBe(other);
  });
});
