// The race UI: the comparison view's attempt cards and actions, the composer's
// race setup, and the sidebar's race group.
// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null), save: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import type { ModelInfo, ProviderView, Race, RaceDetail, Settings, TaskSummary } from "../api";
import { get, set } from "../store";
import { archiveRace, getRaces, groupRaceRows, resetRaces, setRaces } from "../race";
import { RaceView } from "./Race";
import { RaceSetupDialog } from "./RaceSetup";
import { Sidebar } from "./Chrome";

const model = (id: string, name: string): ModelInfo => ({
  id, name, provider: "anthropic", context: 200000, output: 32000, input_price: 1, output_price: 1, effort: true,
  input_types: ["text"], capabilities: [], reasoning_levels: ["low", "medium", "high", "xhigh", "max"], reasoning_param: "effort", custom: false, enabled: true,
});
const provider = { id: "anthropic", name: "Anthropic", enabled: true, connected: true, has_key: true, account: null } as unknown as ProviderView;

const task = (id: string, over: Partial<TaskSummary> = {}): TaskSummary => ({
  id, title: `Race · ${id}`, status: "done", waiting_kind: null, paused: null, archived: false, pinned: false, project: "C:/p", cwd: `C:/wt/${id}`, branch: `ol/race-${id}`,
  worktree: true, subs: [], usage: { input: 0, output: 0, cache_read: 0, cache_write: 0, cost: 0.42, last_context: 0 },
  created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:03:05Z", touched_at: "2026-01-01T00:00:00Z", rev: 1, unread: false, order: 0, ...over,
} as TaskSummary);

const race = (over: Partial<Race> = {}): Race => ({
  id: "r1", prompt: "Fix the flaky test", project: "C:/p", base_branch: "main", created_at: "2026-01-01T00:00:00Z",
  attempts: [
    { task_id: "a", model: "anthropic/opus", effort: 0, checked: true, checks: [{ label: "test", cmd: "npm test", ok: true, output: "all green" }] },
    { task_id: "b", model: "anthropic/sonnet", effort: 2, checked: true, checks: [{ label: "test", cmd: "npm test", ok: false, output: "1 failing: boom" }] },
    { task_id: "c", model: "anthropic/opus", effort: 0, checked: true, checks: [] },
  ],
  judge_model: "anthropic/opus", judge_effort: 2, state: "judged",
  judge: { model: "anthropic/opus", summary: "Attempt 2 is cleanest.", at: "2026-01-01T00:10:00Z", cost: 0.1, error: null, ranking: [
    { task_id: "b", rank: 1, score: 9, reason: "Smallest correct fix" },
    { task_id: "a", rank: 2, score: 7, reason: "Works but noisy" },
  ] },
  winner: "a", merged: null, error: null, archived: false, archived_chats: [], ...over,
});

const detail = (r: Race): RaceDetail => ({ race: r, stats: [
  { task_id: "a", files: 3, add: 40, del: 2, error: null },
  { task_id: "b", files: 1, add: 5, del: 1, error: null },
] });

beforeEach(() => {
  resetRaces();
  set({
    models: [model("anthropic/opus", "Opus 5"), model("anthropic/sonnet", "Sonnet 5")], providers: [provider],
    tasks: { a: task("a"), b: task("b", { status: "running" }) }, // "c" was deleted
    projectInfo: { path: "C:/p", exists: true, git: true, branch: "develop", branches: ["develop"], memory: [], docs: [], disabled_docs: [], trusted: true, decided: true },
    settings: { routes: [], saved_prompts: [] } as unknown as Settings, view: "home", task: null, sidebar: true,
  });
  (Element.prototype as unknown as { getAnimations?: unknown }).getAnimations ??= () => [];
  window.matchMedia ??= (() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia;
});
afterEach(() => { cleanup(); vi.mocked(invoke).mockReset(); });

function serve(r: Race) {
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    if (cmd === "race_detail") return detail(r);
    if (cmd === "race_pick") return { ...r, winner: (args as { taskId: string | null }).taskId };
    if (cmd === "race_merge") return { race: { ...r, merged: "Merged ol/race-a into develop" }, message: "Merged ol/race-a into develop" };
    if (cmd === "race_judge") throw "budget: monthly cap of $5 reached";
    return null;
  });
}

describe("race comparison view", () => {
  it("shows each attempt's model, status, diff, checks, cost, rank and the winner", async () => {
    const r = race();
    serve(r);
    setRaces({ races: { r1: r } });
    const view = render(<RaceView race={r} onClose={() => {}} />);
    await act(async () => {});
    const a = within(view.getByTestId("race-attempt-1"));
    const b = within(view.getByTestId("race-attempt-2"));
    const c = within(view.getByTestId("race-attempt-3"));
    expect(a.getByText(/Opus 5/)).toBeTruthy();
    expect(a.getByText("Winner")).toBeTruthy();
    expect(a.getByText("#2 · 7")).toBeTruthy();
    expect(view.getByTestId("race-attempt-1").textContent).toContain("3 files");
    expect(view.getByTestId("race-attempt-1").textContent).toContain("+40");
    expect(view.getByTestId("race-attempt-1").textContent).toContain("$0.42");
    expect(view.getByTestId("race-attempt-1").textContent).toContain("3m 5s");
    expect(a.getByText("pass")).toBeTruthy();
    // The judge's #1 is highlighted, and its failing check reads as a failure.
    expect(view.getByTestId("race-attempt-2").className).toContain("first");
    expect(b.getByText("#1 · 9")).toBeTruthy();
    expect(b.getByText("Smallest correct fix")).toBeTruthy();
    expect(b.getByText("fail")).toBeTruthy();
    expect(b.queryByText("1 failing: boom")).toBeNull();
    fireEvent.click(b.getByRole("button", { name: /test/ }));
    expect(b.getByText("1 failing: boom")).toBeTruthy();
    // A deleted attempt says so, and has nothing to open.
    expect(c.getByText("deleted")).toBeTruthy();
    expect(c.getByText("checks off")).toBeTruthy();
    expect((c.getByRole("button", { name: "Open chat" }) as HTMLButtonElement).disabled).toBe(true);
    expect(view.getByText("Attempt 2 is cleanest.")).toBeTruthy();
  });

  it("asks before merging, naming the winner branch and the target branch", async () => {
    const r = race();
    serve(r);
    setRaces({ races: { r1: r } });
    const view = render(<RaceView race={r} onClose={() => {}} />);
    await act(async () => {});
    fireEvent.click(view.getByRole("button", { name: "Merge winner into develop…" }));
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === "race_merge")).toBe(false);
    const dialog = view.getByRole("dialog", { name: "Merge the winner?" });
    expect(dialog.textContent).toContain("ol/race-a");
    expect(dialog.textContent).toContain("develop");
    expect(dialog.textContent).toMatch(/uncommitted work in the winner's worktree/);
    await act(async () => { fireEvent.click(within(dialog).getByRole("button", { name: "Merge" })); });
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === "race_merge")).toBe(true);
    expect(getRaces().races.r1?.merged).toContain("Merged");
  });

  it("disables merge until a winner is picked, and picks through race_pick", async () => {
    const r = race({ winner: null });
    serve(r);
    setRaces({ races: { r1: r } });
    const view = render(<RaceView race={r} onClose={() => {}} />);
    await act(async () => {});
    expect((view.getByRole("button", { name: /Merge winner/ }) as HTMLButtonElement).disabled).toBe(true);
    expect((view.getByRole("button", { name: "Clean up losers…" }) as HTMLButtonElement).disabled).toBe(true);
    await act(async () => { fireEvent.click(within(view.getByTestId("race-attempt-2")).getByRole("button", { name: "Pick winner" })); });
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("race_pick", { id: "r1", taskId: "b" });
    expect(getRaces().races.r1?.winner).toBe("b");
  });

  it("asks to go past the budget cap and retries the judge with force", async () => {
    const r = race({ state: "running", judge: null, winner: null });
    serve(r);
    setRaces({ races: { r1: r } });
    const view = render(<RaceView race={r} onClose={() => {}} />);
    await act(async () => {});
    await act(async () => { fireEvent.click(view.getByRole("button", { name: "Judge now" })); });
    const dialog = view.getByRole("dialog", { name: "Over the monthly budget" });
    expect(dialog.textContent).toContain("monthly cap of $5 reached");
    await act(async () => { fireEvent.click(within(dialog).getByRole("button", { name: "Judge anyway" })); });
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("race_judge", { id: "r1", model: null, force: true });
  });

  it("cleans up losers through the chat delete, keeping worktrees unless asked", async () => {
    const r = race();
    serve(r);
    setRaces({ races: { r1: r } });
    const view = render(<RaceView race={r} onClose={() => {}} />);
    await act(async () => {});
    fireEvent.click(view.getByRole("button", { name: "Clean up losers…" }));
    const dialog = view.getByRole("dialog", { name: "Clean up losers" });
    // Only the losers still in the store: "a" won, "c" is already gone.
    expect(dialog.textContent).toContain("Race · b");
    expect(dialog.textContent).not.toContain("Race · a");
    await act(async () => { fireEvent.click(within(dialog).getByRole("button", { name: "Delete chats; keep worktrees" })); });
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("task_delete", { id: "b", removeWorktree: false });
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === "task_delete")).toHaveLength(1);
    expect(get().tasks.b).toBeUndefined();
  });
});

describe("composer race setup", () => {
  it("defaults to the composer's model repeated N times and offers 2..6", () => {
    set((s) => ({ home: { ...s.home, model: "anthropic/sonnet", effort: 2 } }));
    setRaces({ setup: true, config: { on: true, n: 3, attempts: null, judge: null } });
    const view = render(<RaceSetupDialog />);
    const rows = view.getAllByLabelText(/^Attempt \d$/);
    expect(rows).toHaveLength(3);
    for (const row of rows) expect(row.textContent).toContain("Sonnet 5");
    expect(view.getByLabelText("Judge").textContent).toContain("Sonnet 5");
    const counts = within(view.getByRole("radiogroup", { name: "Attempts" })).getAllByRole("radio").map((b) => b.textContent);
    expect(counts).toEqual(["2", "3", "4", "5", "6"]);
    act(() => { fireEvent.click(within(view.getByRole("radiogroup", { name: "Attempts" })).getByRole("radio", { name: "5" })); });
    expect(view.getAllByLabelText(/^Attempt \d$/)).toHaveLength(5);
    expect(getRaces().config.n).toBe(5);
  });
});

describe("sidebar race group", () => {
  it("groups a race's chats under one header with a Compare button", () => {
    set({ tasks: { a: task("a"), b: task("b"), x: task("x", { title: "Unrelated chat" }) }, settings: { routes: [], saved_prompts: [], project: "C:/p" } as unknown as Settings });
    setRaces({ races: { r1: race() } });
    const view = render(<Sidebar />);
    const group = view.container.querySelector<HTMLElement>('[data-race="r1"]')!;
    expect(group).toBeTruthy();
    expect(group.textContent).toContain("Race · Fix the flaky test");
    expect(group.textContent).toContain("Race · a");
    expect(group.textContent).toContain("Race · b");
    expect(group.textContent).not.toContain("Unrelated chat");
    act(() => { fireEvent.click(within(group).getByRole("button", { name: "Compare" })); });
    expect(getRaces().open).toBe("r1");
  });
});

describe("race archive, folder and progress", () => {
  it("opens an attempt's worktree folder, and disables it for a deleted chat", async () => {
    const r = race();
    serve(r);
    setRaces({ races: { r1: r } });
    const view = render(<RaceView race={r} onClose={() => {}} />);
    await act(async () => {});
    await act(async () => { fireEvent.click(within(view.getByTestId("race-attempt-1")).getByRole("button", { name: "Open folder" })); });
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("open_path", { cwd: "C:/wt/a", path: "." });
    expect((within(view.getByTestId("race-attempt-3")).getByRole("button", { name: "Open folder" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("shows the whole race's cost in the header", async () => {
    const r = race();
    serve(r);
    setRaces({ races: { r1: r } });
    render(<RaceView race={r} onClose={() => {}} />);
    await act(async () => {});
    // two chats at $0.42 plus the judge's $0.10
    expect(document.querySelector(".race-subtitle")!.textContent).toContain("$0.94 total");
  });

  it("archives through race_archive, closes the view, and Undo restores", async () => {
    const r = race();
    vi.mocked(invoke).mockImplementation(async (cmd, args) => (cmd === "race_archive" ? { ...r, archived: (args as { archived: boolean }).archived, archived_chats: ["a", "b"] } : null));
    setRaces({ races: { r1: r }, open: "r1" });
    expect(await archiveRace("r1")).toBe(true);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("race_archive", { id: "r1", archived: true });
    expect(getRaces().races.r1?.archived).toBe(true);
    expect(getRaces().open).toBeNull();
    expect(get().toast).toBe("Race archived");
    await archiveRace("r1", false);
    expect(getRaces().races.r1?.archived).toBe(false);
  });

  it("reports a failed archive and leaves the race as it was", async () => {
    vi.mocked(invoke).mockRejectedValue("disk is full");
    setRaces({ races: { r1: race() } });
    expect(await archiveRace("r1")).toBe(false);
    expect(getRaces().races.r1?.archived).toBe(false);
    expect(get().toast).toContain("disk is full");
  });

  it("does not group chats of an archived race, and lists it in Settings' archive via the store", () => {
    const rows = groupRaceRows([{ id: "a" }, { id: "x" }], { r1: race({ archived: true }) });
    expect(rows.every((e) => e.kind === "task")).toBe(true);
  });

  it("shows attempt progress on a running race's group header", () => {
    set({ tasks: { a: task("a"), b: task("b", { status: "running", model: "anthropic/sonnet" }), x: task("x") }, settings: { routes: [], saved_prompts: [], project: "C:/p" } as unknown as Settings });
    setRaces({ races: { r1: race({ state: "running", winner: null, attempts: race().attempts.slice(0, 2) }) } });
    const view = render(<Sidebar />);
    expect(view.container.querySelector('[data-race="r1"] .race-pill')!.textContent).toBe("Running 1/2");
  });
});

describe("per-attempt detailed stats", () => {
  it("toggles a Stats panel with tokens, cache, cost, context and price for that attempt's model", async () => {
    const r = race();
    serve(r);
    set((st) => ({ tasks: { ...st.tasks, a: task("a", { model: "anthropic/opus", usage: { input: 12000, output: 3400, cache_read: 50000, cache_write: 800, cost: 0.42, last_context: 100000 } }) } }));
    setRaces({ races: { r1: r } });
    const view = render(<RaceView race={r} onClose={() => {}} />);
    await act(async () => {});
    const card = within(view.getByTestId("race-attempt-1"));
    expect(card.queryByTestId("race-stats-1")).toBeNull();
    fireEvent.click(card.getByRole("button", { name: "Stats" }));
    const panel = card.getByTestId("race-stats-1").textContent!;
    expect(panel).toContain("12,000");
    expect(panel).toContain("3,400");
    expect(panel).toContain("50,000");
    expect(panel).toContain("50%");
    expect(panel).toContain("Opus 5");
    fireEvent.click(card.getByRole("button", { name: "Stats" }));
    expect(card.queryByTestId("race-stats-1")).toBeNull();
    expect((within(view.getByTestId("race-attempt-3")).getByRole("button", { name: "Stats" }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("sidebar race group archive", () => {
  it("archives the whole race from the group header", async () => {
    set({ tasks: { a: task("a"), b: task("b") }, settings: { routes: [], saved_prompts: [], project: "C:/p" } as unknown as Settings });
    const r = race();
    setRaces({ races: { r1: r } });
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "race_archive" ? { ...r, archived: true, archived_chats: ["a", "b"] } : null));
    const view = render(<Sidebar />);
    const group = view.container.querySelector<HTMLElement>('[data-race="r1"]')!;
    await act(async () => { fireEvent.click(within(group).getByRole("button", { name: "Archive" })); });
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("race_archive", { id: "r1", archived: true });
    expect(getRaces().races.r1?.archived).toBe(true);
  });
});
