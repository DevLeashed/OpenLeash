// @vitest-environment jsdom
// Typing must not move the caret, whatever else the app is doing.
//
// The composer draft lives in the store, and the store used to hand its
// subscribers a microtask to re-render in. That broke the caret: React restores
// a controlled field's value at the end of the event batch, from the props of
// the render it just committed, so a store write that had not re-rendered yet
// made React write the *previous* draft back over the character just typed. The
// character vanished and the caret landed at the end of the line — which is
// exactly "the cursor jumps to the end of the text while typing".
//
// It only happened *sometimes* because it was a race: the microtask had to land
// before React's restore. An agent streaming into the same window is what made
// it show up in practice, since a `task` event arrives mid-sentence.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { get, set } from "../store";
import { Composer } from "./Composer";
import type { TaskSummary } from "../api";

const box = () => document.querySelector(".composer textarea") as HTMLTextAreaElement;
/** Type the way the browser does: the value changes, the caret is left alone. */
const type = (el: HTMLTextAreaElement, value: string, caret: number) => {
  fireEvent.change(el, { target: { value } });
  act(() => { el.setSelectionRange(caret, caret); });
};
/** An agent step landing: the burst of store writes that used to race the caret. */
const taskEvent = () => act(() => { set((s) => ({ tasks: { ...s.tasks, t1: { ...s.tasks.t1, step: "Working" } as TaskSummary } })); });

describe("the composer caret while typing", () => {
  beforeEach(() => { set({ draft: "" }); });
  afterEach(() => cleanup());

  it("keeps the caret where it was when something unrelated re-renders", () => {
    render(<Composer mode="home" />);
    const el = box();
    el.focus();
    type(el, "hello world", 5);
    expect(el.selectionStart).toBe(5);

    act(() => { set({ toast: "a task event arrived" }); });

    expect(el.value, "the character typed must not be undone").toBe("hello world");
    expect(el.selectionStart, "the caret must not jump to the end").toBe(5);
  });

  it("keeps editing in the middle of a line while events keep arriving", () => {
    render(<Composer mode="home" />);
    const el = box();
    el.focus();
    type(el, "one two three", 4);
    // Fix the middle of the line, with events landing between the keystrokes.
    act(() => { el.setSelectionRange(4, 7); });
    fireEvent.change(el, { target: { value: "one TWO three" } });
    act(() => { el.setSelectionRange(7, 7); });
    taskEvent();
    fireEvent.change(el, { target: { value: "one TWOX three" } });
    act(() => { el.setSelectionRange(8, 8); });
    taskEvent();

    expect(el.value).toBe("one TWOX three");
    expect(get().draft, "the store must match the box").toBe("one TWOX three");
    expect(el.selectionStart, "the caret must stay put mid-line").toBe(8);
  });

  it("keeps a selection spanning several words", () => {
    render(<Composer mode="home" />);
    const el = box();
    el.focus();
    type(el, "ship it now", 0);
    act(() => { el.setSelectionRange(0, 7); });
    taskEvent();

    expect(el.value).toBe("ship it now");
    expect(el.selectionStart).toBe(0);
    expect(el.selectionEnd, "the selection must not collapse to a caret").toBe(7);
  });

  it("does not move the caret when the attach rows re-render around it", () => {
    render(<Composer mode="home" />);
    const el = box();
    el.focus();
    type(el, "ship it", 4);

    // An attachment appearing adds rows above the box and rebuilds the bar
    // under it.
    act(() => { set((s) => ({ attach: { ...s.attach, "new-chat": ["data:image/png;base64,AAA"] } })); });

    expect(el.value).toBe("ship it");
    expect(el.selectionStart).toBe(4);
  });
});
