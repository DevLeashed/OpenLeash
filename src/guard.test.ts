/// <reference types="node" />
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const frontend = readFileSync("src/guard.tsx", "utf8");
const native = readFileSync("src-tauri/src/lib.rs", "utf8");
const show = native.slice(native.indexOf("pub fn show_guard("), native.indexOf("fn show_main("));

describe("PC-control banner layout and visibility", () => {
  it("uses a compact stopped headline rather than clipping the safety message", () => {
    expect(frontend).toContain('"Screen control stopped"');
    expect(frontend).toContain('"You pressed Esc. The agent was told to stop."');
  });

  it("paints the native window edges and fills its available height", () => {
    expect(frontend).toMatch(/html, body, #root \{[^}]*background: #18181b/);
    expect(frontend).toMatch(/\.bar \{\s*margin: 0; height: 100%/);
  });

  it("reasserts topmost after showing without stealing keyboard focus", () => {
    expect(show).toMatch(/w\.show\(\)[\s\S]*w\.set_always_on_top\(true\)/);
    expect(show).not.toContain("set_focus");
  });

  it("anchors on the monitor using physical coordinates and the actual banner size", () => {
    expect(show).toContain("main.current_monitor()");
    expect(show).toContain("w.outer_size()");
    expect(show).toContain("PhysicalPosition::new");
    expect(show).not.toContain("LogicalPosition");
  });
});
