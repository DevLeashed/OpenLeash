import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/** Source-level assertions, in the style `ui.test.ts` already uses for App.css.
 *
 *  These exist because the failure they guard is invisible at runtime: eleven of
 *  seventeen settings pages were reachable only by clicking, and the palette
 *  looked perfectly healthy while it happened. */
describe("the command palette's coverage", () => {
  const overlays = readFileSync("src/ui/Overlays.tsx", "utf8");
  const tabs = readFileSync("src/ui/settingsTabs.ts", "utf8");

  it("builds its settings rows from the one list the nav also reads", () => {
    expect(overlays, "the palette maps SETTINGS_TABS rather than naming pages by hand").toMatch(/SETTINGS_TABS\.map\(/);
    expect(overlays).toMatch(/from "\.\/settingsTabs"/);
  });

  it("keeps the settings page list unique and complete enough to be the source", () => {
    const ids = [...tabs.matchAll(/^\s*\["([a-z_]+)",/gm)].map((m) => m[1]);
    // A page added to the Settings render switch but not to this list is
    // unreachable from both the nav and the palette, which is the bug this pins.
    expect(ids.length).toBeGreaterThanOrEqual(15);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("keeps the keyboard reference reachable from the palette", () => {
    expect(overlays).toMatch(/ShortcutSheet/);
    expect(overlays, "the row that opens it").toMatch(/Keyboard shortcuts/);
  });
});
