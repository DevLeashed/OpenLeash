// The race store's pure half: N clamping, the attempt lineup a send uses, the
// `ol://race` reducer, snapshot merging and sidebar grouping.
// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));

import type { Race, TaskSummary } from "./api";
import { get, set } from "./store";
import {
  applyRaceEvent, attemptElapsed, clampRaceN, getRaces, groupRaceRows, isBudgetError, budgetMessage, listenRaces, mergeRaceSnapshot,
  raceIndex, resetRaces, resolveAttempts, resolveJudge, setRaces, startRace, toggleRace,
} from "./race";

const race = (over: Partial<Race> = {}): Race => ({
  id: "r1", prompt: "fix the bug", project: "C:/p", base_branch: "main", created_at: "2026-01-01T00:00:00Z",
  attempts: [
    { task_id: "a", model: "m1", effort: 2, checks: [], checked: false },
    { task_id: "b", model: "m2", effort: 0, checks: [], checked: false },
  ],
  judge_model: "m1", judge_effort: 2, state: "running", judge: null, winner: null, merged: null, error: null, archived: false, archived_chats: [], ...over,
});

beforeEach(() => resetRaces());
afterEach(() => { vi.mocked(invoke).mockReset(); vi.mocked(listen).mockReset(); });

describe("race config", () => {
  it("clamps N to 2..6", () => {
    expect(clampRaceN(1)).toBe(2);
    expect(clampRaceN(0)).toBe(2);
    expect(clampRaceN(-4)).toBe(2);
    expect(clampRaceN(3)).toBe(3);
    expect(clampRaceN(6)).toBe(6);
    expect(clampRaceN(9)).toBe(6);
    expect(clampRaceN(Number.NaN)).toBe(2);
    expect(clampRaceN(4.4)).toBe(4);
  });

  it("defaults every attempt and the judge to the composer's model and effort", () => {
    const cfg = { on: true, n: 3, attempts: null, judge: null };
    expect(resolveAttempts(cfg, "opus", 1)).toEqual([{ model: "opus", effort: 1 }, { model: "opus", effort: 1 }, { model: "opus", effort: 1 }]);
    expect(resolveJudge(cfg, "opus", 1)).toEqual({ model: "opus", effort: 1 });
  });

  it("keeps picked attempts, pads with the composer model and clamps the count", () => {
    const picked = [{ model: "a", effort: 0 }, { model: "b", effort: 4 }];
    expect(resolveAttempts({ on: true, n: 4, attempts: picked, judge: null }, "opus", 2)).toEqual([...picked, { model: "opus", effort: 2 }, { model: "opus", effort: 2 }]);
    expect(resolveAttempts({ on: true, n: 1, attempts: [...picked, { model: "c", effort: 1 }], judge: null }, "opus", 2)).toEqual(picked);
    expect(resolveAttempts({ on: true, n: 40, attempts: null, judge: null }, "opus", 2)).toHaveLength(6);
  });

  it("only turns on for a new task in a git project", () => {
    set({ view: "session", projectInfo: null });
    toggleRace();
    expect(getRaces().config.on).toBe(false);
    set({ view: "home", projectInfo: { git: false } as never });
    toggleRace();
    expect(getRaces().config.on).toBe(false);
    set({ view: "home", projectInfo: { git: true, path: "C:/p", branch: "main" } as never });
    toggleRace();
    expect(getRaces().config.on).toBe(true);
    expect(getRaces().setup).toBe(true);
    toggleRace();
    expect(getRaces().config.on).toBe(false);
  });
});

describe("race events", () => {
  it("upserts a race and drops one on removed", () => {
    const r = race();
    let m = applyRaceEvent({}, r);
    expect(m).toEqual({ r1: r });
    const judged = race({ state: "judged" });
    m = applyRaceEvent(m, judged);
    expect(m.r1?.state).toBe("judged");
    expect(applyRaceEvent(m, judged)).toBe(m);
    m = applyRaceEvent(m, { id: "r1", removed: true });
    expect(m).toEqual({});
    expect(applyRaceEvent(m, { id: "nope", removed: true })).toBe(m);
  });

  it("lets an event that already arrived win over the list snapshot", () => {
    const live = race({ state: "judged" });
    const merged = mergeRaceSnapshot({ r1: live }, [race({ state: "running" }), race({ id: "r2" })]);
    expect(merged.r1).toBe(live);
    expect(merged.r2?.id).toBe("r2");
  });

  it("listens on ol://race, then loads race_list, and closes the view of a forgotten race", async () => {
    let handler: ((e: { payload: unknown }) => void) | undefined;
    vi.mocked(listen).mockImplementation(async (name, cb) => { if (name === "ol://race") handler = cb as never; return () => {}; });
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "race_list" ? [race({ id: "r9" })] : null));
    await listenRaces();
    await vi.waitFor(() => expect(getRaces().races.r9).toBeTruthy());
    handler!({ payload: race() });
    expect(getRaces().races.r1?.state).toBe("running");
    setRaces({ open: "r1" });
    handler!({ payload: { id: "r1", removed: true } });
    expect(getRaces().races.r1).toBeUndefined();
    expect(getRaces().open).toBeNull();
  });
});

describe("sidebar grouping", () => {
  const t = (id: string) => ({ id });
  it("passes chats through when there are no races", () => {
    expect(groupRaceRows([t("x"), t("y")], {})).toEqual([{ kind: "task", t: t("x") }, { kind: "task", t: t("y") }]);
  });

  it("puts a race's attempts under one entry at the first attempt's place, in attempt order", () => {
    const rows = groupRaceRows([t("x"), t("b"), t("y"), t("a")], { r1: race() });
    expect(rows.map((r) => (r.kind === "task" ? r.t.id : `race:${r.tasks.map((x) => x.id).join(",")}`))).toEqual(["x", "race:a,b", "y"]);
    expect(raceIndex({ r1: race() }).get("b")).toBe("r1");
  });

  it("keeps a group for a race with only some of its attempts in the list", () => {
    const rows = groupRaceRows([t("b")], { r1: race() });
    expect(rows).toHaveLength(1);
    expect(rows[0]!.kind).toBe("race");
  });
});

describe("helpers", () => {
  it("recognises the judge's budget refusal", () => {
    expect(isBudgetError("budget: monthly cap of $50 reached")).toBe(true);
    expect(isBudgetError(new Error("budget: over"))).toBe(true);
    expect(isBudgetError("conflict")).toBe(false);
    expect(budgetMessage("budget: monthly cap reached")).toBe("monthly cap reached");
  });

  it("times a settled attempt to its last update and a live one to now", () => {
    const base = { created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:02:00Z" };
    expect(attemptElapsed({ ...base, status: "done" }, Date.parse("2026-01-01T01:00:00Z"))).toBe(120_000);
    expect(attemptElapsed({ ...base, status: "running" }, Date.parse("2026-01-01T00:05:00Z"))).toBe(300_000);
  });
});

describe("starting a race", () => {
  it("sends the composer's lineup, adds the chats and opens the comparison view", async () => {
    set({ view: "home", projectInfo: { git: true, path: "C:/p", branch: "main" } as never, tasks: {} });
    setRaces({ config: { on: true, n: 2, attempts: null, judge: { model: "judge", effort: 3 } } });
    const tasks = [{ id: "a", title: "Race 1/2 · fix" }, { id: "b", title: "Race 2/2 · fix" }] as TaskSummary[];
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "race_create" ? { race: race(), tasks } : null));
    await startRace({ prompt: "fix", project: "C:/p", base_branch: "", perm: "turbo", plan: false, assist: "default", agents: ["explore", "general"], route: "" }, "opus", 1);
    const call = vi.mocked(invoke).mock.calls.find(([cmd]) => cmd === "race_create");
    expect(call?.[1]).toEqual({ req: expect.objectContaining({ attempts: [{ model: "opus", effort: 1 }, { model: "opus", effort: 1 }], judge_model: "judge", judge_effort: 3 }) });
    expect(Object.keys(get().tasks)).toEqual(["a", "b"]);
    expect(getRaces().open).toBe("r1");
    expect(getRaces().config.on).toBe(false);
  });
});
