import { describe, expect, it, vi } from "vitest";

import { flash, get, set } from "./store";

describe("an actionable toast", () => {
  it("carries the action alongside the message", () => {
    const run = vi.fn();
    flash("Chat archived", { label: "Undo", run });
    expect(get().toast).toBe("Chat archived");
    expect(get().toastAction?.label).toBe("Undo");
    expect(get().toastAction?.run).toBe(run);
  });

  // A plain message must clear a previous action, or the button from the last
  // toast would sit under a message it does not belong to — and its closure
  // would still be pointing at an id that may since have been deleted.
  it("has no action when the message has none, even after one that did", () => {
    flash("Chat archived", { label: "Undo", run: () => {} });
    flash("Something else");
    expect(get().toast).toBe("Something else");
    expect(get().toastAction).toBeNull();
  });

  it("is cleared with the message", () => {
    set({ toast: null, toastAction: null });
    expect(get().toast).toBeNull();
    expect(get().toastAction).toBeNull();
  });
});
