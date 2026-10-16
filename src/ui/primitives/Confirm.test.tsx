// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { createElement } from "react";

import { ConfirmDialog } from "./index";

afterEach(() => cleanup());

describe("the app's own confirm dialog", () => {
  it("shows the question, both answers, and the caller's labels", () => {
    const { getByText } = render(createElement(ConfirmDialog, {
      title: "Replace your commit message?",
      body: "Generating a message will overwrite what you typed.",
      confirm: "Replace",
      onConfirm: () => {},
      onClose: () => {},
    }));
    expect(getByText("Replace your commit message?")).toBeTruthy();
    expect(getByText("Generating a message will overwrite what you typed.")).toBeTruthy();
    expect(getByText("Replace")).toBeTruthy();
    expect(getByText("Cancel")).toBeTruthy();
  });

  // The reason this exists rather than `window.confirm`: a keyboard user must be
  // able to answer it, and landing on the destructive button by default is how a
  // dialog trains people to press Enter without reading.
  //
  // Asserted as DOM order, not as `document.activeElement`: `Modal` focuses the
  // first *laid out* focusable stop, and jsdom lays nothing out — `getClientRects`
  // is empty for every element, so the modal falls back to focusing its own panel
  // and any focus assertion here would be testing the test environment.
  it("puts Cancel before the destructive answer, so a keyboard reaches it first", () => {
    const { getByRole, getByText } = render(createElement(ConfirmDialog, {
      title: "Delete everything?",
      confirm: "Delete",
      danger: true,
      onConfirm: () => {},
      onClose: () => {},
    }));
    const buttons = [...getByRole("dialog").querySelectorAll("button")];
    expect(buttons.map((b) => b.textContent)).toEqual(["Cancel", "Delete"]);
    // Destructive actions are marked as such rather than looking like the safe one.
    expect((getByText("Delete") as HTMLElement).style.background).toBe("rgb(255, 138, 138)");
  });

  it("is a modal dialog, so the rest of the window is inert to a screen reader", () => {
    const { getByRole } = render(createElement(ConfirmDialog, {
      title: "Proceed?",
      confirm: "Yes",
      onConfirm: () => {},
      onClose: () => {},
    }));
    const dialog = getByRole("dialog");
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-label")).toBe("Proceed?");
  });

  it("closes before confirming, so a slow action cannot be fired twice", () => {
    const calls: string[] = [];
    const { getByText } = render(createElement(ConfirmDialog, {
      title: "Proceed?",
      confirm: "Yes",
      onConfirm: () => calls.push("confirm"),
      onClose: () => calls.push("close"),
    }));
    getByText("Yes").click();
    expect(calls).toEqual(["close", "confirm"]);
  });

  it("cancelling runs the close path only", () => {
    const onConfirm = vi.fn();
    const onClose = vi.fn();
    const { getByText } = render(createElement(ConfirmDialog, {
      title: "Proceed?",
      confirm: "Yes",
      onConfirm,
      onClose,
    }));
    getByText("Cancel").click();
    expect(onClose).toHaveBeenCalled();
    expect(onConfirm).not.toHaveBeenCalled();
  });
});
