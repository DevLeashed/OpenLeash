// @vitest-environment jsdom
// The composer beside an Artificts studio.
//
// Three things it owes the user there: a new chat started from a studio is
// about that artifict (and opens on it); a message sent from a chat whose
// studio is open tells the agent which revision is on screen, once per change,
// without that note showing in the user's bubble; and the bar carries only
// what a message about a design needs.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, createEvent, fireEvent, render, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type Settings, type TaskSummary } from "../api";
import { get, set } from "../store";
import { Composer } from "./Composer";
import { stripHarnessBlocks } from "./SelectionNote";
import { studioHandoff } from "./studioHandoff";

const chat = (over: Partial<TaskSummary> = {}) => ({
  id: "t1", title: "chat", titled: false, status: "idle", step: "", project: "D:/proj", cwd: ".", branch: "", base_branch: "main",
  worktree: false, model: "anthropic/claude-opus-5", effort: 2, perm: "auto", plan: false, ultra: false,
  ultra_wt: false, ultra_x: null, subagents: true, goal: null, assist: "default", agents: [],
  pending: {}, paused: null, serving: "", route: "", unpaused: false, busy: 0, bg_now: [], archived: false, hidden: false,
  unread: false, forked_from: null, pinned: false, artifict_open: true, order: 0, todos: [], subs: [],
  usage: { input: 0, output: 0, cache_read: 0, cache_write: 0, cost: 0, last_context: 0 },
  context_window: 200_000, created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-01T00:00:00Z",
  touched_at: "2026-09-01T00:00:00Z", waiting_kind: null, rev: 1, ...over,
} as TaskSummary);

const viewing = { artifictId: "art-1", versionId: "v2", title: "Landing", number: 2, kind: "html" };
const enter = () => {
  const box = document.querySelector(".composer textarea") as HTMLTextAreaElement;
  fireEvent(box, createEvent.keyDown(box, { key: "Enter" }));
};

afterEach(() => { cleanup(); vi.restoreAllMocks(); studioHandoff.clear(); });

describe("a chat whose studio is open", () => {
  const mount = (draft: string, task = chat()) => {
    set({
      view: "session", task: task.id, tasks: { [task.id]: task }, sessionDrafts: { [task.id]: draft }, notes: {},
      attach: {}, attachFiles: {}, pendingMessages: {}, studioView: { [task.id]: viewing },
      settings: { send_with: "enter" } as unknown as Settings,
    });
    return render(<Composer mode="session" />);
  };

  it("tells the agent which revision is on screen, once per change, out of the user's sight", async () => {
    const send = vi.spyOn(api, "send").mockResolvedValue(undefined);
    const first = mount("make the header bigger", chat({ id: "t-view" }));
    enter();
    await waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    const body = send.mock.calls[0]![1];
    expect(body).toContain("id art-1, revision v2, version id v2, kind html");
    expect(stripHarnessBlocks(body)).toBe("make the header bigger");
    first.unmount();
    // Same revision still on screen: the agent already knows, so no note.
    mount("and the footer", chat({ id: "t-view" }));
    enter();
    await waitFor(() => expect(send).toHaveBeenCalledTimes(2));
    expect(send.mock.calls[1]![1]).toBe("and the footer");
  });

  it("keeps only attach, the model and send on its bar, and asks for a plain Message", () => {
    mount("");
    expect(screen.getByRole("button", { name: "Attach files" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Plan mode, Artificts, compact, MCP" })).toBeNull();
    expect(screen.queryByText("Default")).toBeNull();
    expect((document.querySelector(".composer textarea") as HTMLTextAreaElement).placeholder).toBe("Message");
  });
});

describe("a new chat started from a studio", () => {
  it("opens with the user's message without waiting for the studio flag save", async () => {
    vi.spyOn(api, "create").mockResolvedValue(chat({ id: "instant-chat", artifict_open: false }));
    let finishSave!: (value: TaskSummary) => void;
    vi.spyOn(api, "update").mockImplementation(() => new Promise((resolve) => { finishSave = resolve; }));
    // Keep the initial transcript request pending too: the echo is visible even
    // before task_open answers, not just a shortcut around the flag save.
    let finishOpen!: (value: Awaited<ReturnType<typeof api.taskOpen>>) => void;
    vi.spyOn(api, "taskOpen").mockImplementation(() => new Promise((resolve) => { finishOpen = resolve; }));
    set({
      view: "home", task: null, tasks: {}, items: {}, pendingMessages: {}, draft: "make it dark", notes: {}, attach: {}, attachFiles: {},
      settings: { project: "D:/proj", send_with: "enter" } as unknown as Settings,
      home: { ...get().home, artificts: true, studio: null },
    });
    render(<Composer mode="home" />);
    enter();
    await waitFor(() => expect(get().task).toBe("instant-chat"));
    expect(get().view).toBe("session");
    expect(get().tasks["instant-chat"]?.artifict_open).toBe(true);
    expect(stripHarnessBlocks(get().pendingMessages["instant-chat"]![0]!.text)).toBe("make it dark");
    finishSave(chat({ id: "instant-chat", artifict_open: true }));
    finishOpen({ summary: chat({ id: "instant-chat", artifict_open: true }), items: [], bg: [], cursor: null, total: 0 });
  });

  it("is about that artifict, opens on it, and leaves Artificts mode behind on the new-task screen", async () => {
    vi.spyOn(api, "create").mockResolvedValue(chat({ id: "new-chat", artifict_open: false }));
    const update = vi.spyOn(api, "update").mockResolvedValue(chat({ id: "new-chat", artifict_open: true }));
    set({
      view: "home", task: null, draft: "make a dark mode variant", notes: {}, attach: {}, attachFiles: {},
      settings: { project: "D:/proj", send_with: "enter" } as unknown as Settings,
      home: { ...get().home, artificts: true, studio: { taskId: "t1", ...viewing } },
    });
    render(<Composer mode="home" />);
    enter();
    await waitFor(() => expect(api.create).toHaveBeenCalled());
    const req = vi.mocked(api.create).mock.calls[0]![0];
    expect(req.prompt).toContain("started this chat from the Artificts studio");
    expect(req.prompt).toContain("“Landing” (id art-1, revision v2");
    expect(stripHarnessBlocks(req.prompt)).toBe("make a dark mode variant");
    await waitFor(() => expect(update).toHaveBeenCalledWith("new-chat", { artifict_open: true }));
    expect(studioHandoff.get("new-chat")).toEqual({ artifictId: "art-1", versionId: "v2" });
    expect(get().home.artificts).toBe(false);
    expect(get().home.studio).toBeNull();
  });
});
