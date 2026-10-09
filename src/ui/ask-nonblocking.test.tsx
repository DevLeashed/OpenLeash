// @vitest-environment jsdom
/// <reference types="node" />
// A question the agent asked without waiting (`ask_nonblocking`):
//
// 1. It does not block the run, and the UI has to say so. A card that looks
//    like the blocking one, in the same place, with the same buttons, reads as
//    "the agent is stopped until I answer this" — which is a lie, and the user
//    ends up answering a question whose work finished ten minutes ago.
// 2. It is not in the transcript. The card is a view of live state above the
//    send bar, so the transcript filter has to drop the item or it shows up
//    twice, in two places, with two different states.
// 3. It arrives open, one question per page. Folded, it needed a click before
//    anything could be read; stacked, a batch of ten was a wall that pushed the
//    composer off screen. Both are ways of making an optional question go
//    unread, which is the one outcome this card cannot afford.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { readFileSync } from "node:fs";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { api } from "../api";
import { describe as notifyText, surfaces } from "../notify";
import { noticeMeta } from "./Notices";
import { AskNonBlocking, pendingOf } from "./AskNonBlocking";
import type { Item, TaskSummary } from "../api";

const task = { id: "t1" } as unknown as TaskSummary;

const card = (over: Partial<Item> = {}): Item =>
  ({
    id: "a1",
    kind: "asklater",
    text: "Naming",
    ts: "2026-09-01T00:00:00Z",
    data: {
      title: "Naming",
      questions: [
        { question: "What should the crate be called?", type: "single", options: [{ label: "openleash" }, { label: "leash" }] },
      ],
    },
    ...over,
  }) as Item;

const threeQuestions = (): Item =>
  card({
    data: {
      title: "Setup",
      questions: [
        { question: "What should the crate be called?", type: "single", options: [{ label: "openleash" }, { label: "leash" }] },
        { question: "Which theme?", type: "confirm" },
        { question: "Anything else?", type: "text" },
      ],
    },
  } as Partial<Item>);

const answerNonBlocking = vi.spyOn(api, "answerNonBlocking").mockResolvedValue(undefined);
const draftSet = vi.spyOn(api, "draftSet").mockResolvedValue(undefined);
const draftGet = vi.spyOn(api, "draftGet").mockResolvedValue("");

describe("a question the agent isn't waiting on", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    answerNonBlocking.mockClear();
    draftSet.mockClear();
    draftGet.mockClear();
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("says it isn't waiting, in the open card and again once folded", () => {
    // The one thing that must never be ambiguous: nothing is parked on this.
    // Both states have to say it identically, or the state you read before
    // folding is not the one you get back.
    render(<AskNonBlocking it={card()} task={task} />);
    const said = document.querySelector(".al-noblock")!.textContent;
    expect(said).toMatch(/not waiting on you/i);
    expect(screen.queryByRole("button", { name: /^Submit/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Fold" }));
    expect(document.querySelector(".al-noblock")!.textContent).toBe(said);
  });

  it("arrives open, so a question is never hidden behind a click", () => {
    render(<AskNonBlocking it={card()} task={task} />);
    // The whole question is readable the moment it appears. This is the fix for
    // the collapsed-by-default card, which made an optional question something
    // you had to notice and click before you could even weigh it.
    expect(document.querySelector(".asklater")!.className).toContain("open");
    expect(screen.getByText("What should the crate be called?")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Open" }), "nothing left to expand").toBeNull();
  });

  it("folds and re-opens, which is the only reason the Fold button exists", () => {
    render(<AskNonBlocking it={card()} task={task} />);
    fireEvent.click(screen.getByRole("button", { name: "Fold" }));
    expect(document.querySelector(".asklater")!.className).not.toContain("open");
    // Folded, it is one line — and still says the agent isn't waiting.
    expect(screen.getByRole("button", { name: "Open" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(document.querySelector(".asklater")!.className).toContain("open");
  });

  it("shows one question at a time, not the whole form stacked", () => {
    render(<AskNonBlocking it={threeQuestions()} task={task} />);
    // A batch of ten used to render as ten stacked forms, which grew without
    // bound and pushed the composer off the screen. Only the first is here.
    expect(screen.getByText("What should the crate be called?")).toBeTruthy();
    expect(screen.queryByText("Which theme?")).toBeNull();
    expect(screen.queryByText("Anything else?")).toBeNull();
    // And it says where you are, so one-at-a-time doesn't read as one-only.
    expect(screen.getByText("1 of 3")).toBeTruthy();
  });

  it("pages through with Next, and sends once, on the last page", () => {
    render(<AskNonBlocking it={threeQuestions()} task={task} />);
    fireEvent.click(screen.getByText("leash"));
    fireEvent.click(screen.getByRole("button", { name: /Next/ }));
    expect(screen.getByText("Which theme?")).toBeTruthy();
    expect(screen.getByText("2 of 3")).toBeTruthy();
    fireEvent.click(screen.getByText("Yes"));
    fireEvent.click(screen.getByRole("button", { name: /Next/ }));
    expect(screen.getByText("Anything else?")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Send answer/ })).toBeTruthy();
    fireEvent.change(screen.getByPlaceholderText("Type your answer"), { target: { value: "no" } });
    fireEvent.click(screen.getByRole("button", { name: /Send answer/ }));
    expect(answerNonBlocking).toHaveBeenCalledTimes(1);
    const [id, itemId, response] = answerNonBlocking.mock.calls[0]!;
    expect(id).toBe("t1");
    expect(itemId).toBe("a1");
    // Every question is sent, not just the page you happened to be on.
    expect(response).toEqual({ answers: ["leash", true, "no"], notes: ["", "", ""], skipped: [] });
  });

  // Nothing here is a gate, and that is the point: the run carried on without the
  // answer, so the form must not then refuse to hand over what it does have. An
  // empty required question is reported as unanswered, which is truthful.
  it("sends even with a required question left empty, and says how many landed", () => {
    render(<AskNonBlocking it={threeQuestions()} task={task} />);
    fireEvent.click(screen.getByText("leash"));
    fireEvent.click(screen.getByRole("button", { name: /Next/ }));
    fireEvent.click(screen.getByText("Yes"));
    fireEvent.click(screen.getByRole("button", { name: /Next/ }));
    // The empty text page is the last one and the send is live: refusing here
    // would contradict the card's own hint and teach the user to ignore it.
    expect((screen.getByRole("button", { name: /Send answer/ }) as HTMLButtonElement).disabled).toBe(false);
    expect(document.querySelector(".al-hint")!.textContent).toMatch(/2 of 3/);
    fireEvent.click(screen.getByRole("button", { name: /Send answer/ }));
    expect(answerNonBlocking).toHaveBeenCalledTimes(1);
    const [, , response] = answerNonBlocking.mock.calls[0]!;
    expect(response, "the empty one is null, not invented").toEqual({ answers: ["leash", true, null], notes: ["", "", ""], skipped: [] });
  });

  // "Skip this" advances without answering, and records that the question was
  // deliberately left to the agent — a different statement from having no
  // opinion, and the agent is told which one happened.
  it("skips a page without answering it, and the agent is told it was skipped", () => {
    render(<AskNonBlocking it={threeQuestions()} task={task} />);
    fireEvent.click(screen.getByText("leash"));
    fireEvent.click(screen.getByRole("button", { name: /Next/ }));
    fireEvent.click(screen.getByRole("button", { name: /Skip this/ }));
    expect(screen.getByText("3 of 3")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Send answer/ }));
    expect(answerNonBlocking).toHaveBeenCalledTimes(1);
    const [, , response] = answerNonBlocking.mock.calls[0]!;
    // Only page 2 was skipped. Page 3 was sent empty, which reads as "no opinion"
    // rather than "skip this one" — the backend renders the two differently, so
    // the card has to report the distinction rather than blur it.
    expect(response).toEqual({ answers: ["leash", null, null], notes: ["", "", ""], skipped: [1] });
  });

  it("goes back a page without losing what was answered", () => {
    render(<AskNonBlocking it={threeQuestions()} task={task} />);
    fireEvent.click(screen.getByText("leash"));
    fireEvent.click(screen.getByRole("button", { name: /Next/ }));
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(screen.getByText("What should the crate be called?")).toBeTruthy();
    // The pick survived the round trip; paging must not silently reset a page.
    expect(document.querySelector(".opt.on")).toBeTruthy();
  });

  it("widens a single-choice question and sends every selected option", () => {
    render(<AskNonBlocking it={card()} task={task} />);
    fireEvent.click(screen.getByRole("button", { name: "Allow multiple" }));
    fireEvent.click(screen.getByText("openleash"));
    fireEvent.click(screen.getByText("leash"));
    fireEvent.click(screen.getByRole("button", { name: /Send answer/ }));
    const [, , response] = answerNonBlocking.mock.calls[0]!;
    expect(response).toEqual({ answers: [["openleash", "leash"]], notes: [""], skipped: [] });
  });

  it("sends the chosen option as the answer, with the item it belongs to", () => {
    render(<AskNonBlocking it={card()} task={task} />);
    fireEvent.click(screen.getByText("leash"));
    fireEvent.click(screen.getByRole("button", { name: /Send answer/ }));
    expect(answerNonBlocking).toHaveBeenCalledTimes(1);
    const [id, itemId, response] = answerNonBlocking.mock.calls[0]!;
    expect(id).toBe("t1");
    expect(itemId).toBe("a1");
    expect(response).toEqual({ answers: ["leash"], notes: [""], skipped: [] });
  });

  // "Send" and "Skip" stay different decisions: Send is the user weighing in
  // with what they have, Skip is handing the whole thing back so the agent
  // decides every part of it. They are not two names for one action — they
  // produce different responses — so Send is live even on an untouched form.
  it("distinguishes sending what you have from handing the whole thing back", () => {
    render(<AskNonBlocking it={card()} task={task} />);
    // Live from the start: an empty answer is a real answer here, and blocking
    // it would make "ignore the card" the only way past it.
    expect((screen.getByRole("button", { name: /Send answer/ }) as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: /Skip/ }));
    expect(answerNonBlocking).toHaveBeenCalledTimes(1);
    const [, , response] = answerNonBlocking.mock.calls[0]!;
    expect(response, "a skip is the agent deciding for itself").toEqual({ dismissed: true });
  });

  it("cannot send twice — a second answer is a second note to the agent", () => {
    render(<AskNonBlocking it={card()} task={task} />);
    fireEvent.click(screen.getByText("leash"));
    fireEvent.click(screen.getByRole("button", { name: /Send answer/ }));
    fireEvent.click(screen.getByRole("button", { name: /Send answer/ }));
    expect(answerNonBlocking).toHaveBeenCalledTimes(1);
  });

  it("keeps a half-typed answer across a re-mount, like the blocking form does", () => {
    const { unmount } = render(<AskNonBlocking it={card()} task={task} />);
    fireEvent.click(screen.getByText("leash"));
    act(() => { vi.advanceTimersByTime(300); });
    unmount();
    // Saved by option index, not label — the same way the blocking form saves,
    // so a label edited by the agent can't repoint somebody else's answer.
    const [, body] = draftSet.mock.calls.at(-1)!;
    expect(JSON.parse(body).ans[0].sel).toEqual([1]);
  });

  // A half-typed answer survives a fold, not just a chat switch: folding is the
  // normal way to get the card out of the way of a long run, so it cannot be the
  // thing that loses what you were typing.
  it("keeps the draft when the card is folded, and restores it when reopened", () => {
    const { unmount } = render(<AskNonBlocking it={card()} task={task} />);
    fireEvent.click(screen.getByText("leash"));
    act(() => { vi.advanceTimersByTime(300); });
    fireEvent.click(screen.getByRole("button", { name: "Fold" }));
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(document.querySelector(".opt.on"), "the pick is still there").toBeTruthy();
    unmount();
  });
});

describe("typing in the card is just typing", () => {
  // The first `describe` above leaves its render mounted, so without this the
  // `getAllByText("1 of 3")` below matches two cards and the assertions stop
  // meaning anything.
  afterEach(cleanup);
  // This is the bug: the body excluded only the *non*-text inputs
  // (`INPUT && type !== "text"`), so every plain text box fell through to the
  // card's shortcuts. Digits were taken as option picks, and any word with an
  // "s" in it fired skip — so the form jumped to the next question while you
  // were still writing the note. Silent, and it read as the card moving on by
  // itself.
  const note = () => document.querySelector<HTMLInputElement>('.al-body input[placeholder="Add a note (optional)"]')!;

  it("does not treat a typed digit in the note as an option pick", () => {
    render(<AskNonBlocking it={card()} task={task} />);
    expect(document.querySelector(".opt.on"), "nothing picked to begin with").toBeNull();
    fireEvent.change(note(), { target: { value: "512" } });
    // Every digit, not just the first: the keydown handler saw each one.
    for (const ch of "512") fireEvent.keyDown(note(), { key: ch });
    expect(document.querySelector(".opt.on"), "a digit in the note must not pick an option").toBeNull();
  });

  it("does not skip the question when the note contains an s", () => {
    render(<AskNonBlocking it={threeQuestions()} task={task} />);
    fireEvent.change(note(), { target: { value: "use sass" } });
    fireEvent.keyDown(note(), { key: "s" });
    // Still on page 1. This is what the user sees: mid-word, the card
    // advances to the next question.
    expect(screen.getAllByText("1 of 3").length).toBeGreaterThan(0);
    expect(screen.queryByText("Which theme?")).toBeNull();
  });

  it("leaves the page alone for an arrow key typed into the note too", () => {
    render(<AskNonBlocking it={threeQuestions()} task={task} />);
    fireEvent.keyDown(note(), { key: "ArrowRight" });
    expect(screen.getAllByText("1 of 3").length).toBeGreaterThan(0);
  });

  // ...while the shortcuts still work from the card itself, which is the whole
  // reason the body is focusable. A fix that only disabled the keys would make
  // the footer's "1-2 to choose · Enter to continue · S to skip" a lie.
  it("still answers the shortcuts from the card body", () => {
    render(<AskNonBlocking it={card()} task={task} />);
    const body = document.querySelector<HTMLElement>(".al-body")!;
    fireEvent.keyDown(body, { key: "2" });
    expect(document.querySelector(".opt.on")?.textContent, "option 2 picked").toMatch(/leash/);
    // Enter still pages, and S still skips: the guards are on the field, not
    // on the keys.
    render(<AskNonBlocking it={threeQuestions()} task={task} />);
    fireEvent.keyDown(document.querySelectorAll<HTMLElement>(".al-body")[1]!, { key: "Enter" });
    expect(screen.getAllByText("2 of 3").length).toBeGreaterThan(0);
  });
});

describe("which questions the card list shows", () => {
  it("shows only the ones still unanswered, in the order they were asked", () => {
    const items = [
      card({ id: "a1" }),
      card({ id: "a2", data: { questions: [{ question: "b" }], answers: ["x"] } } as any),
      card({ id: "a3", data: { questions: [{ question: "c" }], dismissed: true } } as any),
      { id: "a4", kind: "question", text: "", ts: "", data: { questions: [] } } as Item,
    ] as Item[];
    expect(pendingOf(items).map((i) => i.id)).toEqual(["a1"]);
    expect(pendingOf(undefined)).toEqual([]);
  });

  it("shows answered and dismissed questions as transcript receipts", () => {
    const session = readFileSync("src/ui/Session.tsx", "utf8");
    expect(session).toContain("it.kind === \"asklater\" && (it.data?.answers || it.data?.dismissed)");
    expect(session).toContain("it.kind === \"asklater\" && (it.data?.answers || it.data?.dismissed) && <Question it={it} task={task} />");
  });

  it("is kept out of the transcript while unanswered, so it cannot show in two places at once", () => {
    const session = readFileSync("src/ui/Session.tsx", "utf8");
    const filter = session.split("\n").find((l) => l.includes("return !(item.kind === \"user\""));
    expect(filter, "FeedRows filters unresolved asklater forms out").toBeTruthy();
    expect(filter).toContain("data?.queued");
    expect(filter).toContain("data?.answers");
    expect(filter).toContain("data?.dismissed");
    expect(session).toContain("it.kind === \"asklater\" && (it.data?.answers || it.data?.dismissed)");
  });
});

describe("the card is announced, because it sits where nothing is scrolling", () => {
  // This is the bug that made the card easy to miss entirely: `ask_nonblocking`
  // emitted no attention event at all, so no toast ever fired and the in-app
  // notice never appeared. The card sits above the composer, not where the
  // agent's output arrives, so on a busy run it is genuinely easy to never see.
  it("emits its own attention kind from the backend", () => {
    const runner = readFileSync("src-tauri/src/agent/runner.rs", "utf8");
    const body = runner.slice(runner.indexOf("async fn ask_nonblocking"));
    expect(body).toContain('"nonblocking"');
    expect(body, "on the attention channel").toContain("ol://attention");
  });

  it("words it as an offer, not a demand, in the toast copy", () => {
    // The copy has to say the agent kept going, or an optional question reads as
    // an emergency and the notification itself becomes the interruption the
    // tool exists to avoid.
    const n = { task_id: "t", kind: "nonblocking" };
    expect(notifyText(n, "Fix fluids", "")?.body).toMatch(/kept going|leave it/i);
    expect(notifyText(n, "Fix fluids", "")?.needs).toBe("nonblocking");
    expect(notifyText(n, "Fix fluids", "")?.title).toBe("Fix fluids asked a question");
  });

  it("still reaches a user already looking at the chat, where a blocking question would not", () => {
    // You are staring at this very run. A `question` event would be suppressed
    // here as redundant with the card in the transcript — but this card is above
    // the composer, not in the feed, so suppressing it means the question is
    // only ever announced to users who are looking elsewhere.
    expect(surfaces({ task_id: "t", kind: "nonblocking" }, "X", "", true, true)).toEqual(["inapp"]);
    expect(surfaces({ task_id: "t", kind: "question" }, "X", "", true, true)).toEqual([]);
  });

  // The run did not stop, so there is no card in the transcript to scroll back
  // to. That is the whole reason this question exists in this shape, and it is
  // the case a toast is for — unlike a blocking question, which stops the run
  // and therefore leaves a visible card behind.
  it("toasts when the window is in the background, because nothing stopped to wait", () => {
    expect(surfaces({ task_id: "t", kind: "nonblocking" }, "X", "", false, true)).toContain("desktop");
    expect(surfaces({ task_id: "t", kind: "nonblocking" }, "X", "", false, false)).toContain("desktop");
  });

  it("times the in-app notice out, because the run carried on", () => {
    // A blocking question never times out: the agent is frozen until answered.
    // This one must, or the notice outlives the moment it was worth reporting.
    expect(noticeMeta("nonblocking").linger).toBeGreaterThan(0);
    expect(noticeMeta("question").linger).toBe(0);
  });
});

describe("question text is not clipped", () => {
  // `.qtext` is the queued-message row's one-line ellipsis rule, and the
  // question card reuses the class. It resets word-break but not white-space,
  // so every long question, description and form title used to be cut off
  // mid-sentence. A question is read, not scanned.
  it("the question card undoes the single-line clip it inherits from .qtext", () => {
    const css = readFileSync("src/App.css", "utf8");
    const rule = css.split("\n").find((l) => l.startsWith(".card-q .qtext, .card-q .qtitle, .card-q .qintro"));
    expect(rule, "the .card-q text rule exists").toBeTruthy();
    expect(rule).toMatch(/white-space:\s*normal/);
    expect(rule).toMatch(/overflow:\s*visible/);
  });

  it("while the queued-message row, which is scanned and not read, stays one line", () => {
    const css = readFileSync("src/App.css", "utf8");
    const rule = css.split("\n").find((l) => l.startsWith(".qtext {"));
    expect(rule).toMatch(/white-space:\s*nowrap/);
  });

  // The list, not each card, owns the height cap so concurrent questions stay
  // in order and scroll together above the composer.
  it("bounds the open question stack rather than overlapping individual cards", () => {
    const css = readFileSync("src/App.css", "utf8");
    const stack = css.split("\n").find((l) => l.startsWith(".asklater-stack {"));
    expect(stack, "the complete stack is bounded").toBeTruthy();
    expect(stack).toMatch(/max-height/);
    expect(stack, "all questions are reachable in the stack").toMatch(/overflow-y:\s*auto/);
    expect(css).not.toMatch(/\.asklater\.open \{[^}]*max-height/);
    expect(css).not.toMatch(/\.asklater\.open > \.al-head\s*\{[^}]*position:\s*sticky/);
    expect(css).not.toMatch(/\.asklater\.open > \.al-foot\s*\{[^}]*position:\s*sticky/);
  });

  // And the boundary itself: a cap on the body cut an option description off
  // mid-sentence. The cap is on the card, never on the body, so the whole of a
  // described option is still in the DOM to be scrolled to.
  it("still never caps the body, so a described option is not cut mid-sentence", () => {
    const css = readFileSync("src/App.css", "utf8");
    expect(css).not.toMatch(/\.al-body\s*\{[^}]*max-height/);
  });

  // `.al-q` was declared twice in the asklater block: once as the clipped
  // one-line title and once as the flex-column question wrapper. The second
  // declaration silently won, so the folded title's ellipsis never applied and
  // the two rules fought over the same class.
  it("does not declare one class twice in the asklater block", () => {
    const css = readFileSync("src/App.css", "utf8");
    const block = css.slice(css.indexOf(".asklater {"), css.indexOf(".todo-panel {"));
    const seen = new Map<string, number>();
    for (const m of block.matchAll(/^\.([a-z0-9-]+)\s*[,{]/gm)) seen.set(m[1]!, (seen.get(m[1]!) ?? 0) + 1);
    const dupes = [...seen].filter(([, n]) => n > 1).map(([c]) => c);
    expect(dupes, "a duplicated selector makes the later rule win silently").toEqual([]);
  });
});
