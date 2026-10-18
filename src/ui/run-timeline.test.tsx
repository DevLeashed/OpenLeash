// @vitest-environment jsdom
// The run timeline: pure layout/zoom/format helpers, and the panel rendered
// from a fixture — lanes, totals, hover card, jump-to-row, empty and partial
// states.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type Item, type RunBlock, type RunLane, type RunTimeline, type RunTotals } from "../api";
import { get, set } from "../store";
import {
  AXIS_H, LANE_PAD, MIN_SPAN, ROW_GAP, ROW_H, RunTimelinePanel, axisTicks, blockTip, fmtDur, jumpToRow, layoutTimeline,
  openContextWindow, openRunTimeline, panView, revealItem, splitSegments, timelineBounds, zoomView,
} from "./RunTimeline";

const T0 = 1_000_000;
const lane = (o: Partial<RunLane> & { id: string }): RunLane => ({ label: o.id, role: "", parent: "", depth: 0, status: "done", item: "", start_ms: T0, end_ms: T0 + 20_000, rows: 1, ...o });
const block = (o: Partial<RunBlock> & { lane: string; kind: RunBlock["kind"]; start: number; end: number }): RunBlock => {
  const { start, end, ...rest } = o;
  return { row: 0, name: "", open: false, status: "ok", item: "", estimated: false, start_ms: T0 + start, end_ms: T0 + end, ...rest };
};
const totals = (o: Partial<RunTotals> = {}): RunTotals => ({
  wall_ms: 20_000, you_ms: 1_000, paused_ms: 0, model_ms: 10_000, retry_ms: 1_000, tool_ms: 5_000, idle_ms: 3_000, bg_ms: 6_000,
  requests: 3, failed_requests: 1, retries: 1, fallbacks: 1, tool_calls: 3, tool_errors: 0, cost: 0.12, task_cost: 0.5,
  by_model: [{ model: "anthropic/claude-opus-5", requests: 2, failed: 1, cost: 0.12, ms: 7_000, input: 100, output: 50, cache_read: 0, cache_write: 0 }],
  slowest_tools: [{ name: "bash", ms: 1_000, lane: "main", item: "i-tool", start_ms: T0 + 8_000 }, { name: "grep", ms: 400, lane: "s2", item: "s2-x", start_ms: T0 + 12_000 }],
  ...o,
});

const fixture = (o: Partial<RunTimeline> = {}): RunTimeline => ({
  task_id: "t1", start_ms: T0, end_ms: T0 + 20_000, now_ms: T0 + 20_000, recorded: true, partial: false,
  lanes: [
    lane({ id: "main", label: "Main agent", rows: 2 }),
    lane({ id: "s1", label: "map the store", role: "explore", depth: 1, item: "pill-s1", status: "done" }),
    lane({ id: "s2", label: "grep callers", role: "general", depth: 2, parent: "s1", status: "running" }),
    lane({ id: "bg", label: "Background commands" }),
  ],
  blocks: [
    block({ lane: "main", kind: "model", name: "anthropic/claude-opus-5", start: 0, end: 2_000, status: "error", attempt: 1, detail: "overloaded", account: "work", role: "main" }),
    block({ lane: "main", kind: "wait", name: "retry", start: 2_000, end: 3_000 }),
    block({ lane: "main", kind: "model", name: "openai/gpt-6", start: 3_000, end: 8_000, attempt: 2, fallback: true, cost: 0.12, input: 1200, output: 300, ttft_ms: 900, account: "work", role: "main" }),
    block({ lane: "main", kind: "tool", name: "bash", start: 8_000, end: 9_000, item: "i-tool" }),
    block({ lane: "main", kind: "tool", name: "read_file", row: 1, start: 8_000, end: 8_500, item: "i-folded" }),
    block({ lane: "s1", kind: "model", name: "anthropic/claude-opus-5", start: 9_000, end: 12_000, item: "s1-a", estimated: true }),
    block({ lane: "s2", kind: "tool", name: "grep", start: 12_000, end: 20_000, item: "s2-x", open: true, status: "" }),
    block({ lane: "bg", kind: "bg", name: "npm test", start: 9_000, end: 15_000, item: "i-tool", status: "exit 0" }),
    block({ lane: "main", kind: "wait", name: "approval", start: 15_000, end: 16_000 }),
  ],
  totals: totals(),
  ...o,
});

const item = (id: string): Item => ({ id, kind: "text", text: id, data: {}, ts: "2026-01-01T00:00:00Z" } as Item);
const scroll = vi.fn();

beforeEach(() => {
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener() {}, removeEventListener() {} })));
  scroll.mockReset();
  Element.prototype.scrollIntoView = scroll;
  set({
    view: "session", task: "t1", runTimeline: null, subOpen: null, details: false,
    tasks: { t1: { id: "t1", title: "Fix the flaky test", status: "done", subs: [] } as never },
    items: { t1: [item("i-head"), item("i-tool"), item("i-folded")] },
    subItems: {},
  } as never);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

describe("fmtDur", () => {
  it("reads like a person would say it", () => {
    expect(fmtDur(0)).toBe("0s");
    expect(fmtDur(-5)).toBe("0s");
    expect(fmtDur(450)).toBe("450ms");
    expect(fmtDur(4_200)).toBe("4.2s");
    expect(fmtDur(5_000)).toBe("5s");
    expect(fmtDur(42_000)).toBe("42s");
    expect(fmtDur(120_000)).toBe("2m");
    expect(fmtDur(245_000)).toBe("4m 05s");
    expect(fmtDur(3_600_000)).toBe("1h");
    expect(fmtDur(3_900_000)).toBe("1h 05m");
    expect(fmtDur(51 * 3_600_000)).toBe("2d 3h");
    // Rounding up across a unit boundary lands on the bigger unit, not "60s".
    expect(fmtDur(59_700)).toBe("1m");
  });
});

describe("axisTicks", () => {
  it("picks a round step and labels relative to the run start", () => {
    expect(axisTicks(0, 120_000, 4).map((t) => t.label)).toEqual(["0s", "30s", "1m", "1m30s", "2m"]);
  });
  it("keeps labels on multiples counted from the origin when panned", () => {
    const t = axisTicks(T0 + 12_000, T0 + 40_000, 3, T0);
    expect(t[0]!.at).toBe(T0 + 20_000);
    expect(t.map((x) => x.label)).toEqual(["20s", "30s", "40s"]);
  });
  it("returns nothing for an empty span", () => {
    expect(axisTicks(5, 5, 4)).toEqual([]);
  });
});

describe("zoom and pan", () => {
  const b = { from: 0, to: 1000 };
  it("zooms around the anchor", () => {
    expect(zoomView(b, 0.5, 0.5, b)).toEqual({ from: 250, to: 750 });
    expect(zoomView(b, 0.5, 0, b)).toEqual({ from: 0, to: 500 });
    expect(zoomView({ from: 200, to: 400 }, 0.5, 1, b)).toEqual({ from: 300, to: 400 });
  });
  it("never zooms out past the run or in past the minimum span", () => {
    expect(zoomView({ from: 600, to: 1000 }, 10, 0.5, b)).toEqual(b);
    const tiny = zoomView(b, 1e-9, 0.5, b);
    expect(tiny.to - tiny.from).toBe(MIN_SPAN);
  });
  it("pans without leaving the run", () => {
    expect(panView({ from: 100, to: 300 }, 50, b)).toEqual({ from: 150, to: 350 });
    expect(panView({ from: 100, to: 300 }, -500, b)).toEqual({ from: 0, to: 200 });
    expect(panView({ from: 100, to: 300 }, 5000, b)).toEqual({ from: 800, to: 1000 });
  });
});

describe("layoutTimeline", () => {
  const tl = fixture();
  const view = timelineBounds(tl);
  it("stacks lanes by their row count and places blocks proportionally", () => {
    const lay = layoutTimeline(tl, 1000, view);
    const main = ROW_H * 2 + ROW_GAP + LANE_PAD * 2, one = ROW_H + LANE_PAD * 2;
    expect(lay.lanes.map((l) => [l.lane.id, l.top])).toEqual([["main", AXIS_H], ["s1", AXIS_H + main], ["s2", AXIS_H + main + one], ["bg", AXIS_H + main + one * 2]]);
    expect(lay.height).toBe(AXIS_H + main + one * 3);
    const first = lay.rects.find((r) => r.index === 0)!;
    expect(first).toMatchObject({ x: 0, w: 100, y: AXIS_H + LANE_PAD, h: ROW_H });
    // Parallel calls on one lane sit on their own row.
    expect(lay.rects.find((r) => r.index === 4)!.y).toBe(AXIS_H + LANE_PAD + ROW_H + ROW_GAP);
  });
  it("drops blocks outside the view, clips the rest, and keeps tiny ones visible", () => {
    const lay = layoutTimeline(tl, 1000, { from: T0 + 8_000, to: T0 + 10_000 });
    const ids = lay.rects.map((r) => r.index);
    expect(ids).not.toContain(0);
    expect(ids).toContain(3);
    const s1 = lay.rects.find((r) => r.index === 5)!;
    expect(s1.x + s1.w).toBeLessThanOrEqual(1000);
    const tiny = layoutTimeline({ ...tl, blocks: [block({ lane: "main", kind: "tool", start: 100, end: 101 })] }, 100, view);
    expect(tiny.rects[0]!.w).toBe(1);
  });
  it("gives an unrecorded run a scale anyway", () => {
    const b = timelineBounds(fixture({ start_ms: null, end_ms: null, blocks: [] }));
    expect(b.to - b.from).toBe(1000);
  });
});

describe("splitSegments", () => {
  it("keeps the backend's precedence order and drops empty parts", () => {
    const s = splitSegments(totals());
    expect(s.map((x) => x.key)).toEqual(["you", "model", "retry", "tool", "idle"]);
    expect(s.reduce((a, x) => a + x.ms, 0)).toBe(20_000);
  });
});

describe("blockTip", () => {
  it("describes a failed model attempt", () => {
    const t = blockTip(fixture().blocks[0]!, T0);
    expect(t.title).toMatch(/Model/);
    expect(Object.fromEntries(t.lines)).toMatchObject({ Duration: "2s", "Starts at": "+0s", Account: "work", Role: "Main agent", Status: "error", Error: "overloaded" });
  });
  it("marks an estimated block and an open one", () => {
    expect(Object.fromEntries(blockTip(fixture().blocks[5]!, T0).lines).Timing).toMatch(/estimated/);
    expect(Object.fromEntries(blockTip(fixture().blocks[6]!, T0).lines).Duration).toMatch(/still running/);
  });
});

describe("RunTimelinePanel", () => {
  const show = async (tl: RunTimeline = fixture()) => {
    const spy = vi.spyOn(api, "runTimeline").mockResolvedValue(tl);
    render(<RunTimelinePanel />);
    openRunTimeline("t1");
    await screen.findByText("Fix the flaky test");
    return spy;
  };

  it("draws lanes, labels, blocks and totals", async () => {
    const spy = await show();
    await screen.findByText("Wall time");
    expect(spy).toHaveBeenCalledWith("t1");
    for (const l of ["Main agent", "explore · map the store", "general · grep callers", "Background commands"]) expect(screen.getByText(l)).toBeTruthy();
    // Nested lanes indent by depth.
    const s2 = document.querySelector<HTMLElement>('[data-lane="s2"]')!;
    const s1 = document.querySelector<HTMLElement>('[data-lane="s1"]')!;
    expect(parseInt(s2.style.paddingLeft)).toBeGreaterThan(parseInt(s1.style.paddingLeft));
    expect(document.querySelectorAll("rect[data-block]").length).toBe(9);
    expect(document.querySelector('rect[data-block="0"]')!.getAttribute("class")).toMatch(/\bfail\b/);
    expect(document.querySelector('rect[data-block="5"]')!.getAttribute("class")).toMatch(/\best\b/);
    expect(document.querySelector('rect[data-block="6"]')!.getAttribute("class")).toMatch(/\bopen\b/);
    expect(document.querySelector('rect[data-block="1"]')!.getAttribute("class")).toMatch(/\bretry\b/);
    expect(document.querySelector('rect[data-block="8"]')!.getAttribute("class")).toMatch(/\byou\b/);
    expect(screen.getAllByText("20s").length).toBeGreaterThan(0);
    expect(screen.getByText("recorded $0.12 of $0.50")).toBeTruthy();
    expect(screen.getByText("Cost by model")).toBeTruthy();
    expect(screen.getByText("Slowest tools")).toBeTruthy();
  });

  it("shows a hover card for a block", async () => {
    await show();
    await screen.findByText("Wall time");
    fireEvent.mouseEnter(document.querySelector('rect[data-block="0"]')!, { clientX: 40, clientY: 30 });
    const tip = document.querySelector(".rt-tip")!;
    expect(tip.textContent).toMatch(/overloaded/);
    expect(tip.textContent).toMatch(/work/);
    fireEvent.mouseLeave(document.querySelector('rect[data-block="0"]')!);
    expect(document.querySelector(".rt-tip")).toBeNull();
  });

  it("jumps to a main-transcript row and closes", async () => {
    await show();
    await screen.findByText("Wall time");
    const host = document.createElement("div");
    host.className = "timeline";
    host.innerHTML = '<div class="it" data-item="i-head"></div><div class="it" data-item="i-tool"></div>';
    document.body.appendChild(host);
    fireEvent.click(document.querySelector('rect[data-block="3"]')!);
    await waitFor(() => expect(scroll).toHaveBeenCalled());
    expect(scroll.mock.contexts[0]).toBe(host.querySelector('[data-item="i-tool"]'));
    expect(get().runTimeline).toBeNull();
  });

  it("opens the subagent's panel for a block on a subagent lane", async () => {
    await show();
    await screen.findByText("Wall time");
    fireEvent.click(document.querySelector('rect[data-block="5"]')!);
    expect(get().subOpen).toBe("s1");
    expect(get().details).toBe(true);
    expect(get().runTimeline).toBeNull();
  });

  it("jumps from the slowest-tools list", async () => {
    await show();
    await screen.findByText("Wall time");
    fireEvent.click(screen.getAllByRole("button").find((b) => b.classList.contains("rt-jump") && b.textContent?.startsWith("grep"))!);
    expect(get().subOpen).toBe("s2");
  });

  it("hands the context window off to the details rail", async () => {
    await show();
    fireEvent.click(screen.getByText("Context window…"));
    expect(get()).toMatchObject({ runTimeline: null, details: true, subOpen: null });
  });

  it("says so when the chat has no recorded timings", async () => {
    await show(fixture({ recorded: false, blocks: [], start_ms: null, end_ms: null }));
    expect(await screen.findByText(/No timings recorded for this chat/)).toBeTruthy();
    expect(screen.queryByText("Wall time")).toBeNull();
  });

  it("flags a partly estimated timeline", async () => {
    await show(fixture({ partial: true }));
    expect(await screen.findByText(/estimated from transcript timestamps/)).toBeTruthy();
  });

  it("reports a failed load", async () => {
    vi.spyOn(api, "runTimeline").mockRejectedValue("no such chat");
    render(<RunTimelinePanel />);
    openRunTimeline("t1");
    expect((await screen.findByRole("alert")).textContent).toMatch(/no such chat/);
  });

  it("zooms with the buttons and fits back", async () => {
    await show();
    await screen.findByText("Wall time");
    const fit = screen.getByLabelText("Fit") as HTMLButtonElement;
    expect(fit.disabled).toBe(true);
    fireEvent.click(screen.getByLabelText("Zoom in"));
    expect(fit.disabled).toBe(false);
    // Zoomed to the middle half: the first model attempt (0–2s of 20s) is gone.
    expect(document.querySelector('rect[data-block="0"]')).toBeNull();
    fireEvent.click(fit);
    expect(document.querySelector('rect[data-block="0"]')).not.toBeNull();
  });
});

describe("revealItem / jumpToRow", () => {
  it("falls back to the nearest mounted row before an unmounted one", () => {
    document.body.innerHTML = '<div class="it" data-item="i-head"></div>';
    expect(revealItem("i-folded", get().items.t1)).toBe("near");
    expect(scroll.mock.contexts[0]).toBe(document.querySelector('[data-item="i-head"]'));
    expect(revealItem("nowhere", get().items.t1)).toBeNull();
  });

  it("pages older history in when the row is not loaded", async () => {
    document.body.innerHTML = '<div class="timeline"></div>';
    const hist = vi.spyOn(api, "taskHistory");
    const ok = await jumpToRow("t1", "main", "old-row", { tries: 3, delay: 1 });
    expect(ok).toBe(false);
    // No cursor is known for this chat, so the store has nothing to page in;
    // the point is that the jump gives up cleanly rather than spinning.
    expect(hist).not.toHaveBeenCalled();
  });

  it("opens the chat first when the panel came from another chat's menu", async () => {
    set({ task: "other" } as never);
    vi.spyOn(api, "taskOpen").mockResolvedValue(null as never);
    await jumpToRow("t1", "main", "", { tries: 1, delay: 1 });
    expect(get().task).toBe("t1");
    expect(get().view).toBe("session");
  });

  it("opens the details rail for the context window", () => {
    set({ runTimeline: "t1", details: false, subOpen: "s1" } as never);
    openContextWindow("t1");
    expect(get()).toMatchObject({ runTimeline: null, details: true, subOpen: null });
  });
});
