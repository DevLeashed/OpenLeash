// @vitest-environment jsdom
/// <reference types="node" />
// Two field bugs from one screenshot pass:
//  - the home screen's Local / Worktree / Docker picker forces a 26px track,
//    but each option was a fixed 24px inside 2px of padding and 1px borders, so
//    the labels overflowed downwards and sat 2px below the knob's centre;
//  - hovering a tool row's path popped a centred tip over the row above, even
//    when the path was fully visible, and its text started nowhere near the
//    path it was spelling out.
// jsdom has no layout, so the CSS half pins the rules; the tooltip half fakes
// the overflow measurements the component reads.
import { readFileSync } from "node:fs";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Tooltip, tooltipPosition } from "./Tooltip";

const css = readFileSync("src/App.css", "utf8");
const block = (sel: string) => {
  // Anchored to a line start: `.settings-head > .seg {` must not count as `.seg {`.
  const i = css.search(new RegExp("^" + sel.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + " \\{", "m"));
  return i < 0 ? "" : css.slice(i, css.indexOf("}", i));
};

describe("segmented control", () => {
  it("lets options stretch to the track instead of a fixed height", () => {
    const o = block(".seg > .o");
    expect(o).not.toMatch(/height:\s*24px/);
    expect(o).toMatch(/height:\s*auto/);
    expect(block(".seg")).toMatch(/align-items:\s*stretch/);
  });

  it("can shrink on a narrow screen", () => {
    expect(block(".seg")).toMatch(/max-width:\s*100%/);
  });
});

describe("tooltipPosition", () => {
  const anchor = { top: 100, bottom: 120, left: 40, right: 440, width: 400, height: 20 };
  const viewport = { width: 1000, height: 800 };

  it("centres by default", () => {
    expect(tooltipPosition(anchor, { width: 200, height: 24 }, "top", viewport)).toEqual({ left: 140, top: 68 });
  });

  it("starts a start-aligned tip's text over the anchor's first character", () => {
    // 9 = the tip's 8px padding + 1px border.
    expect(tooltipPosition(anchor, { width: 200, height: 24 }, "top", viewport, "start").left).toBe(31);
  });

  it("still keeps a start-aligned tip on screen", () => {
    const edge = { ...anchor, left: 2, right: 402 };
    expect(tooltipPosition(edge, { width: 200, height: 24 }, "top", viewport, "start").left).toBe(8);
  });
});

describe("truncated tooltips", () => {
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
  // jsdom has no ResizeObserver; the tooltip only needs it to exist.
  const stubObserver = () => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });

  const measure = (el: HTMLElement, scroll: number, client: number) => {
    Object.defineProperty(el, "scrollWidth", { configurable: true, value: scroll });
    Object.defineProperty(el, "clientWidth", { configurable: true, value: client });
  };

  it("stays hidden while the text fits", () => {
    vi.useFakeTimers();
    render(<Tooltip content="src/a.ts" truncated><span>src/a.ts</span></Tooltip>);
    const el = screen.getByText("src/a.ts");
    measure(el, 80, 200);
    fireEvent.mouseEnter(el);
    act(() => { vi.advanceTimersByTime(500); });
    expect(screen.queryByRole("tooltip")).toBeNull();
  });

  it("shows the full text, left-aligned, when the line is clipped", () => {
    vi.useFakeTimers();
    stubObserver();
    const full = "Assets/Scripts/Presentation/Creatures/Tests/SoftBodySilhouetteTests.cs";
    render(<Tooltip content={full} truncated><span>{full}</span></Tooltip>);
    const el = screen.getByText(full);
    measure(el, 600, 200);
    fireEvent.mouseEnter(el);
    act(() => { vi.advanceTimersByTime(500); });
    const tip = screen.getByRole("tooltip");
    expect(tip.textContent).toBe(full);
    expect(tip.className).toContain("start");
    expect(tip.className).toContain("full");
  });
});
