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
it("hosts the plugin's browser as a view of the Artificts surface, not a panel of its own", () => {
  (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver;
  vi.spyOn(api, "review").mockResolvedValue({ git: false, files: [] });
  const task = { id: "browser-chat", title: "Chat", status: "stopped", model: "test/model", project: "D:/dummy", subs: [], pending: {}, todos: [], cost: 0, tokens: {}, usage: { cost: 0, last_context: 0, cache_read: 0 }, context_window: 200000 } as unknown as TaskSummary;
  const settings = { plugins: { browser: { enabled: false } } } as unknown as Settings;
  set({ task: task.id, tasks: { [task.id]: task }, items: { [task.id]: [] }, settings, details: false, find: false });
  render(<Session />);
  // No separate header control: the browser lives behind the workspace now.
  expect(screen.queryByRole("button", { name: "Browser" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Artificts" }));
  expect(screen.queryByRole("button", { name: "Browser" })).toBeNull();
  cleanup();
  // The chat remembers it was left on its artificts surface — that is saved with
  // the chat — so the second render opens straight into it, and the browser view
  // is simply what the plugin adds to that surface.
  set({ settings: { ...settings, plugins: { ...settings.plugins, browser: { enabled: true, width: 1280, height: 800, persistent_profile: false, headed: false, developer_mode: false, interactions: "ask" as const, download_dir: "" } } } });
  const { container } = render(<Session />);
  const workspace = container.querySelector(".collaboration-workspace")!;
  expect(workspace).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Browser" }));
  // The view renders inside the workspace pane, so it shares the surface's frame.
  expect(workspace.contains(screen.getByText("Browser for browser-chat"))).toBe(true);
  fireEvent.click(screen.getByText("Close test browser"));
  expect(screen.queryByText("Browser for browser-chat")).toBeNull();
});
it("offers the Browser view by default, before any plugin setting has been saved", () => {
  (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver;
  vi.spyOn(api, "review").mockResolvedValue({ git: false, files: [] });
  const task = { id: "default-chat", title: "Chat", status: "stopped", model: "test/model", project: "D:/dummy", subs: [], pending: {}, todos: [], cost: 0, tokens: {}, artifict_open: true, usage: { cost: 0, last_context: 0, cache_read: 0 }, context_window: 200000 } as unknown as TaskSummary;
  // The browser ships on (Rust `BrowserCfg::default`), so a settings object
  // without a plugins block still shows the view.
  set({ task: task.id, tasks: { [task.id]: task }, items: { [task.id]: [] }, settings: {} as unknown as Settings, details: false, find: false });
  render(<Session />);
  fireEvent.click(screen.getByRole("button", { name: "Browser" }));
  expect(screen.getByText("Browser for default-chat")).toBeTruthy();
});
