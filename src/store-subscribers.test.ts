// Unchanged state is skipped before the subscribers run, so `set` only reaches
// them when something actually changed. A subscriber that throws used to stop
// every subscriber after it — and since `set` is called from Tauri event
// callbacks, the throw landed in Rust's dispatcher instead of on screen.
import { afterEach, describe, expect, it, vi } from "vitest";
import { set, subscribe, type State } from "./store";

const spies: Array<() => void> = [];
afterEach(() => { for (const off of spies.splice(0)) off(); vi.restoreAllMocks(); });

describe("a subscriber that throws", () => {
  it("does not stop the others from hearing about the change", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const calls: string[] = [];
    spies.push(subscribe(() => { calls.push("first"); throw new Error("broken row"); }));
    spies.push(subscribe(() => { calls.push("second"); }));

    set((s) => ({ ready: !(s as State).ready }));

    expect(calls).toEqual(["first", "second"]);
  });

  it("keeps notifying after it throws, run after run", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    let thrown = 0;
    spies.push(subscribe(() => { thrown++; throw new Error("broken row"); }));
    const heard = vi.fn();
    spies.push(subscribe(heard));

    set((s) => ({ ready: !(s as State).ready }));
    set((s) => ({ ready: !(s as State).ready }));

    expect(thrown).toBe(2);
    expect(heard).toHaveBeenCalledTimes(2);
  });
});
