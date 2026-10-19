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

  it("clearly distinguishes stopping screen control from stopping the run", () => {
    expect(frontend).toContain("OpenLeash is controlling your computer · Esc to stop");
    expect(frontend).toContain("Esc to stop");
    expect(frontend).toContain("press it twice to stop the entire run");
  });

  it("paints the native window edges and fills its available height", () => {
    expect(frontend).toMatch(/html, body, #root \{[^}]*background: #18181b/);
    expect(frontend).toMatch(/\.bar \{\s*margin: 0; height: 100%/);
  });

  it("sizes for the always-on-top control instructions", () => {
    expect(native).toContain('.inner_size(560.0, 62.0)');
    expect(native).toContain('.title("OpenLeash is controlling your computer · Esc to stop")');
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
