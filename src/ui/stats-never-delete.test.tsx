// @vitest-environment jsdom
//
// The Stats screen must not offer a way to delete usage numbers.
//
// This is the frontend half of "stats never delete": the backend has no
// `stats_reset` and no `stats_prune_chats` command any more, so a button left
// on the screen would either do nothing or reach for a command that no longer
// exists. Both are worse than not having the button.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";

const invoke = vi.hoisted(() => vi.fn(async (_cmd: string, _args?: unknown): Promise<unknown> => null));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { set } from "../store";
import { StatsTab } from "./Stats";
import type { Settings } from "../api";

const empty = {
  since: "2026-01-01T00:00:00Z",
  models: {},
  accounts: {},
  daily: {},
  hourly: {},
  agents: {},
  tools: {},
  events: {},
  chats: {},
};

beforeEach(() => {
  // `MorphText` (the number tiles) animates through `troph`, which reads
  // `matchMedia` to decide whether the user asked for reduced motion. jsdom
  // does not implement it, so the tiles throw without a stub.
  vi.stubGlobal("matchMedia", vi.fn(() => ({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })));
  // ...and the Web Animations API it drives. jsdom has neither, so the tiles
  // throw on unmount without both stubs. The prototype is what React's
  // elements actually use, so patching it covers every node.
  if (!Element.prototype.getAnimations) {
    Element.prototype.getAnimations = function () { return []; };
  }
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "stats_get") return empty;
    if (cmd === "usage_get") return { month: 0, spend: {}, tokens: 0, budget: 0 };
    return null;
  });
  set({ settings: { budget: 50 } as Settings, tasks: {}, monthSpend: 0 });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe("the stats screen", () => {
  it("offers no way to clear the numbers", async () => {
    render(<StatsTab />);
    await waitFor(() => expect(screen.getByText("Stats")).toBeTruthy());
    // The three controls that used to delete usage.
    expect(screen.queryByText(/Reset stats/i)).toBeNull();
    expect(screen.queryByText(/Clear all stats/i)).toBeNull();
    expect(screen.queryByText(/Clear stats of deleted/i)).toBeNull();
    expect(screen.queryByText(/Keep stats for deleted chats/i)).toBeNull();
  });

  it("never calls a command that deletes stats", async () => {
    render(<StatsTab />);
    await waitFor(() => expect(screen.getByText("Stats")).toBeTruthy());
    const cmds = invoke.mock.calls.map((c) => c[0] as string);
    expect(cmds).toContain("stats_get");
    expect(cmds).not.toContain("stats_reset");
    expect(cmds).not.toContain("stats_prune_chats");
  });
});
