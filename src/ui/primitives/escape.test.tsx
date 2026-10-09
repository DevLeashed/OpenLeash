// @vitest-environment jsdom
import { describe, expect, it, vi, afterEach } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { useEscapeClose } from "./escape";
import { Dropdown } from "./Dropdown";

// No auto-cleanup here (this suite does not opt into vitest globals), and a
// listener left attached by one test would keep swallowing the next test's key —
// which looks exactly like the hook ignoring `open`.
afterEach(cleanup);

/**
 * Escape is the one key with two meanings here. The popups close on it; the
 * global handler in `src/App.tsx` reads it as "pause this chat", then "stop it"
 * if already paused. So the hook has to *stop* the event as well as consume it.
 *
 * Registration order can't be used to prove that here: the hook's listener is
 * attached by an effect, and a listener added after it in the same phase is
 * still called (that is what `stopImmediatePropagation` alone would do — and the
 * hook must not use that, or a second popup bound later could not close). So the
 * tests below assert the two observable things separately: the key was consumed
 * (`defaultPrevented`), and the event was stopped (`cancelBubble`).
 */
describe("useEscapeClose", () => {
  it("consumes and stops Escape when open", () => {
    const close = vi.fn();
    const Comp = () => { useEscapeClose(true, close); return null; };
    render(<Comp />);

    // A listener on an element *below* the popup's target stands in for the app's
    // own handler: `stopPropagation` is exactly what keeps the key from reaching
    // it. `cancelBubble` is not readable off the event in jsdom once dispatch
    // has finished, so the assertion has to be made from the far side — and the
    // key is dispatched on that element, so `window` sees it during the capture
    // phase on the way down.
    const reached = vi.fn();
    const inner = document.createElement("div");
    inner.addEventListener("keydown", reached);
    document.body.appendChild(inner);

    const e = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => { inner.dispatchEvent(e); });
    inner.remove();

    expect(close).toHaveBeenCalledTimes(1);
    expect(e.defaultPrevented, "Escape must be consumed").toBe(true);
    expect(reached, "and must not reach anything further down the tree").not.toHaveBeenCalled();
  });

  it("closes only while open", () => {
    const close = vi.fn();
    const Comp = () => { useEscapeClose(false, close); return null; };
    render(<Comp />);

    const e = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    window.dispatchEvent(e);

    expect(close).not.toHaveBeenCalled();
    expect(e.defaultPrevented, "a closed popup must leave the key alone").toBe(false);
  });

  it("leaves every other key alone", () => {
    const close = vi.fn();
    const Comp = () => { useEscapeClose(true, close); return null; };
    render(<Comp />);

    const e = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
    window.dispatchEvent(e);

    expect(close).not.toHaveBeenCalled();
    expect(e.defaultPrevented).toBe(false);
  });

  it("binds on the capture phase, so it beats a global listener bound at mount", () => {
    // The app's own handler is attached to `window` before any popup opens, so a
    // bubble-phase listener would already have seen the key by the time the
    // popup ran. Ordering is observable here in one direction only: a capture
    // listener always precedes a bubble one.
    const order: string[] = [];
    const bubble = () => order.push("bubble");
    const Comp = () => { useEscapeClose(true, () => order.push("escape")); return null; };
    render(<Comp />);
    window.addEventListener("keydown", bubble);
    try {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    } finally {
      window.removeEventListener("keydown", bubble);
    }
    expect(order[0], "capture runs before bubble").toBe("escape");
  });

  it("the dropdown uses it: Escape closes the picker, not the chat behind it", () => {
    const { getByRole } = render(<Dropdown value="" options={[{ value: "a", label: "A" }, { value: "b", label: "B" }]} onChange={() => {}} />);
    act(() => { getByRole("combobox").click(); });
    expect(document.querySelector(".ddpop"), "the picker is open").toBeTruthy();

    let reached = false;
    window.addEventListener("keydown", (e) => { if (e.key === "Escape") reached = true; });
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });

    expect(document.querySelector(".ddpop"), "Escape should close the picker").toBeNull();
    expect(reached, "and must not be read by anything else on the way up").toBe(false);
  });
});