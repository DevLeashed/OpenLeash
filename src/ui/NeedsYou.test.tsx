// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { api, type Item, type TaskSummary } from "../api";
import { get, set } from "../store";
import { NeedsYou } from "./NeedsYou";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const task = (kind: "question" | "approval"): TaskSummary => ({
  id: `chat-${kind}`, title: `${kind} chat`, status: "waiting", waiting_kind: kind, paused: null,
} as TaskSummary);
const item = (kind: "question" | "approval"): Item => kind === "question"
  ? { id: "question-id", kind, text: "", data: { questions: [{ question: "Which database should I use?", type: "single", options: [] }] }, ts: new Date(Date.now() - 125_000).toISOString() }
  : { id: "approval-id", kind, text: "", data: { kind: "command", detail: "npm test" }, ts: new Date(Date.now() - 125_000).toISOString() };

it.each(["question", "approval"] as const)("shows a pending %s preview and opens its existing decision card", async (kind) => {
  const waiting = task(kind);
  const pending = item(kind);
  const fetchTask = vi.spyOn(api, "task").mockResolvedValue({ summary: waiting, items: [pending], sub_items: {}, bg: [] });
  set({ tasks: { [waiting.id]: waiting }, items: {}, unread: { [waiting.id]: true }, view: "home", task: null });

  render(<NeedsYou />);
  const entry = screen.getByRole("button", { name: /Needs you inbox · 1 pending/ });
  expect(entry.classList.contains("has-pending")).toBe(true);
  fireEvent.click(entry);
  expect(await screen.findByText(kind === "question" ? "Which database should I use?" : "npm test")).toBeTruthy();
  expect(screen.getByText("Waiting 2m")).toBeTruthy();
  expect(fetchTask).toHaveBeenCalledTimes(1); // Preview only after opening the inbox.
  expect(get().unread[waiting.id], "opening the inbox does not acknowledge the chat").toBe(true);

  fireEvent.click(screen.getByRole("button", { name: `Open ${waiting.title} · ${kind}` }));
  await waitFor(() => expect(get().view).toBe("session"));
  expect(get().task).toBe(waiting.id);
  expect(get().unread[waiting.id], "the existing blocking wait keeps its unread semantics").toBe(true);
  expect(fetchTask).toHaveBeenCalledTimes(2); // Normal chat navigation re-loads the session.
  expect(fetchTask).toHaveBeenNthCalledWith(2, waiting.id);
});

it("keeps an entry visible at zero and Escape closes the drawer without navigation", () => {
  set({ tasks: {}, view: "home", task: null });
  render(<NeedsYou />);
  const entry = screen.getByRole("button", { name: /Needs you inbox · 0 pending/ });
  expect(entry.classList.contains("has-pending")).toBe(false);
  entry.focus(); // JSDOM's fireEvent.click does not apply the browser's native focus default.
  fireEvent.click(entry);
  expect(screen.getByRole("dialog", { name: "Needs you inbox" })).toBeTruthy();
  fireEvent.keyDown(window, { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Needs you inbox" })).toBeNull();
  expect(get().view).toBe("home");
  expect(document.activeElement).toBe(entry);
});
