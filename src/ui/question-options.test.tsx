// @vitest-environment jsdom
/// <reference types="node" />
// Long option lists on the question cards. An agent can now offer up to 30
// options on one question, and past a handful the list needs a filter, a
// bounded height, and number keys that keep naming the rows you can see:
//
// - a short list is unchanged: no filter box, no scroll cap, 1-9 as before;
// - a long list gets a filter; the digits then name the *visible* rows, so a
//   filtered list renumbers from 1 and a digit never picks something hidden;
// - Enter in the filter picks the first match, and with no match it turns what
//   was typed into the user's own answer;
// - a pick the filter is hiding is counted, so it never silently vanishes.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { api } from "../api";
import { Question } from "./Question";
import { AskNonBlocking } from "./AskNonBlocking";
import { digitCount, digitTarget, FILTER_AT, filterOptions } from "./QuestionFilter";
import type { Item, TaskSummary } from "../api";
import type { Opt } from "./Question";

const task = { id: "t1" } as unknown as TaskSummary;

const LOCALES = ["English", "French", "German", "Spanish", "Italian", "Dutch", "Polish", "Czech", "Swedish", "Danish", "Finnish", "Greek",
  "Turkish", "Arabic", "Hebrew", "Hindi", "Bengali", "Thai", "Vietnamese", "Indonesian", "Malay", "Japanese", "Korean", "Chinese (Simplified)",
  "Chinese (Traditional)", "Portuguese (Brazil)", "Portuguese (Portugal)", "Ukrainian", "Romanian", "Hungarian"];

const options = (n: number): Opt[] => LOCALES.slice(0, n).map((label, i) => ({ label, ...(i === 21 ? { recommended: true } : {}), ...(label === "Korean" ? { description: "Hangul, CJK fonts" } : {}) }));

const item = (kind: "question" | "asklater", opts: Opt[], type: "single" | "multi" = "single"): Item =>
  ({ id: "q1", kind, text: "", ts: "2026-09-01T00:00:00Z", data: { questions: [{ question: "Which locale?", type, options: opts }] } }) as Item;

const respond = vi.spyOn(api, "respond").mockResolvedValue(undefined);
const answerNonBlocking = vi.spyOn(api, "answerNonBlocking").mockResolvedValue(undefined);
vi.spyOn(api, "draftSet").mockResolvedValue(undefined);
vi.spyOn(api, "draftGet").mockResolvedValue("");

const card = () => document.querySelector<HTMLElement>(".card-q")!;
const filter = () => screen.queryByRole("searchbox", { name: "Filter options" }) as HTMLInputElement | null;
const checked = () => screen.queryAllByRole("radio").concat(screen.queryAllByRole("checkbox")).filter((r) => r.getAttribute("aria-checked") === "true").map((r) => r.textContent);
const rows = () => screen.queryAllByRole("radio").concat(screen.queryAllByRole("checkbox")).filter((r) => r.querySelector("[data-opt]"));

describe("filterOptions / digit mapping", () => {
  const opts = options(30);
  it("matches every term against label and description, case-insensitively, in the agent's order", () => {
    expect(filterOptions(opts, "")).toHaveLength(30);
    expect(filterOptions(opts, "chinese").map((i) => opts[i]!.label)).toEqual(["Chinese (Simplified)", "Chinese (Traditional)"]);
    expect(filterOptions(opts, "portuguese BRA").map((i) => opts[i]!.label)).toEqual(["Portuguese (Brazil)"]);
    expect(filterOptions(opts, "hangul").map((i) => opts[i]!.label)).toEqual(["Korean"]);
    // Not a regex: a stray metacharacter is just text that matches nothing.
    expect(filterOptions(opts, "(")).toHaveLength(4);
    expect(filterOptions(opts, ".*")).toEqual([]);
  });
  it("digits name visible rows, then the free-text row, and nothing past 9", () => {
    expect(digitTarget([4, 7], 1, true)).toEqual({ option: 4 });
    expect(digitTarget([4, 7], 2, true)).toEqual({ option: 7 });
    expect(digitTarget([4, 7], 3, true)).toEqual({ other: true });
    expect(digitTarget([4, 7], 4, true)).toBeNull();
    const all = [...Array(30).keys()];
    expect(digitTarget(all, 9, true)).toEqual({ option: 8 });
    expect(digitTarget(all, 10, true)).toBeNull();
    expect(digitCount(all, true)).toBe(9);
    expect(digitCount([1, 2], true)).toBe(3);
  });
});

describe("a long list on the blocking question card", () => {
  beforeEach(() => { respond.mockClear(); });
  afterEach(cleanup);

  it("keeps a short list exactly as it was: no filter, no scroll cap", () => {
    render(<Question it={item("question", options(FILTER_AT))} task={task} />);
    expect(filter()).toBeNull();
    expect(document.querySelector(".opt-scroll")).toBeNull();
    expect(rows()).toHaveLength(FILTER_AT);
    fireEvent.keyDown(card(), { key: "3" });
    expect(checked()).toEqual(["German3"]);
  });

  it("renders all 30 options in a bounded list under a filter box", () => {
    render(<Question it={item("question", options(30))} task={task} />);
    expect(filter()).not.toBeNull();
    expect(document.querySelector(".opt-scroll")).not.toBeNull();
    expect(rows()).toHaveLength(30);
    expect(screen.getByText("30 options")).toBeTruthy();
    // The recommended one stays marked, in place.
    expect(screen.getByText("Recommended").closest("[data-opt]")!.getAttribute("data-opt")).toBe("21");
    expect(screen.getByText(/1-9 to choose · \/ to filter/)).toBeTruthy();
  });

  it("digit keys 1-9 pick the first nine and 10+ are not reachable by a stray key", () => {
    render(<Question it={item("question", options(30))} task={task} />);
    fireEvent.keyDown(card(), { key: "9" });
    expect(checked()).toEqual(["Swedish9"]);
    // "0" is not a row, and the free-text row (31st) has no digit.
    fireEvent.keyDown(card(), { key: "0" });
    expect(checked()).toEqual(["Swedish9"]);
  });

  it("filters, renumbers the visible rows from 1, and submits the real label", () => {
    render(<Question it={item("question", options(30))} task={task} />);
    fireEvent.change(filter()!, { target: { value: "portuguese" } });
    expect(rows().map((r) => r.textContent)).toEqual(["Portuguese (Brazil)1", "Portuguese (Portugal)2"]);
    expect(screen.getByText("2 of 30")).toBeTruthy();
    fireEvent.keyDown(card(), { key: "2" });
    expect(checked()).toEqual(["Portuguese (Portugal)2"]);
    // The free-text row follows the visible ones and takes the next digit.
    expect(screen.getByPlaceholderText("Your own choice…").closest("[role=radio]")!.textContent).toContain("3");
    fireEvent.keyDown(card(), { key: "Enter" });
    expect(respond).toHaveBeenCalledWith("t1", "q1", expect.objectContaining({ answers: ["Portuguese (Portugal)"] }));
  });

  it("'/' focuses the filter, Enter there picks the first match and Escape clears", () => {
    render(<Question it={item("question", options(30))} task={task} />);
    fireEvent.keyDown(card(), { key: "/" });
    expect(document.activeElement).toBe(filter());
    fireEvent.change(filter()!, { target: { value: "kor" } });
    fireEvent.keyDown(filter()!, { key: "Enter" });
    expect(checked()).toEqual(["KoreanHangul, CJK fonts1"]);
    expect(respond).not.toHaveBeenCalled();
    fireEvent.keyDown(filter()!, { key: "Escape" });
    expect(filter()!.value).toBe("");
    expect(rows()).toHaveLength(30);
    // A second Enter with the pick already made carries on: here, submits.
    fireEvent.change(filter()!, { target: { value: "kor" } });
    fireEvent.keyDown(filter()!, { key: "Enter" });
    expect(respond).toHaveBeenCalledWith("t1", "q1", expect.objectContaining({ answers: ["Korean"] }));
  });

  it("with no match, says so and Enter makes the typed text the user's own answer", () => {
    render(<Question it={item("question", options(30))} task={task} />);
    fireEvent.change(filter()!, { target: { value: "Klingon" } });
    expect(rows()).toHaveLength(0);
    expect(screen.getByText(/No options match/)).toBeTruthy();
    fireEvent.keyDown(filter()!, { key: "Enter" });
    expect((screen.getByPlaceholderText("Your own choice…") as HTMLInputElement).value).toBe("Klingon");
    fireEvent.keyDown(card(), { key: "Enter" });
    expect(respond).toHaveBeenCalledWith("t1", "q1", expect.objectContaining({ answers: ["Klingon"] }));
  });

  it("counts picks the filter hides on a multi question and keeps them in the answer", () => {
    render(<Question it={item("question", options(30), "multi")} task={task} />);
    fireEvent.keyDown(card(), { key: "1" });
    fireEvent.change(filter()!, { target: { value: "thai" } });
    expect(screen.getByText(/1 picked option hidden/)).toBeTruthy();
    fireEvent.keyDown(card(), { key: "1" });
    fireEvent.keyDown(card(), { key: "Enter" });
    expect(respond).toHaveBeenCalledWith("t1", "q1", expect.objectContaining({ answers: [["English", "Thai"]] }));
  });

  it("clears the filter when the page changes", () => {
    const it2 = { ...item("question", options(30)), data: { questions: [
      { question: "Which locale?", type: "single", options: options(30) },
      { question: "Which fallback?", type: "single", options: options(30) },
    ] } } as Item;
    render(<Question it={it2} task={task} />);
    fireEvent.change(filter()!, { target: { value: "greek" } });
    fireEvent.keyDown(card(), { key: "1" });
    fireEvent.keyDown(card(), { key: "Enter" });
    expect(screen.getByText("Which fallback?")).toBeTruthy();
    expect(filter()!.value).toBe("");
    expect(rows()).toHaveLength(30);
  });
});

describe("a long list on the non-blocking card", () => {
  beforeEach(() => { answerNonBlocking.mockClear(); });
  afterEach(cleanup);

  it("gets the same filter and visible-row digits", () => {
    render(<AskNonBlocking it={item("asklater", options(30))} task={task} />);
    const body = document.querySelector<HTMLElement>(".al-body")!;
    expect(rows()).toHaveLength(30);
    expect(document.querySelector(".asklater .opt-scroll")).not.toBeNull();
    fireEvent.keyDown(body, { key: "/" });
    expect(document.activeElement).toBe(filter());
    fireEvent.change(filter()!, { target: { value: "hindi" } });
    expect(rows()).toHaveLength(1);
    fireEvent.keyDown(body, { key: "1" });
    fireEvent.click(screen.getByRole("button", { name: /Send answer/ }));
    expect(answerNonBlocking).toHaveBeenCalledWith("t1", "q1", expect.objectContaining({ answers: ["Hindi"] }));
  });

  it("leaves a short list alone", () => {
    render(<AskNonBlocking it={item("asklater", options(3))} task={task} />);
    expect(filter()).toBeNull();
    expect(document.querySelector(".opt-scroll")).toBeNull();
  });
});
