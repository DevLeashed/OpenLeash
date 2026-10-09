// @vitest-environment jsdom
// Picking a model colour out of the model picker felt broken, and every part of
// it was wrong in the same place: what happens *while you are choosing*, rather
// than what ends up saved.
//
//  1. The custom-colour control was a permanent conic gradient. The one widget
//     whose job is to show you what colour you just picked showed a colour wheel
//     instead, so a hue chosen from the OS dialog had nothing on screen to
//     confirm it. It also only read the input on `change`, so nothing moved at
//     all until the OS picker closed.
//
//  2. `ColorDot` (Overlays.tsx) writes that value straight through `saveSettings`,
//     and Chromium fires `change` continuously through a native colour drag — so
//     a drag along the hue strip was a settings write, and a full settings
//     round-trip, per pixel. The dot lagged a step behind the cursor the whole
//     way, which is what "doesn't react well on input" was.
//
//  3. Selection was `outline` + `transform: scale()`, and `.swatch:hover`
//     re-declared `transform` on the same element. Pointing at the colour you
//     had already chosen removed the ring, so the grid showed nothing selected
//     at exactly the moment you were checking it. A translucent white outline
//     was also invisible on the light end of the palette, which is most of it.
//
// The paint is now driven off `input` (live, no write), the commit is debounced
// and flushed on unmount, and the ring is a box-shadow no transform can cancel.
// `ColorPicker` is the only thing that writes, so these are pinned here at that
// boundary; the CSS half is pinned against the stylesheet, the way
// ask-nonblocking.test.tsx does.
import { readFileSync } from "node:fs";
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ColorPicker } from "./primitives";

const css = readFileSync("src/App.css", "utf8");

/**
 * What the picker is being told, in order. Wrapped so `value` follows what the
 * picker itself emits — callers in the app hold the value in a store, and a test
 * that froze it would be testing a picker no user ever sees.
 */
const picker = (initial: string) => {
  const onChange = vi.fn();
  const Host = () => {
    const [v, setV] = useState(initial);
    return <ColorPicker value={v} onChange={(c) => { onChange(c); setV(c); }} />;
  };
  const view = render(<Host />);
  return { ...view, onChange, unmount: view.unmount };
};

/** One step of a native colour drag, which is what Chromium actually reports. */
const dragTo = (v: string) =>
  fireEvent.input(document.querySelector<HTMLInputElement>('.cpicker input[type="color"]')!, { target: { value: v } });

const selected = () => [...document.querySelectorAll(".cpicker .swatch")].filter((s) => s.classList.contains("on"));
/** The colour painted inside the custom control, as the DOM reports it. */
const customFill = () => document.querySelector<HTMLElement>(".cpicker .cswatch-fill")?.style.background ?? "";
/** jsdom rewrites a hex in `style.background` to `rgb()`. */
const asRgb = (hex: string) =>
  `rgb(${parseInt(hex.slice(1, 3), 16)}, ${parseInt(hex.slice(3, 5), 16)}, ${parseInt(hex.slice(5, 7), 16)})`;

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });

describe("the custom colour control shows what you are picking", () => {
  it("paints the live colour as the drag moves, before anything is committed", () => {
    picker("#e8967a");
    expect(customFill(), "it starts on the colour in effect, not a fixed wheel").toBe(asRgb("#e8967a"));
    // No timer advanced and nothing awaited: the paint has to be a direct
    // consequence of the event, or the control is still showing nothing.
    dragTo("#fb7185");
    expect(customFill(), "the swatch follows the drag").toBe(asRgb("#fb7185"));
  });

  it("commits nothing on every step of the drag", () => {
    const { onChange } = picker("#e8967a");
    for (const v of ["#fb7185", "#f87171", "#fb923c", "#fbbf24"]) dragTo(v);
    // Each of these used to become a settings write. That is the lag.
    expect(onChange, "a write per pixel of hue is what made this feel broken").not.toHaveBeenCalled();
  });

  it("commits once when the drag settles, on the colour it ended on", () => {
    const { onChange } = picker("#e8967a");
    for (const v of ["#fb7185", "#f87171", "#fb923c"]) dragTo(v);
    act(() => { vi.advanceTimersByTime(200); });
    expect(onChange, "one write for the whole drag").toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith("#fb923c");
  });

  it("commits on `change` too, which is where the OS picker closes", () => {
    const { onChange } = picker("#e8967a");
    // A platform that only reports when the OS dialog closes, rather than
    // streaming it: the commit must still happen, not wait for a timer that was
    // never started by an `input`.
    fireEvent.change(document.querySelector<HTMLInputElement>('.cpicker input[type="color"]')!, { target: { value: "#fb923c" } });
    act(() => { vi.advanceTimersByTime(200); });
    expect(onChange, "the picker closing is an unambiguously final commit").toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith("#fb923c");
  });

  it("still commits if the picker goes away mid-drag", () => {
    // The debounce must not eat the last colour: the panel closing, the dialog
    // being cancelled and the app quitting all unmount this with the timer
    // still pending.
    const { onChange, unmount } = picker("#e8967a");
    dragTo("#fb923c");
    unmount();
    expect(onChange, "the colour the user actually chose still lands").toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith("#fb923c");
  });

  it("does not re-commit a colour that was already committed", () => {
    const { onChange } = picker("#e8967a");
    dragTo("#fb923c");
    act(() => { vi.advanceTimersByTime(200); });
    // Chromium ends a drag with a native `change` landing just after the stream
    // stopped, so the timer has usually already fired. Writing again here is how
    // one colour became two round-trips.
    fireEvent.change(document.querySelector<HTMLInputElement>('.cpicker input[type="color"]')!, { target: { value: "#fb923c" } });
    act(() => { vi.advanceTimersByTime(200); });
    expect(onChange).toHaveBeenCalledTimes(1);
  });
});

describe("what the grid says is selected", () => {
  it("shows a colour the grid does not carry as the current one", () => {
    // A hue from the OS picker has to read as *the* colour, or picking one
    // looks like it did nothing.
    picker("#1a2b3c");
    expect(selected(), "exactly one swatch reads as selected").toHaveLength(1);
    expect(selected()[0]!.getAttribute("aria-label")).toBe("#1a2b3c");
  });

  it("marks a grid swatch the user clicks, and commits it immediately", () => {
    const { onChange } = picker("#e8967a");
    const target = [...document.querySelectorAll(".cpicker .swatch")].find((s) => s.getAttribute("aria-label") === "#4ade80")!;
    fireEvent.click(target);
    expect(selected()[0]!.getAttribute("aria-label")).toBe("#4ade80");
    // A grid click is a decision, not a drag: no debounce, no second step.
    expect(onChange).toHaveBeenCalledTimes(1);
  });
});

describe("the ring around the selected swatch", () => {
  it("is not carried by the property the hover rule animates", () => {
    // The bug was structural: `.swatch.on` and `.swatch:hover` both declared
    // `transform`, and `:hover` won, so hovering the colour you had already
    // chosen removed the selection.
    expect(css, "selection paints through box-shadow").toMatch(/\.swatch\.on\s*\{[^}]*box-shadow/);
    expect(css, "and hover only scales, so it cannot cancel the ring").toMatch(/\.swatch:hover\s*\{[^}]*transform: scale\([^)]*\);?\s*\}/);
  });

  it("is visible against the surface the picker actually renders on", () => {
    // The ring's inner gap used to be translucent white, which vanished on the
    // light half of the palette; and when that became a token it has to be one
    // that does not flip with the theme, because both surfaces the picker opens
    // on — `.modal` and `.statuspop` — are hardcoded dark in light mode too.
    expect(css).toMatch(/\.swatch\.on\s*\{[^}]*var\(--ring-gap\)/);
    expect(css).toMatch(/--ring-gap:\s*#1e1e21/);
    // The declaration, not the usages: `:root[data-mode="light"]` must not
    // re-point it, because the surfaces it is cut against stay dark there.
    expect(css.match(/^\s*--ring-gap:/gm), "declared once, not re-pointed per mode").toHaveLength(1);
  });
});

describe("the grid's shape", () => {
  it("is a fixed pitch rather than a wrap", () => {
    // 16 swatches at 18px + 5px was 350px: it broke after 13 in the 252px
    // panel and left three swatches and Reset stranded on a second line, and in
    // the model dialog it broke wherever the label above happened to end.
    expect(css, "two even rows of 8, the same 16 swatches").toMatch(/\.cpicker\s*\{[^}]*grid-template-columns:\s*repeat\(8,\s*18px\)/);
  });
});
