import { describe, expect, it } from "vitest";

import { applyMode, resolveMode } from "./theme";

/** A `matchMedia` stand-in that can be flipped, so "system" can be tested both
 *  as an initial read and as a later OS change. */
function media(light: boolean) {
  const listeners = new Set<() => void>();
  return {
    matches: light,
    addEventListener: (_: string, fn: () => void) => void listeners.add(fn),
    removeEventListener: (_: string, fn: () => void) => void listeners.delete(fn),
    change(to: boolean) {
      this.matches = to;
      for (const fn of listeners) fn();
    },
    get listeners() {
      return listeners.size;
    },
  };
}

function harness(light = false) {
  const mq = media(light);
  const doc = { documentElement: { dataset: {} as DOMStringMap } } as unknown as Document;
  const win = { matchMedia: () => mq } as unknown as Window;
  return { mq, doc, win, mode: () => doc.documentElement.dataset.mode };
}

describe("resolveMode", () => {
  it("honours an explicit choice", () => {
    expect(resolveMode("light", false)).toBe("light");
    expect(resolveMode("dark", true)).toBe("dark");
  });

  it("follows the OS only when asked to", () => {
    expect(resolveMode("system", true)).toBe("light");
    expect(resolveMode("system", false)).toBe("dark");
  });

  it("falls back to dark for an absent or unknown value", () => {
    // Absent is the real case: settings saved before light mode existed have no
    // `mode` key, and the backend defaults those to dark.
    expect(resolveMode(undefined, true)).toBe("dark");
    expect(resolveMode("", true)).toBe("dark");
    expect(resolveMode("sepia", true)).toBe("dark");
  });
});

describe("applyMode", () => {
  it("paints the resolved palette onto the document root", () => {
    const a = harness(false);
    applyMode("light", a.doc, a.win);
    expect(a.mode()).toBe("light");
    applyMode("dark", a.doc, a.win);
    expect(a.mode()).toBe("dark");
  });

  it("keeps following the OS when the setting says system", () => {
    const a = harness(true);
    const teardown = applyMode("system", a.doc, a.win);
    expect(a.mode()).toBe("light");
    // The desktop switches to dark at sunset: the app goes with it.
    a.mq.change(false);
    expect(a.mode()).toBe("dark");
    teardown();
    expect(a.mq.listeners).toBe(0);
  });

  it("does not subscribe when the choice is explicit", () => {
    const a = harness(true);
    applyMode("dark", a.doc, a.win);
    expect(a.mode()).toBe("dark");
    expect(a.mq.listeners).toBe(0);
  });

  it("falls back to dark rather than throwing where matchMedia is missing", () => {
    const doc = { documentElement: { dataset: {} as DOMStringMap } } as unknown as Document;
    const win = {} as unknown as Window;
    expect(() => applyMode("system", doc, win)).not.toThrow();
    expect(doc.documentElement.dataset.mode).toBe("dark");
  });
});
