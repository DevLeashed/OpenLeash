// @vitest-environment jsdom
/// `useNow` drives the "3m ago" timestamps. It used to create one `setInterval`
/// per caller, so an 80-row sidebar meant 80 live timers. Now one interval is
/// shared and refcounted. The failure this guards is invisible in the UI — a
/// clock that stops ticking leaves stale timestamps that still *look* fine, and
/// one that leaks keeps the whole app awake — so both are pinned here.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { useNow } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

let ticks = 0;

/** A row that asks for a clock and counts its own renders, like `Chrome.Row`. */
function Probe({ ms }: { ms?: number }) {
  useNow(ms);
  return <span data-testid="ticks">{++ticks}</span>;
}

beforeEach(() => {
  ticks = 0;
  vi.useFakeTimers();
});
afterEach(() => {
  // Unmount before restoring timers: a caller left mounted keeps a subscriber
  // in the shared set, and the next test would inherit its clock.
  cleanup();
  vi.useRealTimers();
});

describe("useNow", () => {
  it("gives every caller one shared interval, not one each", () => {
    const set = vi.spyOn(globalThis, "setInterval");
    const { unmount } = render(
      <>
        <Probe />
        <Probe />
        <Probe />
      </>,
    );
    // The whole point of the refcount: mounting N clocks costs one timer.
    expect(set).toHaveBeenCalledTimes(1);
    unmount();
    set.mockRestore();
  });

  it("still ticks every caller on the shared interval", () => {
    const { getAllByTestId } = render(
      <>
        <Probe />
        <Probe />
      </>,
    );
    const before = getAllByTestId("ticks").map((n) => Number(n.textContent));
    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    const after = getAllByTestId("ticks").map((n) => Number(n.textContent));
    expect(after.every((n, i) => n > (before[i] ?? 0))).toBe(true);
  });

  it("keeps ticking past the first tick", () => {
    // The state bump has to be an increment. Setting a constant makes the
    // second update identical, React bails out, and the clock stops dead.
    const { getByTestId } = render(<Probe />);
    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    const first = Number(getByTestId("ticks").textContent);
    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    const second = Number(getByTestId("ticks").textContent);
    expect(second).toBeGreaterThan(first);
    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    expect(Number(getByTestId("ticks").textContent)).toBeGreaterThan(second);
  });

  it("stops the interval once the last caller unmounts", () => {
    const clear = vi.spyOn(globalThis, "clearInterval");
    const first = render(<Probe />);
    const second = render(<Probe />);
    // One goes away while the other is still mounted: the shared timer stays.
    first.unmount();
    expect(clear).not.toHaveBeenCalled();
    second.unmount();
    expect(clear).toHaveBeenCalledTimes(1);
    clear.mockRestore();
  });

  it("does not tick a caller that has unmounted", () => {
    const { unmount } = render(<Probe />);
    unmount();
    expect(() =>
      act(() => {
        vi.advanceTimersByTime(30_000 * 4);
      }),
    ).not.toThrow();
  });
});
