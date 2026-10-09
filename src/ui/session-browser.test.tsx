// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { api, type Settings, type TaskSummary } from "../api";
import { set } from "../store";
import { Session } from "./Session";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./BrowserPanel", () => ({ BrowserPanel: ({ taskId, onClose }: { taskId: string; onClose: () => void }) => <div>Browser for {taskId}<button onClick={onClose}>Close test browser</button></div> }));
vi.mock("./Composer", () => ({ Composer: () => null, PendingStar: () => null }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });
it("offers a direct control to clear the current goal", async () => {
  (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver;
  const send = vi.spyOn(api, "send").mockResolvedValue();
  const task = { id: "goal-chat", title: "Chat", status: "stopped", model: "test/model", project: "D:/dummy", subs: [], pending: {}, todos: [], goal: { text: "Ship the fix", status: "active", summary: "", nudges: 0 }, usage: { cost: 0, last_context: 0, cache_read: 0 }, context_window: 200000 } as unknown as TaskSummary;
  set({ task: task.id, tasks: { [task.id]: task }, items: { [task.id]: [] }, settings: { plugins: { browser: { enabled: false } } } as unknown as Settings, details: false, find: false });
  render(<Session />);
  fireEvent.click(screen.getByRole("button", { name: "Remove current goal" }));
  expect(send).toHaveBeenCalledWith(task.id, "/goal clear");
});
it("only offers the same-task browser when its plugin is enabled", () => {
  (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver;
  vi.spyOn(api, "review").mockResolvedValue({ git: false, files: [] });
  const task = { id: "browser-chat", title: "Chat", status: "stopped", model: "test/model", project: "D:/dummy", subs: [], pending: {}, todos: [], cost: 0, tokens: {}, usage: { cost: 0, last_context: 0, cache_read: 0 }, context_window: 200000 } as unknown as TaskSummary;
  const settings = { plugins: { browser: { enabled: false } } } as unknown as Settings;
  set({ task: task.id, tasks: { [task.id]: task }, items: { [task.id]: [] }, settings, details: false, find: false });
  render(<Session />);
  expect(screen.queryByRole("button", { name: "Browser" })).toBeNull();
  cleanup();
  set({ settings: { ...settings, plugins: { ...settings.plugins, browser: { enabled: true, width: 1280, height: 800 } } } });
  render(<Session />);
  fireEvent.click(screen.getByRole("button", { name: "Browser" }));
  expect(screen.getByText("Browser for browser-chat")).toBeTruthy();
  fireEvent.click(screen.getByText("Close test browser"));
  expect(screen.queryByText("Browser for browser-chat")).toBeNull();
});
