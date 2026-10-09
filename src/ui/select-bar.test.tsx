/** The sidebar's selection bar: what it says, and — more to the point — what it
 *  counts. The store holds ticks by id and nothing prunes them, so a tick on a
 *  chat that has since been deleted or archived outlives the chat. The bar is
 *  the one thing the user reads before pressing a button that acts on a whole
 *  selection, so if its count is wrong the action is wrong. */
import { describe, expect, it, beforeEach, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import type { TaskSummary } from "../api";
import { set } from "../store";
import { Sidebar } from "./Chrome";

const chat = (id: string, over: Record<string, unknown> = {}) =>
  ({ id, title: id, archived: false, pinned: false, status: "stopped", order: 0, touched_at: "", updated_at: "", ...over }) as unknown as TaskSummary;

/** The "Chats" section header, as plain text: label on the left, controls on the right. */
function chatsHeader(picked: Record<string, true>, tasks: Record<string, TaskSummary>) {
  set({ tasks, picked, selecting: true, sidebar: true, view: "home", task: null, settings: { all_projects: false } as never });
  const html = renderToStaticMarkup(createElement("div", { className: "sidebar-in" }, createElement(Sidebar)));
  const heads = [...html.matchAll(/class="shead"[^>]*>(.*?)<\/div>/gs)].map((m) => m[1]!.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim());
  return heads[heads.length - 1]!;
}

describe("sidebar selection bar", () => {
  beforeEach(() => set({ view: "home", task: null }));

  it("says how to use it when nothing is ticked, rather than offering an action", () => {
    const head = chatsHeader({}, { a: chat("a"), b: chat("b") });
    expect(head).toContain("Esc to cancel");
    expect(head).not.toContain("Archive");
  });

  it("counts the ticked chats and offers to archive them", () => {
    expect(chatsHeader({ a: true }, { a: chat("a"), b: chat("b") })).toContain("1 picked");
    expect(chatsHeader({ a: true, b: true }, { a: chat("a"), b: chat("b") })).toContain("Archive 2");
  });

  // The number on the button is what the user is agreeing to act on. A tick on a
  // chat that has since been archived elsewhere still sits in the store, so a bar
  // that counted it would read "Archive 2" and archive one.
  it("counts only chats that are still there and not archived", () => {
    expect(chatsHeader({ a: true, ghost: true }, { a: chat("a") })).toContain("Archive 1");
    expect(chatsHeader({ a: true, b: true }, { a: chat("a"), b: chat("b", { archived: true }) })).toContain("Archive 1");
  });

  it("shows a singular label for one chat", () => {
    expect(chatsHeader({ a: true }, { a: chat("a") })).toContain("1 picked");
    expect(chatsHeader({ a: true }, { a: chat("a") })).not.toContain("1 picked chats");
  });
});
