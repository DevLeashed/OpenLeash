/// Where a right-click menu lands. The project list opens downwards from the
/// titlebar, so the folder you right-click is usually near the bottom of the
/// screen: the menu has to turn up rather than hang off the edge, and it has to
/// do that inside the app root's zoom.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { afterEach, describe, expect, it, vi } from "vitest";
import { ctxPlace, menuLayer } from "./ui/primitives";

const W = 228;
const H = 76;

/** `ctxPlace` works in app-root pixels, so the window is described at zoom 1. */
function atZoom(z: number, innerWidth: number, innerHeight: number) {
  (globalThis as any).window = { innerWidth, innerHeight };
  return z;
}

describe("context menu placement", () => {
  afterEach(() => { delete (globalThis as any).window; });

  it("opens at the cursor when there is room below it", () => {
    const at = ctxPlace(200, 300, W, H, atZoom(1, 1440, 900));
    expect(at).toMatchObject({ top: 300, bottom: "auto", left: 200, width: W });
  });

  it("turns upwards near the bottom edge instead of running off the screen", () => {
    const at = ctxPlace(200, 860, W, H, atZoom(1, 1440, 900));
    expect(at.top).toBe("auto");
    // Anchored by its bottom edge, just above the cursor.
    expect(at.bottom).toBe((900 - 860) / 1 + 6);
  });

  it("keeps the menu on screen at the right edge", () => {
    const at = ctxPlace(1430, 300, W, H, atZoom(1, 1440, 900));
    expect(at.left).toBe(1440 - W - 8);
  });

  it("keeps the menu on screen at the left and top edges", () => {
    const at = ctxPlace(0, 0, W, H, atZoom(1, 1440, 900));
    expect(at.left).toBe(8);
    expect(at.top).toBe(0);
  });

  it("never offers more height than the window has", () => {
    const at = ctxPlace(200, 300, W, H, atZoom(1, 1440, 900));
    expect(at.maxHeight).toBe(900 - 16);
  });

  it("converts cursor pixels through the app zoom, since the layer carries it", () => {
    // At 200% a press at x=400 is 200 in the zoomed layer the menu is placed in.
    const at = ctxPlace(400, 600, W, H, atZoom(2, 1440, 900));
    expect(at.left).toBe(200);
    expect(at.top).toBe(300);
  });

  it("carries the same zoom on its layer, or the menu renders at the wrong size", () => {
    expect(menuLayer(1.25)).toMatchObject({ zoom: 1.25, zIndex: 90 });
  });
});
