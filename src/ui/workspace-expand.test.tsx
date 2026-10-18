// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { api, type Settings, type TaskSummary } from "../api";
import { set } from "../store";
import { Sidebar } from "./Chrome";
import { Session } from "./Session";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./Composer", () => ({ Composer: () => null, PendingStar: () => null }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

/** The workspace expand control used to toggle state nothing read, so the
 *  artifict surface could never be larger than its share of the split. */
it("lets the artifict surface take the whole window, and hands the chat back", () => {
  (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver;
  vi.spyOn(api, "review").mockResolvedValue({ git: false, files: [] });
  const task = { id: "wide-chat", title: "Chat", status: "stopped", model: "test/model", project: "D:/dummy", subs: [], pending: {}, todos: [], cost: 0, tokens: {}, usage: { cost: 0, last_context: 0, cache_read: 0 }, context_window: 200000 } as unknown as TaskSummary;
  set({ task: task.id, tasks: { [task.id]: task }, items: { [task.id]: [] }, settings: { plugins: { browser: { enabled: false } } } as unknown as Settings, details: false, find: false });
  const { container } = render(<Session />);
  fireEvent.click(screen.getByRole("button", { name: "Artificts" }));
  const chat = container.querySelector(".collaboration-chat") as HTMLElement;
  // The studio gets most of the split by default; the chat keeps a column.
  expect(chat.style.flex).toBe("0 0 26%");
  expect(chat.style.display).not.toBe("none");

  fireEvent.click(screen.getByRole("button", { name: "Expand artifict editor" }));
  expect(chat.style.display).toBe("none");

  fireEvent.click(screen.getByRole("button", { name: "Collapse artifict editor" }));
  expect(chat.style.display).not.toBe("none");
  expect(chat.style.flex).toBe("0 0 26%");

  // Leaving the workspace restores the chat even from an expanded surface.
  fireEvent.click(screen.getByRole("button", { name: "Expand artifict editor" }));
  fireEvent.click(screen.getByRole("button", { name: "Chat" }));
  expect(chat.style.display).not.toBe("none");
});

it("forces the chat rail collapsed while a chat is on its artificts surface, and leaves the hover peek", () => {
  (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver;
  const task = { id: "t1", artifict_open: false } as unknown as TaskSummary;
  set({ sidebar: true, view: "session", task: "t1", tasks: { t1: task }, items: {} });
  const first = render(<Sidebar />);
  expect(first.container.querySelector(".sidebar")?.getAttribute("style")).toContain("246");
  expect(first.container.querySelector(".peekzone")).toBeNull();
  first.unmount();

  // The user's own preference is never touched, so the rail is simply back when
  // this chat leaves its artificts surface.
  set({ tasks: { t1: { ...task, artifict_open: true } as unknown as TaskSummary } });
  const second = render(<Sidebar />);
  expect(second.container.querySelector(".sidebar")?.getAttribute("style")).toContain("width: 0px");
  expect(second.container.querySelector(".peekzone")).toBeTruthy();

  // And another chat's answer is its own: the flag is per chat, not a mode.
  set({ task: "t2", tasks: { t1: { ...task, artifict_open: true } as unknown as TaskSummary, t2: { id: "t2", artifict_open: false } as unknown as TaskSummary } });
  const third = render(<Sidebar />);
  expect(third.container.querySelector(".sidebar")?.getAttribute("style")).toContain("246");
});

it("takes the subagent and background rail out of the way while the record surface is open", () => {
  (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver;
  vi.spyOn(api, "review").mockResolvedValue({ git: false, files: [] });
  const task = { id: "rails-chat", title: "Chat", status: "stopped", model: "test/model", project: "D:/dummy", subs: [], pending: {}, todos: [], cost: 0, tokens: {}, usage: { cost: 0, last_context: 0, cache_read: 0 }, context_window: 200000 } as unknown as TaskSummary;
  set({ task: task.id, tasks: { [task.id]: task }, items: { [task.id]: [] }, settings: { plugins: { browser: { enabled: false } } } as unknown as Settings, details: true, find: false });
  const { container } = render(<Session />);
  expect(screen.getByRole("button", { name: "Details" })).toBeTruthy();
  expect(container.querySelector(".details")).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Artificts" }));
  expect(screen.queryByRole("button", { name: "Details" })).toBeNull();
  expect(container.querySelector(".details")).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Chat" }));
  expect(screen.getByRole("button", { name: "Details" })).toBeTruthy();
});

it("gives the rail back on the new-task screen even though the last chat was on its artificts", () => {
  (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver;
  // New chat from a studio: the view is Home, but `task` still names that chat.
  set({ sidebar: true, view: "home", task: "t1", tasks: { t1: { id: "t1", artifict_open: true } as unknown as TaskSummary }, items: {} });
  const { container } = render(<Sidebar />);
  expect(container.querySelector(".sidebar")?.getAttribute("style")).toContain("246");
});
