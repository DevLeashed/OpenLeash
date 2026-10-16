// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { readFileSync } from "node:fs";

const handlers = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (name: string, cb: (event: { payload: unknown }) => void) => {
  handlers.set(name, cb);
  return () => handlers.delete(name);
}) }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type Item, type TaskSummary } from "../api";
import { boot, disposeBoot, get, mergeNoticeSnapshot, set, type Notice } from "../store";
import { describe as describeAttention, surfaces, toastBody } from "../notify";
import { AgentNotice, AgentNoticeList, pendingNotices } from "./AgentNotice";
import { Notices } from "./Notices";
import { NOTICE_EXIT_MS } from "./notice-presence";
import { FeedRows } from "./Session";

const task = { id: "t1", title: "Unity work", status: "running", model: "m", cwd: "D:/p", subs: [], archived: false, hidden: false } as unknown as TaskSummary;
const notice = (id = "n1", dismissed = false): Item => ({
  id, kind: "user_notice", text: "Dismiss the Unity modal when convenient.\nI’ll keep working meanwhile.", ts: "2026-10-08T00:00:00Z",
  data: { title: "Unity Editor", level: "info", dismissed },
});

beforeEach(() => {
  set({ items: {}, tasks: { t1: task }, task: "t1", view: "session", notices: [], settings: null });
});
afterEach(() => { cleanup(); disposeBoot(); vi.restoreAllMocks(); vi.useRealTimers(); });

describe("non-blocking informational notices", () => {
  it("shows the full message with a dismiss action, never a question or form", () => {
    render(<AgentNotice it={notice()} taskId="t1" />);
    expect(screen.getByText("Unity Editor")).toBeTruthy();
    expect(screen.getByText(/No reply needed.*isn't waiting/)).toBeTruthy();
    expect(screen.getByText(/Dismiss the Unity modal/)).toBeTruthy();
    expect(screen.getAllByRole("button").map((b) => b.textContent)).toEqual([" Dismiss"]);
    expect(screen.queryByRole("textbox")).toBeNull();
  });

  it("does not expire and returns after remounting until saved dismissal arrives", () => {
    vi.useFakeTimers();
    set({ items: { t1: [notice()] } });
    const view = render(<AgentNoticeList taskId="t1" />);
    act(() => { vi.advanceTimersByTime(60 * 60 * 1000); });
    expect(screen.getByText("Unity Editor")).toBeTruthy();
    view.unmount();
    render(<AgentNoticeList taskId="t1" />);
    expect(screen.getByText("Unity Editor")).toBeTruthy();
    act(() => { set({ items: { t1: [notice("n1", true)] } }); });
    expect(screen.queryByRole("status", { name: "Agent notice" })).toBeNull();
    expect(screen.getByText("Unity Editor").closest("section")?.className).toContain("notice-exiting");
    act(() => { vi.advanceTimersByTime(NOTICE_EXIT_MS); });
    expect(screen.queryByText("Unity Editor")).toBeNull();
  });

  it("animates the card immediately, before the save resolves, and dismisses only its own item", async () => {
    vi.useFakeTimers();
    let finish!: () => void;
    const dismiss = vi.spyOn(api, "dismissAgentNotice").mockReturnValue(new Promise<void>((resolve) => { finish = resolve; }));
    const answer = vi.spyOn(api, "answerNonBlocking");
    const respond = vi.spyOn(api, "respond");
    render(<AgentNotice it={notice()} taskId="t1" />);
    fireEvent.click(screen.getByRole("button", { name: "Dismiss notice" }));
    const card = screen.getByText("Unity Editor").closest("section")!;
    expect(card.className).toContain("notice-exiting");
    expect(card.hasAttribute("inert")).toBe(true);
    expect(screen.queryByRole("status", { name: "Agent notice" })).toBeNull();
    expect(dismiss).toHaveBeenCalledExactlyOnceWith("t1", "n1");
    act(() => { vi.advanceTimersByTime(NOTICE_EXIT_MS); });
    expect(screen.queryByText("Unity Editor")).toBeNull();
    await act(async () => { finish(); });
    expect(screen.queryByText("Unity Editor")).toBeNull();
    expect(answer).not.toHaveBeenCalled();
    expect(respond).not.toHaveBeenCalled();
    expect(get().tasks.t1?.status).toBe("running");
  });

  it("retains the card and allows retry if persistence fails", async () => {
    const dismiss = vi.spyOn(api, "dismissAgentNotice").mockRejectedValue(new Error("disk full"));
    render(<AgentNotice it={notice()} taskId="t1" />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Dismiss notice" })); });
    expect(screen.getByText("Unity Editor")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Dismiss notice" }) as HTMLButtonElement).disabled).toBe(false);
    expect(get().toast).toContain("disk full");
    expect(dismiss).toHaveBeenCalledTimes(1);
  });

  it("brings the card back after a save fails following the exit, and animates a retry", async () => {
    vi.useFakeTimers();
    let fail!: (error: Error) => void;
    const dismiss = vi.spyOn(api, "dismissAgentNotice").mockReturnValue(new Promise<void>((_, reject) => { fail = reject; }));
    render(<AgentNotice it={notice()} taskId="t1" />);
    fireEvent.click(screen.getByRole("button", { name: "Dismiss notice" }));
    act(() => { vi.advanceTimersByTime(NOTICE_EXIT_MS); });
    expect(screen.queryByText("Unity Editor")).toBeNull();
    await act(async () => { fail(new Error("disk full")); });
    expect(screen.getByRole("status", { name: "Agent notice" }).className).not.toContain("notice-exiting");
    dismiss.mockReturnValue(new Promise<void>(() => {}));
    fireEvent.click(screen.getByRole("button", { name: "Dismiss notice" }));
    expect(screen.getByText("Unity Editor").closest("section")?.className).toContain("notice-exiting");
    act(() => { vi.advanceTimersByTime(NOTICE_EXIT_MS); });
    expect(screen.queryByText("Unity Editor")).toBeNull();
    expect(dismiss).toHaveBeenCalledTimes(2);
  });

  it("shows warning styling and renders model text inertly", () => {
    const it = notice();
    it.data.level = "warning";
    it.text = "<script>alert('not executable')</script>";
    render(<AgentNotice it={it} taskId="t1" />);
    expect(screen.getByRole("status", { name: "Agent warning" }).className).toContain("warning");
    expect(document.querySelector("script")).toBeNull();
    expect(screen.getByText(it.text)).toBeTruthy();
  });

  it("shows every pending notice in order, excluding other item kinds and dismissed ones", () => {
    const other: Item = { ...notice("q"), kind: "asklater" };
    expect(pendingNotices([notice("n1"), other, notice("old", true), notice("n2")]).map((n) => n.id)).toEqual(["n1", "n2"]);
    expect(pendingNotices(undefined)).toEqual([]);
  });

  it("moves notices to the global surface when navigating away, without acknowledging them", () => {
    set({ items: { t1: [notice()] } });
    render(<Notices />);
    expect(screen.queryByText("Unity Editor")).toBeNull();
    act(() => { set({ task: null, view: "home" }); });
    expect(screen.getByText("Unity Editor")).toBeTruthy();
    act(() => { set({ notices: [{ key: "t1:done", task_id: "t1", needs: "done", title: "Finished", body: "Done", at: 0 }] }); });
    expect(screen.getByText("Unity Editor")).toBeTruthy();
    act(() => { set({ task: "t1", view: "session" }); });
    expect(screen.queryByText("Unity Editor")).toBeNull();
    expect(pendingNotices(get().items.t1)).toHaveLength(1);
  });

  it.each(["hidden", "archived"] as const)("hides notices from %s chats without dismissing them", (flag) => {
    set({ items: { t1: [notice()] }, tasks: { t1: { ...task, [flag]: true } }, view: "home", task: null });
    render(<><Notices /><AgentNoticeList taskId="t1" /></>);
    expect(screen.queryByText("Unity Editor")).toBeNull();
    expect(pendingNotices(get().items.t1)).toHaveLength(1);
  });

  it("keeps the current chat's notices visible when the Artificts workspace hides its composer", () => {
    const source = readFileSync("src/ui/Session.tsx", "utf8");
    const workspace = source.split("\n").find((line) => line.includes("{workspaceOpen && <div"));
    expect(workspace).toContain("<AgentNoticeList taskId={task.id} />");
  });

  it("keeps pending notices out of the feed and preserves dismissed receipts", () => {
    const view = render(<FeedRows task={task} items={[notice()]} showThinking={false} bgs={[]} />);
    expect(screen.queryByText("Unity Editor")).toBeNull();
    view.rerender(<FeedRows task={task} items={[notice("n1", true)]} showThinking={false} bgs={[]} />);
    expect(screen.getByText("Unity Editor")).toBeTruthy();
    expect(screen.getByText(/dismissed/)).toBeTruthy();
  });
});

describe("attention notice exit animation", () => {
  const attention = (key = "t1:done", needs: Notice["needs"] = "done"): Notice => ({ key, task_id: "t1", needs, title: "Finished", body: "Done", at: 0 });

  it.each(["dismiss", "open", "timeout"] as const)("animates %s without delaying store updates", async (action) => {
    vi.useFakeTimers();
    vi.spyOn(api, "task").mockResolvedValue({ summary: task, items: [], sub_items: {}, bg: [] } as Awaited<ReturnType<typeof api.task>>);
    vi.spyOn(api, "draftGet").mockResolvedValue("");
    set({ notices: [attention()], view: "home", task: null });
    render(<Notices />);
    if (action === "timeout") act(() => { vi.advanceTimersByTime(6000); });
    else await act(async () => { fireEvent.click(screen.getByRole("button", { name: action === "open" ? "Open" : "Dismiss" })); });
    expect(get().notices).toEqual([]);
    if (action === "open") expect(get().task).toBe("t1");
    const card = screen.getByText("Finished").closest(".notice")!;
    expect(card.className).toContain("notice-exiting");
    expect(card.hasAttribute("inert")).toBe(true);
    expect(screen.queryByRole("status")).toBeNull();
    act(() => { vi.advanceTimersByTime(NOTICE_EXIT_MS - 1); });
    expect(screen.getByText("Finished")).toBeTruthy();
    act(() => { vi.advanceTimersByTime(1); });
    expect(screen.queryByText("Finished")).toBeNull();
    expect(document.querySelector(".notices")).toBeNull();
  });

  it("does not restart an exit when another notice arrives", () => {
    vi.useFakeTimers();
    set({ notices: [attention()] });
    render(<Notices />);
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    act(() => { vi.advanceTimersByTime(100); set({ notices: [{ ...attention("second"), title: "Second" }] }); });
    act(() => { vi.advanceTimersByTime(NOTICE_EXIT_MS - 100); });
    expect(screen.queryByText("Finished")).toBeNull();
    expect(screen.getByText("Second")).toBeTruthy();
  });

  it("finishes staggered dismissals on independent timers", () => {
    vi.useFakeTimers();
    set({ notices: [attention(), { ...attention("second"), title: "Second" }] });
    render(<Notices />);
    fireEvent.click(screen.getAllByRole("button", { name: "Dismiss" })[0]!);
    act(() => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    act(() => { vi.advanceTimersByTime(NOTICE_EXIT_MS - 100); });
    expect(screen.queryByText("Finished")).toBeNull();
    expect(screen.getByText("Second")).toBeTruthy();
    act(() => { vi.advanceTimersByTime(100); });
    expect(screen.queryByText("Second")).toBeNull();
  });

  it("cancels removal when the same notice becomes active again", () => {
    vi.useFakeTimers();
    set({ notices: [attention()] });
    render(<Notices />);
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    act(() => { set({ notices: [attention()] }); vi.advanceTimersByTime(NOTICE_EXIT_MS); });
    expect(screen.getByRole("status").className).not.toContain("notice-exiting");
    expect(screen.getByText("Finished")).toBeTruthy();
  });

  it("skips the exit delay for reduced motion", () => {
    vi.useFakeTimers();
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: true })));
    try {
      set({ notices: [attention()] });
      render(<Notices />);
      fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
      act(() => { vi.advanceTimersByTime(0); });
      expect(screen.queryByText("Finished")).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

describe("notice delivery and restart", () => {
  it("notifies in the background, respects desktop notification settings, and never uses question copy", () => {
    const event = { task_id: "t1", kind: "notice", item_id: "n1" };
    expect(surfaces(event, "Unity work", "private details", false, true)).toContain("desktop");
    expect(surfaces(event, "Unity work", "private details", false, false, false)).toEqual(["inapp"]);
    expect(describeAttention(event, "Unity work", "private details")?.title).toBe("Unity work has an update");
    expect(toastBody(event, {})).toBe("The agent left a notice — no reply needed");
    expect(toastBody(event, {})).not.toContain("private details");
  });

  it("merges boot notices without losing full transcripts or overwriting a live dismissal", () => {
    const live = { t1: [{ ...notice("text"), kind: "text" as const }, notice("n1", true)] };
    const merged = mergeNoticeSnapshot(live, { t1: [notice()], t2: [notice("n2")] });
    expect(merged.t1).toEqual(live.t1);
    expect(merged.t2).toEqual([notice("n2")]);
  });

  it("restores saved pending cards on boot, delivers new ones live, and removes only a dismissed notice", async () => {
    vi.useFakeTimers();
    vi.spyOn(api, "boot").mockResolvedValue({ settings: {}, tasks: [task], user_notices: { t1: [notice()] } } as unknown as Awaited<ReturnType<typeof api.boot>>);
    vi.spyOn(api, "draftGet").mockResolvedValue("");
    vi.spyOn(api, "project").mockResolvedValue({} as Awaited<ReturnType<typeof api.project>>);
    await boot();
    set({ view: "home", task: null });
    render(<Notices />);
    expect(screen.getAllByText("Unity Editor")).toHaveLength(1);
    act(() => {
      handlers.get("ol://event")?.({ payload: { task_id: "t1", kind: "item", payload: notice("n2") } });
      handlers.get("ol://attention")?.({ payload: { task_id: "t1", kind: "notice", item_id: "n2" } });
    });
    expect(screen.getAllByText("Unity Editor")).toHaveLength(2);
    expect(get().notices).toEqual([]);
    act(() => { handlers.get("ol://event")?.({ payload: { task_id: "t1", kind: "item", payload: notice("n1", true) } }); });
    expect(screen.getAllByText("Unity Editor")).toHaveLength(2);
    expect(screen.getAllByRole("status", { name: "Agent notice" })).toHaveLength(1);
    act(() => { vi.advanceTimersByTime(NOTICE_EXIT_MS); });
    expect(screen.getAllByText("Unity Editor")).toHaveLength(1);
    expect(pendingNotices(get().items.t1).map((n) => n.id)).toEqual(["n2"]);
  });
});
