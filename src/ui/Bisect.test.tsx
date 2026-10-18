// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { api, type BisectEdit, type BisectReport, type Item, type Settings, type TaskSummary } from "../api";
import { foldBisect, get, set } from "../store";
import { BisectDialog, jumpToEdit } from "./Bisect";
import { Review } from "./Review";

const task = { id: "bisect-task", title: "Fix parser" } as TaskSummary;
const edit = (index: number, extra: Partial<BisectEdit> = {}): BisectEdit => ({
  index, item_id: `tool-${index}`, sub_id: null, msg_index: 0, tool: "edit_file", path: `src/file${index}.ts`, add: 2, del: 1, lines: [], ...extra,
});
const edits = [edit(1), edit(2), edit(3)];
const report = (r: Partial<BisectReport>): BisectReport => ({
  outcome: "found", culprit: null, candidates: [], probes: [], from: 1, to: 3, total: 3, message: "backend says hello", ...r,
});
const finished = (r: BisectReport) => set((s) => ({ bisect: { ...s.bisect, [task.id]: foldBisect(undefined, { phase: "done", report: r }) } }));

beforeEach(() => {
  set({ task: task.id, tasks: { [task.id]: task }, settings: {} as Settings, bisect: {}, items: {}, view: "diff" });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("foldBisect", () => {
  it("folds start, probe, probed and done into one progress record", () => {
    let p = foldBisect(undefined, { phase: "start", from: 2, to: 5, total: 6 });
    expect(p).toMatchObject({ running: true, from: 2, to: 5, total: 6, probes: [], report: null });
    p = foldBisect(p, { phase: "probe", state: 1, step: 1 });
    expect(p.current).toEqual({ state: 1, step: 1 });
    const probe = { state: 1, verdict: "good" as const, code: 0, ms: 10, output: "" };
    p = foldBisect(p, { phase: "probed", probe });
    expect(p.probes).toEqual([probe]);
    const r = report({ outcome: "all_good", probes: [probe], from: 2, to: 5, total: 6 });
    p = foldBisect(p, { phase: "done", report: r });
    expect(p).toMatchObject({ running: false, current: null, report: r });
  });
});

describe("Review's Find breaking edit button", () => {
  it("is disabled when the backend says there is nothing to bisect", async () => {
    vi.spyOn(api, "review").mockResolvedValue({ git: false, files: [] });
    vi.spyOn(api, "bisectEdits").mockResolvedValue({ edits: [], available: false, reason: "Checkpoints are off" });
    const view = render(<Review />);
    const btn = await view.findByRole("button", { name: /Find breaking edit/ });
    await waitFor(() => expect(api.bisectEdits).toHaveBeenCalled());
    expect((btn as HTMLButtonElement).disabled).toBe(true);
  });

  it("opens the panel when edits exist", async () => {
    vi.spyOn(api, "review").mockResolvedValue({ git: false, files: [] });
    vi.spyOn(api, "bisectEdits").mockResolvedValue({ edits, available: true, reason: "" });
    const view = render(<Review />);
    const btn = await view.findByRole("button", { name: /Find breaking edit/ });
    await waitFor(() => expect((btn as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(btn);
    expect(view.getByRole("dialog", { name: "Find breaking edit" })).toBeTruthy();
  });
});

describe("BisectDialog", () => {
  const open = (onReverted = vi.fn()) => render(<BisectDialog task={task} edits={edits} onClose={vi.fn()} onReverted={onReverted} />);

  it("asks before running the exact command, then runs it with the chosen options", async () => {
    const run = vi.spyOn(api, "bisectRun").mockResolvedValue(report({ outcome: "all_good", message: "passes" }));
    const view = open();
    fireEvent.change(view.getByLabelText("Test command"), { target: { value: "  npm test  " } });
    fireEvent.change(view.getByLabelText("Shared folders (optional)"), { target: { value: "node_modules, ,.venv" } });
    fireEvent.click(view.getByRole("button", { name: "Run bisect…" }));
    const confirm = view.getByTestId("bisect-confirm");
    expect(confirm.textContent).toContain("npm test");
    expect(confirm.textContent).toContain("isolated scratch copy");
    expect(confirm.textContent).toMatch(/Deny rule or plan mode still refuses it/);
    expect(confirm.textContent).toContain("node_modules, .venv");
    expect(run).not.toHaveBeenCalled();
    await act(async () => { fireEvent.click(view.getByRole("button", { name: "Run bisect" })); });
    expect(run).toHaveBeenCalledWith(task.id, { command: "npm test", from: null, to: null, timeout: 120_000, runs: 1, share: ["node_modules", ".venv"] });
    await view.findByText("The command passes after the last edit — nothing to bisect.");
    expect(get().bisect[task.id]?.running).toBe(false);
  });

  it("cancelling the confirmation runs nothing", () => {
    const run = vi.spyOn(api, "bisectRun");
    const view = open();
    fireEvent.change(view.getByLabelText("Test command"), { target: { value: "make test" } });
    fireEvent.click(view.getByRole("button", { name: "Run bisect…" }));
    fireEvent.click(view.getByRole("button", { name: "Cancel" }));
    expect(run).not.toHaveBeenCalled();
    expect(view.queryByTestId("bisect-confirm")).toBeNull();
  });

  it("shows a refusal and returns to the form", async () => {
    vi.spyOn(api, "bisectRun").mockRejectedValue("denied by a Deny rule");
    const view = open();
    fireEvent.change(view.getByLabelText("Test command"), { target: { value: "rm -rf /" } });
    fireEvent.click(view.getByRole("button", { name: "Run bisect…" }));
    await act(async () => { fireEvent.click(view.getByRole("button", { name: "Run bisect" })); });
    expect((await view.findByRole("alert")).textContent).toContain("denied by a Deny rule");
    expect(view.getByLabelText("Test command")).toBeTruthy();
    expect(get().bisect[task.id]).toBeUndefined();
  });

  it.each([
    ["resolve", false], ["reject", false], ["resolve", true], ["reject", true],
  ] as const)("ignores a stale IPC %s after a subsequent start (unmounted=%s)", async (completion, unmounted) => {
    let resolve!: (r: BisectReport) => void;
    let reject!: (e: unknown) => void;
    vi.spyOn(api, "bisectRun").mockImplementation(() => new Promise((yes, no) => { resolve = yes; reject = no; }));
    const view = open();
    fireEvent.change(view.getByLabelText("Test command"), { target: { value: "npm test" } });
    fireEvent.click(view.getByRole("button", { name: "Run bisect…" }));
    fireEvent.click(view.getByRole("button", { name: "Run bisect" }));
    if (unmounted) view.unmount();
    act(() => {
      set((s) => ({ bisect: { ...s.bisect, [task.id]: foldBisect(s.bisect[task.id], { phase: "start", from: 2, to: 3, total: 3 }) } }));
      set((s) => ({ bisect: { ...s.bisect, [task.id]: foldBisect(s.bisect[task.id], { phase: "probe", state: 2, step: 1 }) } }));
    });
    const newer = get().bisect[task.id];
    await act(async () => {
      if (completion === "resolve") resolve(report({ outcome: "cancelled", message: "old run cancelled" }));
      else reject("old refusal");
    });
    expect(get().bisect[task.id]).toBe(newer);
    expect(get().bisect[task.id]).toMatchObject({ running: true, current: { state: 2, step: 1 } });
    const remounted = unmounted ? open() : view;
    expect(remounted.baseElement.textContent).toContain("Probe 1: testing the files after #2");
    expect(remounted.baseElement.textContent).not.toContain("old refusal");
  });

  it("does not overwrite the event's cancelled report with an old IPC result", async () => {
    let resolve!: (r: BisectReport) => void;
    vi.spyOn(api, "bisectRun").mockImplementation(() => new Promise((yes) => { resolve = yes; }));
    vi.spyOn(api, "bisectCancel").mockResolvedValue(true);
    const view = open();
    fireEvent.change(view.getByLabelText("Test command"), { target: { value: "npm test" } });
    fireEvent.click(view.getByRole("button", { name: "Run bisect…" }));
    fireEvent.click(view.getByRole("button", { name: "Run bisect" }));
    fireEvent.click(view.getByRole("button", { name: "Cancel" }));
    const cancelled = report({ outcome: "cancelled", message: "cancelled by user" });
    act(() => { set((s) => ({ bisect: { ...s.bisect, [task.id]: foldBisect(s.bisect[task.id], { phase: "done", report: cancelled }) } })); });
    await act(async () => { resolve(report({ outcome: "all_good", message: "stale success" })); });
    expect(get().bisect[task.id]?.report).toBe(cancelled);
    expect(view.baseElement.textContent).toContain("cancelled by user");
  });

  it("hydrates a still-current IPC result when the panel unmounted without events", async () => {
    let resolve!: (r: BisectReport) => void;
    vi.spyOn(api, "bisectRun").mockImplementation(() => new Promise((yes) => { resolve = yes; }));
    const view = open();
    fireEvent.change(view.getByLabelText("Test command"), { target: { value: "npm test" } });
    fireEvent.click(view.getByRole("button", { name: "Run bisect…" }));
    fireEvent.click(view.getByRole("button", { name: "Run bisect" }));
    view.unmount();
    const r = report({ outcome: "all_good", message: "finished while closed" });
    await act(async () => { resolve(r); });
    expect(get().bisect[task.id]?.report).toBe(r);
    const remounted = open();
    expect(remounted.baseElement.textContent).toContain("finished while closed");
  });

  it("shows live progress and Cancel calls bisectCancel", async () => {
    const cancel = vi.spyOn(api, "bisectCancel").mockResolvedValue(true);
    set((s) => ({ bisect: { ...s.bisect, [task.id]: foldBisect(foldBisect(undefined, { phase: "start", from: 1, to: 3, total: 3 }), { phase: "probe", state: 2, step: 3 }) } }));
    act(() => {
      set((s) => ({ bisect: { ...s.bisect, [task.id]: foldBisect(s.bisect[task.id], { phase: "probed", probe: { state: 0, verdict: "good", code: 0, ms: 1200, output: "ok" } }) } }));
    });
    const view = open();
    expect(view.baseElement.textContent).toContain("Probe 3: testing the files after #2 edit_file · src/file2.ts");
    expect(view.baseElement.querySelector('[data-verdict="good"]')?.textContent).toBe("passes");
    fireEvent.click(view.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(cancel).toHaveBeenCalledWith(task.id));
    expect(view.getByRole("button", { name: "Cancelling…" })).toBeTruthy();
  });

  it("renders a found culprit with its diff, and revert asks before calling revertFile", async () => {
    const revert = vi.spyOn(api, "revertFile").mockResolvedValue(undefined);
    const onReverted = vi.fn();
    finished(report({ outcome: "found", culprit: edit(2, { path: "src/evil\u202Efdp.ts", lines: [{ k: "h", t: "@@ -1,2 +1,2 @@" }, { k: "d", t: "old" }, { k: "a", t: "new" }] }), message: "edit #2 broke it" }));
    const view = open(onReverted);
    const card = view.getByTestId("bisect-culprit");
    expect(card.textContent).toContain("src/evil\\u202efdp.ts");
    expect(card.textContent).not.toContain("\u202E");
    expect(card.querySelectorAll(".dline")).toHaveLength(3);
    expect(view.baseElement.textContent).toContain("edit #2 broke it");
    expect(view.getByRole("button", { name: "Jump to message" })).toBeTruthy();
    fireEvent.click(view.getByRole("button", { name: "Revert this file…" }));
    const dialog = view.getByRole("dialog", { name: /^Revert / });
    expect(dialog.textContent).toMatch(/before the agent first touched it/);
    expect(dialog.textContent).toMatch(/later ones too/);
    expect(revert).not.toHaveBeenCalled();
    await act(async () => { fireEvent.click(view.getByRole("button", { name: "Revert file" })); });
    expect(revert).toHaveBeenCalledWith(task.id, "src/evil\u202Efdp.ts");
    expect(onReverted).toHaveBeenCalled();
    expect(view.getByRole("button", { name: "File reverted" })).toBeTruthy();
  });

  it("offers no Jump or Revert for changes made outside the agent's edits", () => {
    finished(report({ outcome: "found", culprit: edit(2, { tool: "external", item_id: "", path: "2 files: a.ts, b.ts", lines: [{ k: "c", t: "a.ts" }, { k: "c", t: "b.ts" }] }) }));
    const view = open();
    expect(view.baseElement.textContent).toContain("Changes made outside the agent's file edits (shell commands or you)");
    expect(view.getByTestId("bisect-culprit").textContent).toContain("b.ts");
    expect(view.queryByRole("button", { name: "Jump to message" })).toBeNull();
    expect(view.queryByRole("button", { name: /Revert this file/ })).toBeNull();
  });

  it.each([
    ["all_good", "The command passes after the last edit — nothing to bisect."],
    ["already_bad", "The command already fails before the first edit in range."],
    ["cancelled", "Bisect cancelled."],
  ] as const)("renders the %s outcome with the backend message", (outcome, text) => {
    finished(report({ outcome, message: `msg for ${outcome}` }));
    const view = open();
    expect(view.baseElement.querySelector(`[data-outcome="${outcome}"]`)).toBeTruthy();
    expect(view.baseElement.textContent).toContain(text);
    expect(view.baseElement.textContent).toContain(`msg for ${outcome}`);
    if (outcome === "already_bad") expect(view.baseElement.textContent).toContain("Shared folders");
  });

  it("lists the candidates of an inconclusive run", () => {
    finished(report({ outcome: "inconclusive", candidates: [edit(2), edit(3)], message: "probes timed out", probes: [{ state: 2, verdict: "timeout", code: null, ms: 120000, output: "" }] }));
    const view = open();
    const list = view.getByTestId("bisect-candidates");
    expect(list.textContent).toContain("src/file2.ts");
    expect(list.textContent).toContain("src/file3.ts");
    expect(view.baseElement.textContent).toContain("probes timed out");
    expect(view.baseElement.querySelector('[data-verdict="timeout"]')?.textContent).toBe("timed out");
  });
});

describe("jumpToEdit", () => {
  it("goes to the session and scrolls to the group row holding a folded tool call", async () => {
    const items = [{ id: "tool-1", kind: "tool" }, { id: "tool-2", kind: "tool" }] as Item[];
    set((s) => ({ items: { ...s.items, [task.id]: items } }));
    const row = document.createElement("div");
    row.dataset.item = "tool-1";
    const scroll = vi.fn();
    row.scrollIntoView = scroll;
    document.body.appendChild(row);
    try {
      jumpToEdit(task.id, edit(2));
      expect(get().view).toBe("session");
      await waitFor(() => expect(scroll).toHaveBeenCalledWith({ block: "center" }));
    } finally {
      row.remove();
    }
  });

  it("opens a subagent's transcript for its edits", () => {
    jumpToEdit(task.id, edit(2, { sub_id: "sub-7" }));
    expect(get()).toMatchObject({ view: "session", subOpen: "sub-7" });
  });
});
