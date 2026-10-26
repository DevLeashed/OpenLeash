// @vitest-environment jsdom
// A subagent's panel shows what it was briefed with, even in a chat opened after
// the app started. `task_open` sends only the main timeline, so the sub-agent
// transcript has to be read when its panel opens; before that, a reopened chat
// showed "No transcript saved" and no brief at all.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type Item, type SubInfo, type TaskSummary } from "../api";
import { loadSubTranscript, mergeSubTranscript, set, get } from "../store";
import { SubPanel } from "./Session";

const task = { id: "t1", title: "chat", subs: [], cwd: "D:/temporary", model: "", effort: 2, status: "idle", busy: 0, usage: { cost: 0, last_context: 0, parts: {} }, context_window: 0 } as unknown as TaskSummary;
const doneSub = {
  id: "s1", role: "explore", task: "look around", status: "done", meta: "", model: "", serving: "", started: null, report: "", background: false,
} as SubInfo;

const brief = { id: "brief", kind: "user", text: "Read the fixture and report what it says.", ts: "2026-10-16T00:00:00Z", data: { brief: true } } as Item;
const reply = { id: "reply", kind: "text", text: "The fixture says hello.", ts: "2026-10-16T00:00:01Z", data: {} } as Item;

beforeEach(() => {
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener() {}, removeEventListener() {} })));
  set({ subItems: {} });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("a subagent's transcript in a chat opened after launch", () => {
  it("shows the brief the subagent was launched with", async () => {
    const read = vi.spyOn(api, "subTranscript").mockResolvedValue([brief, reply]);
    render(<SubPanel task={task} sub={doneSub} />);
    expect(await screen.findByText("Brief from the main agent")).toBeTruthy();
    expect(screen.getByText("Read the fixture and report what it says.")).toBeTruthy();
    expect(screen.getByText("The fixture says hello.")).toBeTruthy();
    expect(read).toHaveBeenCalledWith("t1", "s1");
    expect(screen.queryByText("No transcript saved for this subagent.")).toBeNull();
  });

  it("does not claim there is no transcript while it is still being read", async () => {
    let finish: (items: Item[]) => void = () => {};
    vi.spyOn(api, "subTranscript").mockReturnValue(new Promise<Item[]>((resolve) => { finish = resolve; }));
    render(<SubPanel task={task} sub={doneSub} />);
    expect(screen.queryByText("No transcript saved for this subagent.")).toBeNull();
    finish([brief]);
    expect(await screen.findByText("Brief from the main agent")).toBeTruthy();
  });

  it("says there is no transcript once the read has come back empty", async () => {
    vi.spyOn(api, "subTranscript").mockResolvedValue([]);
    render(<SubPanel task={task} sub={doneSub} />);
    expect(await screen.findByText("No transcript saved for this subagent.")).toBeTruthy();
  });

  // A brief that says "check this screenshot" and shows nothing leaves the user
  // guessing whether the subagent was given the right picture, so the pictures
  // that went with the brief are drawn in the panel.
  it("shows the pictures the brief was sent with", async () => {
    const pic = "data:image/png;base64,iVBORw0KGgo=";
    const withPic = { ...brief, data: { brief: true, has_images: 1, images: [pic] } } as Item;
    vi.spyOn(api, "subTranscript").mockResolvedValue([withPic]);
    render(<SubPanel task={task} sub={doneSub} />);
    expect(await screen.findByText("Brief from the main agent")).toBeTruthy();
    const img = document.querySelector(".brief .ubimgs img") as HTMLImageElement | null;
    expect(img, "the brief draws the picture it was given").toBeTruthy();
    expect(img!.getAttribute("src")).toBe(pic);
  });
});

describe("loadSubTranscript", () => {
  it("keeps live items that arrived while the read was in flight", async () => {
    const live = { id: "live", kind: "text", text: "arrived live", ts: "2026-10-16T00:00:02Z", data: {} } as Item;
    set({ subItems: { t1: { s1: [live] } } });
    vi.spyOn(api, "subTranscript").mockResolvedValue([brief]);
    await loadSubTranscript("t1", "s1");
    expect(get().subItems.t1?.s1?.map((it) => it.id)).toEqual(["brief", "live"]);
  });

  it("prefers the live copy of an item both sides have", () => {
    const stale = { ...reply, text: "partial" };
    const newer = { ...reply, text: "The fixture says hello, in full." };
    expect(mergeSubTranscript([newer], [stale])).toEqual([newer]);
    expect(mergeSubTranscript(undefined, [brief, stale])).toEqual([brief, stale]);
  });
});
