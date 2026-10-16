// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { createElement } from "react";

import { ContextRing, contextPartRows, contextRingData } from "./ContextRing";
import type { Settings } from "../../api";

afterEach(() => cleanup());

describe("context ring", () => {
  it("shows measured used and remaining tokens as a share of the limit", () => {
    expect(contextRingData(25_000, 100_000)).toEqual({
      used: 25_000,
      limit: 100_000,
      remaining: 75_000,
      percentage: 25,
      fill: 0.25,
    });
    const { container, getByRole, getByText } = render(createElement(ContextRing, { used: 25_000, limit: 100_000 }));
    expect(getByRole("img").getAttribute("aria-label")).toContain("25% used");
    expect(getByText("25k")).toBeTruthy();
    expect(getByText("75k")).toBeTruthy();
    expect(getByText("100k token limit")).toBeTruthy();
    expect(container.querySelector(".ctx-ring-used")?.getAttribute("stroke-dashoffset")).toBe(String(2 * Math.PI * 25 * 0.75));
  });

  it("caps the drawing but reports provider usage above the window honestly", () => {
    const data = contextRingData(120, 100);
    expect(data.percentage).toBe(120);
    expect(data.fill).toBe(1);
    expect(data.remaining).toBe(0);
    const { container, getByRole, getByText } = render(createElement(ContextRing, { used: 120, limit: 100 }));
    expect(getByRole("img").getAttribute("aria-label")).toContain("120% used");
    expect(getByText("120%")).toBeTruthy();
    expect(container.querySelector(".ctx-ring-used")?.getAttribute("stroke-dashoffset")).toBe("0");
  });

  it("does not divide by a missing limit or draw a guessed percentage", () => {
    expect(contextRingData(500, 0)).toEqual({ used: 500, limit: 0, remaining: 0, percentage: null, fill: 0 });
    const { container, getByRole, getByText } = render(createElement(ContextRing, { used: 500, limit: 0 }));
    expect(getByRole("img").getAttribute("aria-label")).toContain("context limit unavailable");
    expect(container.querySelector(".ctx-ring > span")?.textContent).toBe("—");
    expect(container.querySelector(".ctx-ring-used")).toBeNull();
    expect(getByText("Context limit unavailable")).toBeTruthy();
  });

  it("sanitizes negative or non-finite provider values", () => {
    expect(contextRingData(-2, Number.NaN)).toEqual({ used: 0, limit: 0, remaining: 0, percentage: null, fill: 0 });
  });
});

describe("context breakdown", () => {
  // The split is an estimate rescaled to the provider's count, so it is opt-in:
  // off, the meter is the measured pair the provider actually reported. The
  // assertions below render with `breakdown: true` because *this* describe block
  // is about the split; the default is asserted here.
  it("keeps the estimated split out of the meter until the setting says otherwise", () => {
    const parts = { system: 40_000, instructions: 5_000, skills: 0, tools: 30_000, attachments: 0, history: 24_000, summary: 1_000 };
    expect(({} as Settings).ctx_breakdown ?? false).toBe(false);
    const off = render(createElement(ContextRing, { used: 25_000, limit: 100_000, parts }));
    expect(off.container.querySelector(".ctx-breakdown")).toBeNull();
    expect(off.container.querySelector(".ctx-breakdown-row")).toBeNull();
    expect(off.container.querySelector(".ctx-note")).toBeNull();
    // Hiding the estimate does not take the measured pair with it.
    expect(off.getByText("25k")).toBeTruthy();
    expect(off.getByText("100k token limit")).toBeTruthy();
    cleanup();
    // And the setting is the only thing that draws it.
    const on = render(createElement(ContextRing, { used: 25_000, limit: 100_000, parts, breakdown: true }));
    expect(on.container.querySelectorAll(".ctx-breakdown-row")).toHaveLength(5);
  });

  // A task that has not run a turn carries no split, so the ring must stay the
  // whole story rather than drawing four empty rows. This is the case the old
  // "no breakdown, ever" assertions were protecting.
  it("draws no breakdown for a task with no measured split", () => {
    const { container } = render(createElement(ContextRing, { used: 25_000, limit: 100_000, breakdown: true }));
    expect(container.querySelector(".ctx-breakdown")).toBeNull();
    expect(container.querySelector(".ctx-note")).toBeNull();
  });

  it("labels each category and omits the ones the request did not use", () => {
    const parts = { system: 40_000, instructions: 5_000, skills: 0, tools: 30_000, attachments: 0, history: 24_000, summary: 1_000 };
    const { container, getByText } = render(createElement(ContextRing, { used: 100_000, limit: 200_000, parts, breakdown: true }));
    // Skills and attachments are zero, so five rows, not seven.
    expect(container.querySelectorAll(".ctx-breakdown-row")).toHaveLength(5);
    expect(getByText("System prompt")).toBeTruthy();
    expect(getByText("Tool schemas")).toBeTruthy();
    expect(getByText("Conversation")).toBeTruthy();
    expect(getByText("Summary")).toBeTruthy();
    expect(container.querySelector(".ctx-breakdown")?.textContent).not.toContain("Skills");
    expect(getByText("40k")).toBeTruthy();
  });

  it("scales the bars to the split it was given, not to the window", () => {
    const parts = { system: 40_000, instructions: 5_000, skills: 0, tools: 30_000, attachments: 0, history: 24_000, summary: 1_000 };
    const { container } = render(createElement(ContextRing, { used: 100_000, limit: 200_000, parts, breakdown: true }));
    const widths = [...container.querySelectorAll(".ctx-breakdown-bar i")].map((el) => (el as HTMLElement).style.width);
    // 40_000/100_000 of the *request*, which happens to be 50% of the window too.
    expect(widths[0]).toBe("40%");
    expect(widths.reduce((n, w) => n + Number.parseFloat(w), 0)).toBeCloseTo(100, 0);
  });

  it("draws nothing, rather than a row of zeroes, when every bucket is empty", () => {
    expect(contextPartRows({ system: 0, instructions: 0, skills: 0, tools: 0, attachments: 0, history: 0, summary: 0 })).toEqual([]);
    expect(contextPartRows(undefined)).toEqual([]);
    const { container } = render(createElement(ContextRing, {
      used: 10, limit: 100, breakdown: true,
      parts: { system: 0, instructions: 0, skills: 0, tools: 0, attachments: 0, history: 0, summary: 0 },
    }));
    expect(container.querySelector(".ctx-breakdown")).toBeNull();
  });

  it("keeps a fixed row order so the panel does not reshuffle between turns", () => {
    const rows = contextPartRows({ system: 1, instructions: 1, skills: 1, tools: 90, attachments: 1, history: 1, summary: 1 });
    // Tools dominate by size, but still draw fourth: order is categorical.
    expect(rows.map((r) => r.key)).toEqual(["system", "instructions", "skills", "tools", "attachments", "history", "summary"]);
    expect(rows.find((r) => r.key === "tools")?.value).toBe(90);
  });

  it("sanitizes negative and non-finite buckets instead of drawing a negative bar", () => {
    const rows = contextPartRows({ system: -5, instructions: Number.NaN, skills: 10, tools: 0, attachments: 0, history: 0, summary: 0 });
    expect(rows.map((r) => r.key)).toEqual(["skills"]);
    expect(rows.find((r) => r.key === "skills")?.share).toBe(1);
  });
});
