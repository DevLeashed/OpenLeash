// @vitest-environment jsdom
// The answered-card send-off is a detached copy: the answer must go out at once
// (never held back for the animation), the copy must clean itself up, it must not
// pass for a live question, and reduced motion gets no copy at all.
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { api, type Item, type TaskSummary } from "../api";
import { ghostOut, Question } from "./Question";

const task = { id: "t1" } as unknown as TaskSummary;
const item = (answers?: unknown[]): Item => ({
  id: "q1", kind: "question", text: "", ts: "2026-09-01T00:00:00Z",
  data: { questions: [{ question: "Pick one", type: "single", options: [{ label: "Alpha" }, { label: "Beta" }] }], ...(answers ? { answers } : {}) },
}) as Item;
const sized = () => vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({ left: 10, top: 20, width: 300, height: 120, right: 310, bottom: 140, x: 10, y: 20, toJSON: () => ({}) } as DOMRect);

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.useRealTimers(); document.querySelectorAll(".q-ghost-frame").forEach((n) => n.remove()); });

it("sends the answer immediately and floats a non-interactive copy that removes itself", () => {
  vi.useFakeTimers();
  sized();
  vi.spyOn(api, "draftSet").mockResolvedValue(undefined);
  const respond = vi.spyOn(api, "respond").mockResolvedValue(undefined);
  render(<Question it={item()} task={task} />);
  fireEvent.click(screen.getByText("Alpha"));
  fireEvent.click(screen.getByRole("button", { name: "Submit" }));
  expect(respond).toHaveBeenCalledTimes(1);
  const ghost = document.querySelector<HTMLElement>(".q-ghost-frame .q-ghost")!;
  expect(ghost).toBeTruthy();
  expect(ghost.getAttribute("aria-hidden")).toBe("true");
  expect(ghost.hasAttribute("data-pending-question")).toBe(false);
  expect(ghost.hasAttribute("tabindex")).toBe(false);
  vi.advanceTimersByTime(1300);
  expect(document.querySelector(".q-ghost-frame")).toBeNull();
});

it("eases in only an answer given in this view, not answers loaded with the transcript", () => {
  vi.spyOn(api, "draftSet").mockResolvedValue(undefined);
  vi.spyOn(api, "respond").mockResolvedValue(undefined);
  const { container, rerender } = render(<Question it={item()} task={task} />);
  fireEvent.click(screen.getByText("Alpha"));
  fireEvent.click(screen.getByRole("button", { name: "Submit" }));
  rerender(<Question it={item(["Alpha"])} task={task} />);
  expect(container.querySelector(".resolved.fresh")).toBeTruthy();
  cleanup();
  const loaded = render(<Question it={item(["Alpha"])} task={task} />);
  expect(loaded.container.querySelector(".resolved")).toBeTruthy();
  expect(loaded.container.querySelector(".resolved.fresh")).toBeNull();
});

it("draws nothing under reduced motion", () => {
  sized();
  const prev = window.matchMedia;
  window.matchMedia = ((q: string) => ({ matches: q.includes("reduce") })) as unknown as typeof window.matchMedia;
  try {
    const card = document.createElement("div");
    document.body.appendChild(card);
    ghostOut(card);
    expect(document.querySelector(".q-ghost-frame")).toBeNull();
    card.remove();
  } finally { window.matchMedia = prev; }
});
