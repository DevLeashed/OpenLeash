/// <reference types="node" />
import { beforeEach, describe, expect, it, vi } from "vitest";

// Navigation is pure store bookkeeping — the only reason it needs a mock is
// that `go("session")` opens the chat it lands on, which would talk to the
// backend. `task_get` has to answer with a shape `openTask` can spread, or the
// async open rejects after the assertions have already run.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "task_get") return { summary: { id: "a", title: "A" }, items: [], bg: [], subs: [] };
    return null;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { back, forward, get, go, set } from "./store";

// Back/Forward as a pair. The Forward button shipped as a dead <span>, so the
// only thing that had ever been exercised was `hist` growing and shrinking;
// these are the invariants that make the arrow worth having at all.
describe("back and forward", () => {
  beforeEach(() => {
    set({ hist: [], fwd: [], view: "home", task: null });
  });

  it("arms Forward once Back has been pressed", () => {
    go("settings");
    expect(get().hist).toHaveLength(1);
    expect(get().fwd, "nothing to forward to yet").toHaveLength(0);

    back();
    // This is the bug: the button read a stack that was never written, so it
    // stayed disabled forever.
    expect(get().fwd).toEqual([{ view: "settings", task: null }]);
    expect(get().view).toBe("home");
  });

  it("returns to the screen Back came from", () => {
    go("settings");
    back();
    forward();
    expect(get().view).toBe("settings");
    // Forward is a replay, not a fresh navigation: it must not re-push the
    // screen it left, or the two stacks grow on every round trip.
    expect(get().fwd).toHaveLength(0);
    expect(get().hist).toHaveLength(1);
  });

  it("walks several steps in both directions", () => {
    go("session", { task: "a" });
    go("session", { task: "b" });
    go("settings");
    expect(get().view).toBe("settings");

    back();
    expect(get().view).toBe("session");
    expect(get().task).toBe("b");
    back();
    expect(get().task).toBe("a");

    forward();
    expect(get().task).toBe("b");
    forward();
    expect(get().view).toBe("settings");
    expect(get().fwd, "nothing left ahead").toHaveLength(0);
    // And Back is still available afterwards: Forward handed its entries back.
    expect(get().hist.length).toBeGreaterThan(0);
  });

  it("drops Forward when a new navigation happens, like a browser", () => {
    go("settings");
    back();
    expect(get().fwd).toHaveLength(1);

    go("saved");
    // Otherwise Forward would jump back into the branch the user just left.
    expect(get().fwd).toHaveLength(0);
  });

  it("does nothing at either end of the stacks", () => {
    forward();
    expect(get().view).toBe("home");
    back();
    expect(get().view).toBe("home");
    expect(get().hist).toHaveLength(0);
  });

  it("keeps re-navigating to the screen already open out of the history", () => {
    // `go` deliberately doesn't record a no-op, so this must not arm Forward
    // either — pressing Back should not walk you to where you already were.
    go("settings");
    go("settings");
    expect(get().hist).toHaveLength(1);
  });
});