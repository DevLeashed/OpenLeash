// @vitest-environment jsdom
/// <reference types="node" />
// The question form autosaves its answers to disk: a 250ms debounce while you
// type, plus a flush when the card unmounts so nothing typed in the last 250ms
// is lost. Those two must coexist. They did not: the flush effect listed the
// form state in its deps, so its cleanup ran on every keystroke and every
// character typed became an IPC write — the debounce was decorative.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { api } from "../api";
import { Question } from "./Question";
import type { Item, TaskSummary } from "../api";

const task = { id: "t1" } as unknown as TaskSummary;

const form = (): Item =>
  ({
    id: "q1",
    kind: "question",
    text: "",
    ts: "2026-09-01T00:00:00Z",
    data: {
      questions: [
        { question: "What should we name it?", type: "single", options: [{ label: "Alpha" }, { label: "Beta" }] },
        { question: "Anything else?", type: "text" },
      ],
    },
  }) as Item;

const draftSet = vi.spyOn(api, "draftSet").mockResolvedValue(undefined);
const respond = vi.spyOn(api, "respond").mockResolvedValue(undefined);

function show() {
  render(<Question it={form()} task={task} />);
}

describe("the question draft autosave", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    draftSet.mockClear();
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("does not write to disk on every keystroke", () => {
    show();
    fireEvent.click(screen.getByText("Alpha"));
    fireEvent.click(screen.getByText("Next"));
    const box = screen.getByPlaceholderText(/type your answer/i);
    fireEvent.change(box, { target: { value: "h" } });
    fireEvent.change(box, { target: { value: "hi" } });
    fireEvent.change(box, { target: { value: "hi!" } });

    expect(draftSet).not.toHaveBeenCalled();
    act(() => { vi.advanceTimersByTime(300); });
    expect(draftSet).toHaveBeenCalledTimes(1);
  });

  it("still saves the typed answer, so the debounce did not swallow it", () => {
    show();
    fireEvent.click(screen.getByText("Alpha"));
    act(() => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Next"));
    fireEvent.change(screen.getByPlaceholderText(/type your answer/i), { target: { value: "ship it" } });
    act(() => { vi.advanceTimersByTime(300); });
    expect(draftSet).toHaveBeenCalledTimes(1);
    const [, body] = draftSet.mock.calls[0]!;
    expect(body).toContain("ship it");
  });

  it("flushes the pending draft when the card goes away mid-debounce", () => {
    show();
    fireEvent.click(screen.getByText("Alpha"));
    act(() => { vi.advanceTimersByTime(100); });
    fireEvent.click(screen.getByText("Next"));
    fireEvent.change(screen.getByPlaceholderText(/type your answer/i), { target: { value: "unfinished" } });

    // Still inside the 250ms window: nothing has been written yet...
    expect(draftSet).not.toHaveBeenCalled();
    // ...and switching away must not lose it.
    cleanup();
    expect(draftSet).toHaveBeenCalledTimes(1);
    const [, body] = draftSet.mock.calls[0]!;
    expect(body).toContain("unfinished");
  });

  // The first page is a choice, so reaching the free-text page means picking
  // an option and pressing Next. The choice question has a "Your own choice…"
  // box, so `getByPlaceholderText(/type your answer/i)` only matches once we
  // are on page 2.
  const toTextPage = () => {
    fireEvent.click(screen.getByText("Alpha"));
    fireEvent.click(screen.getByText("Next"));
    return screen.getByPlaceholderText(/type your answer/i);
  };

  it("unmounting does not write the form a second time", () => {
    show();
    fireEvent.change(toTextPage(), { target: { value: "x" } });
    act(() => { vi.advanceTimersByTime(300); });
    expect(draftSet).toHaveBeenCalledTimes(1);
    cleanup();
    expect(draftSet, "a saved draft should not be re-written on the way out").toHaveBeenCalledTimes(1);
  });
});

// Skipping has to work on a question the agent marked required, and the form
// has to tell the backend which questions were skipped: an empty answer alone
// can't be told apart from an optional field the user left blank on purpose.
describe("skipping a question", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    draftSet.mockClear();
    respond.mockClear();
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  const one = () =>
    ({
      id: "q1",
      kind: "question",
      text: "",
      ts: "2026-09-01T00:00:00Z",
      data: {
        // required defaults to true, which used to hide the Skip button.
        questions: [
          { question: "Which database?", type: "single", required: true, options: [{ label: "Postgres" }, { label: "SQLite" }] },
          { question: "Anything else?", type: "text" },
        ],
      },
    }) as Item;

  const renderOne = () => render(<Question it={one()} task={task} />);

  it("lets a single-choice question accept and submit multiple picks", () => {
    renderOne();
    fireEvent.click(screen.getByRole("button", { name: "Allow multiple" }));
    fireEvent.click(screen.getByText("Postgres"));
    fireEvent.click(screen.getByText("SQLite"));
    expect(screen.getByRole("checkbox", { name: /Postgres/ }).getAttribute("aria-checked")).toBe("true");
    expect(screen.getByRole("checkbox", { name: /SQLite/ }).getAttribute("aria-checked")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.change(screen.getByPlaceholderText(/type your answer/i), { target: { value: "ship it" } });
    fireEvent.click(screen.getByRole("button", { name: "Submit 2" }));
    const [, , response] = respond.mock.calls[0]!;
    expect(response).toEqual({ answers: [["Postgres", "SQLite"], "ship it"], notes: ["", ""], skipped: [] });
  });

  it("returns to single choice with only the first pick retained", () => {
    renderOne();
    fireEvent.click(screen.getByRole("button", { name: "Allow multiple" }));
    fireEvent.click(screen.getByText("Postgres"));
    fireEvent.click(screen.getByText("SQLite"));
    fireEvent.click(screen.getByRole("button", { name: "Use single choice" }));
    expect(screen.getByRole("radio", { name: /Postgres/ }).getAttribute("aria-checked")).toBe("true");
    expect(screen.getByRole("radio", { name: /SQLite/ }).getAttribute("aria-checked")).toBe("false");
  });

  it("offers Skip on a required question, and S skips the page and moves on", () => {
    renderOne();
    expect(screen.getByRole("button", { name: "Skip" })).toBeTruthy();
    fireEvent.keyDown(document.querySelector(".card-q")!, { key: "s" });
    // The page is left behind as skipped rather than blocking on an answer, and
    // the form moves to the next question.
    expect(screen.getByText("2 of 2")).toBeTruthy();
    expect(screen.getByText("Anything else?")).toBeTruthy();
    // Going back shows the skip, and the option is still there to change.
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(screen.getByText(/skipped — the agent will decide/i)).toBeTruthy();
    expect(screen.getByText("Postgres")).toBeTruthy();
  });

  it("skips and answers in one submit, naming both", () => {
    renderOne();
    fireEvent.click(screen.getByText("Postgres"));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.change(screen.getByPlaceholderText(/type your answer/i), { target: { value: "ship it" } });
    // Skipping the last page submits the form rather than sitting on it.
    fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    expect(respond).toHaveBeenCalledTimes(1);
    const [, itemId, response] = respond.mock.calls[0]!;
    expect(itemId).toBe("q1");
    expect(response).toEqual({ answers: ["Postgres", null], notes: ["", ""], skipped: [1] });
  });

  it("undoes a skip so the question can be answered after all", () => {
    renderOne();
    fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    // Skipping page 1 moves to page 2; Back returns to it with the skip shown.
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(screen.getByText(/skipped — the agent will decide/i)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Undo skip" }));
    expect(screen.queryByText(/skipped — the agent will decide/i)).toBeNull();
    expect(screen.getByRole("button", { name: "Skip" })).toBeTruthy();
  });

  it("marks a skip in the saved draft, so it survives a restart", () => {
    renderOne();
    fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    act(() => { vi.advanceTimersByTime(300); });
    const [, body] = draftSet.mock.calls.at(-1)!;
    expect(JSON.parse(body).ans[0].skipped).toBe(true);
  });
});

// The pencil that opens an option's extension sits inside the option's own row,
// so it has to stop its click: bubbling would re-pick the option behind it, and
// on a multi question that would take the pick back off again.
describe("the option extension pencil", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    draftSet.mockClear();
    respond.mockClear();
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  const multi = () =>
    ({
      id: "q1",
      kind: "question",
      text: "",
      ts: "2026-09-01T00:00:00Z",
      data: { questions: [{ question: "Which database?", type: "multi", options: [{ label: "Postgres" }, { label: "SQLite" }] }] },
    }) as Item;

  it("only appears once an option is picked, then opens the box for that option", () => {
    render(<Question it={multi()} task={task} />);
    expect(screen.queryByRole("button", { name: /extend/i })).toBeNull();

    fireEvent.click(screen.getByText("Postgres"));
    const pencil = screen.getByRole("button", { name: "Extend “Postgres”" });
    fireEvent.click(pencil);
    const box = screen.getByPlaceholderText(/extend this/i);
    fireEvent.change(box, { target: { value: "the existing cluster" } });

    // The click did not bubble to the row, so the pick survived it...
    expect(screen.getByRole("checkbox", { name: /Postgres/ }).getAttribute("aria-checked")).toBe("true");
    act(() => { vi.advanceTimersByTime(300); });
    // ...and the extension belongs to the option it was opened from.
    expect(respond).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Submit" }));
    const [, , response] = respond.mock.calls[0]!;
    expect((response as { answers: unknown[] }).answers[0]).toEqual([{ label: "Postgres", note: "the existing cluster" }]);
  });

  it("keeps a second option's box to itself on a multi question", () => {
    render(<Question it={multi()} task={task} />);
    fireEvent.click(screen.getByText("Postgres"));
    fireEvent.click(screen.getByText("SQLite"));
    fireEvent.click(screen.getByRole("button", { name: "Extend “SQLite”" }));
    expect(screen.getAllByPlaceholderText(/extend this/i)).toHaveLength(1);

    fireEvent.change(screen.getByPlaceholderText(/extend this/i), { target: { value: "just for testing" } });
    fireEvent.click(screen.getByRole("button", { name: "Submit" }));
    const [, , response] = respond.mock.calls[0]!;
    expect((response as { answers: unknown[] }).answers[0]).toEqual(["Postgres", { label: "SQLite", note: "just for testing" }]);
  });

  // The user types their own choice out in full, so a note beside it would only
  // repeat it. The question's own note is where that goes.
  it("offers no pencil on the own-choice box, and sends that answer bare", () => {
    render(<Question it={multi()} task={task} />);
    const own = screen.getByPlaceholderText(/your own choices/i);
    fireEvent.change(own, { target: { value: "Redis" } });
    expect(screen.queryByRole("button", { name: /extend your own/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /extend/i })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Submit" }));
    const [, , response] = respond.mock.calls[0]!;
    expect((response as { answers: unknown[] }).answers[0]).toEqual(["Redis"]);
  });
});
