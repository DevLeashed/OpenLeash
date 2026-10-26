/// Reasoning effort: a model has its own levels, so the pickers may only offer
/// the positions that change what actually goes on the wire. The 0…4 value is
/// spread over those levels by Rust's `pick_level`; this is that spread's twin
/// in the UI, and these cases pin the two to each other.
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { clampEffort, EFFORTS, EFFORT_LADDER, effortLabel, effortLadder, effortLevel, effortLevels, effortSteps } from "./api";

const model = (reasoning_levels: string[], reasoning_param = "reasoning_effort") => ({ reasoning_levels, reasoning_param });

// Rust: pick_level(levels, effort) = levels[round(((4 - effort) / 4) * (len - 1))]
const rustLevel = (levels: string[], effort: number) => levels[Math.round(((4 - effort) / 4) * (levels.length - 1))];

describe("effort levels", () => {
  it("offers one rung per level the model actually has", () => {
    expect(effortSteps(model(["low", "medium", "high", "xhigh", "max"]))).toEqual([0, 1, 2, 3, 4]);
    expect(effortSteps(model(["low", "medium", "high"]))).toEqual([0, 2, 4]);
    // off/on: the spread already returns "off" at position 3 (t=0.25 rounds
    // down), so the second rung is there, not 4. Levels run low → high, so
    // effort 4 (Low) is "off" and only effort 0 reaches "on".
    const onOff = model(["off", "on"], "thinking");
    expect(effortSteps(onOff)).toEqual([0, 3]);
    expect(effortLevel(onOff, 3)).toBe("off");
    expect(effortLevel(onOff, 4)).toBe("off");
    expect(effortLevel(onOff, 0)).toBe("on");
  });

  it("offers a Max-only model exactly one choice instead of five", () => {
    const maxOnly = model(["max"], "anthropic_effort");
    expect(effortSteps(maxOnly)).toEqual([0]);
    expect(effortSteps(maxOnly).map((e) => effortLabel(maxOnly, e))).toEqual(["max"]);
  });

  it("offers nothing for a model with no reasoning control", () => {
    const none = model([], "none");
    expect(effortSteps(none)).toEqual([]);
    expect(effortLevels(none)).toEqual([]);
    expect(effortLabel(none, 2)).toBeNull();
  });

  it("ignores blank levels a user left in a custom model", () => {
    expect(effortLevels(model(["low", "", "  ", "high"]))).toEqual(["low", "high"]);
  });
});

describe("effort maps to the level the request carries", () => {
  it("agrees with Rust's pick_level at every position", () => {
    for (const levels of [["low", "medium", "high"], ["off", "on"], ["low", "medium", "high", "xhigh", "max"]]) {
      const m = model(levels);
      for (let e = 0; e <= 4; e++) expect(effortLevel(m, e)).toBe(rustLevel(levels, e));
    }
  });

  it("names the level, so a thinking budget reads as its token count", () => {
    // Levels run low → high and effort runs max → low, so the rungs read
    // highest budget first: 32000 is Max.
    const haiku = model(["0", "4000", "8000", "16000", "32000"], "thinking_budget");
    expect(effortSteps(haiku).map((e) => effortLabel(haiku, e))).toEqual(["32000", "16000", "8000", "4000", "0"]);
  });
});

describe("clampEffort", () => {
  it("leaves a position the model really has", () => {
    expect(clampEffort([0, 2, 4], 2)).toBe(2);
    expect(clampEffort([0], 0)).toBe(0);
  });

  it("snaps a stored value from another model onto a real rung", () => {
    // A chat left on Low (4) after switching to a 3-level model: 4 still exists,
    // but a 5-level-only setting such as 1 has to land on a level it can send.
    expect(clampEffort([0, 2, 4], 1)).toBe(2);
    expect(clampEffort([0, 3], 2)).toBe(3);
    expect(clampEffort([0], 4)).toBe(0);
  });

  it("passes the value through when a model has no rungs at all", () => {
    // Guards the reduce's empty case: undefined here would reach Segmented's
    // value and leave the knob pointing at a button that isn't there.
    expect(clampEffort([], 2)).toBe(2);
  });
});

describe("the generic ladder still covers models with nothing to say", () => {
  it("keeps EFFORTS as the fallback name source", () => {
    expect(EFFORTS).toEqual(["Max", "Extra High", "High", "Medium", "Low"]);
  });
});

describe("the ladder is drawn Low first, Max last", () => {
  // The pickers read as a scale that climbs, like every other slider. The
  // stored value is untouched (0 = Max … 4 = Low, the contract with Rust's
  // `pick_level`), so only the order things are shown in changes.
  it("reverses the generic ladder for display", () => {
    expect(EFFORT_LADDER.map((e) => e.label)).toEqual(["Low", "Medium", "High", "Extra High", "Max"]);
    // Each entry keeps the stored effort it belongs to.
    expect(EFFORT_LADDER.map((e) => e.value)).toEqual([4, 3, 2, 1, 0]);
  });

  it("draws a model's own rungs low to high, with their own names", () => {
    const claude = model(["low", "medium", "high", "xhigh", "max"], "anthropic_effort");
    expect(effortLadder(claude).map((o) => o.label)).toEqual(["low", "medium", "high", "xhigh", "max"]);
    expect(effortLadder(claude).map((o) => o.value)).toEqual([4, 3, 2, 1, 0]);
  });

  it("draws a thinking budget by its token count, smallest first", () => {
    const haiku = model(["0", "4000", "8000", "16000", "32000"], "thinking_budget");
    expect(effortLadder(haiku).map((o) => o.label)).toEqual(["0", "4000", "8000", "16000", "32000"]);
    // The budget the request carries is unchanged by the redraw.
    expect(effortLadder(haiku).map((o) => effortLevel(haiku, o.value))).toEqual(["0", "4000", "8000", "16000", "32000"]);
  });

  it("draws a model with fewer levels low to high too", () => {
    expect(effortLadder(model(["low", "medium", "high"])).map((o) => o.label)).toEqual(["low", "medium", "high"]);
    expect(effortLadder(model(["low", "medium", "high"])).map((o) => o.value)).toEqual([4, 2, 0]);
  });

  it("offers nothing for a model with no reasoning control", () => {
    expect(effortLadder(model([], "none"))).toEqual([]);
  });
});
