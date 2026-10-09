// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { createElement } from "react";

import { ContextRing, contextRingData } from "./ContextRing";

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
    expect(container.querySelector(".ctx-breakdown")).toBeNull();
    expect(container.querySelector(".ctx-note")).toBeNull();
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
