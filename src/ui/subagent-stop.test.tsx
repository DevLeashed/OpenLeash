// @vitest-environment jsdom
// Stop on one subagent: its panel offers the button only while that agent is
// running, and the press stops that agent alone (by its own id).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api, type SubInfo, type TaskSummary } from "../api";
import { set } from "../store";
import { SubPanel } from "./Session";

const task = { id: "t1", title: "chat", subs: [], cwd: "D:/temporary", model: "", effort: 2, status: "running", busy: 0, usage: { cost: 0, last_context: 0, parts: {} }, context_window: 0 } as unknown as TaskSummary;
const sub = (status: string) => ({
  id: "s1", role: "explore", task: "look around", status, meta: "", model: "", serving: "", started: null, report: "", background: false,
}) as SubInfo;

beforeEach(() => {
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener() {}, removeEventListener() {} })));
  set({ subItems: {} });
  vi.spyOn(api, "subTranscript").mockResolvedValue([]);
  vi.spyOn(api, "subReport").mockResolvedValue("");
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("stopping one subagent", () => {
  it("offers Stop while the subagent is running, and stops only that subagent", async () => {
    const stop = vi.spyOn(api, "subStop").mockResolvedValue(undefined);
    render(<SubPanel task={task} sub={sub("running")} />);
    fireEvent.click(screen.getByLabelText("Stop subagent"));
    await waitFor(() => expect(stop).toHaveBeenCalledWith("t1", "s1"));
    expect(stop).toHaveBeenCalledTimes(1);
  });

  it("does not offer Stop once the subagent has finished", () => {
    render(<SubPanel task={task} sub={sub("done")} />);
    expect(screen.queryByLabelText("Stop subagent")).toBeNull();
  });
});
