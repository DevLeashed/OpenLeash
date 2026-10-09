/// <reference types="node" />
// Two things this pins, both about a diff being hard to read in the transcript:
//
//   1. It used to unfold on its own. An edit's body is most of the row, and a
//      run that touches a dozen files unfurled a dozen slabs of green and red
//      into the middle of the story. The header already says what the call did
//      and carries the counts, so the fold costs the reader nothing.
//   2. The body sat on --ov-250 and painted its text with hardcoded pastels.
//      25% white on dark is a lit panel; 25% black on light is a near-black
//      well, and #86efac on that is roughly 3:1 — the one row in the transcript
//      you most need to read was the one you could not. Both modes have to be
//      covered by a token or it comes back.
// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { act, cleanup, render } from "@testing-library/react";
import { createElement } from "react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import type { Item, TaskSummary } from "../../api";
import { FeedRows } from "../Session";

const task = { id: "t1", model: "m", cwd: "D:/p", subs: [] } as unknown as TaskSummary;

const edit = (id: string): Item => ({
  id, kind: "tool", text: "", ts: "2026-09-01T00:00:00Z",
  data: {
    name: "edit_file", status: "ok", input: { path: "src/lib.rs" }, meta: "+1 −1",
    diff: [
      { k: "h", t: "@@ -1 +1 @@" },
      { k: "d", t: "const KEEP: usize = 3;" },
      { k: "a", t: "pub const KEEP: usize = 3;" },
    ],
  },
}) as unknown as Item;

const shot = (id: string): Item => ({
  id, kind: "tool", text: "", ts: "2026-09-01T00:00:00Z",
  data: { name: "screenshot", status: "ok", input: {}, images: ["data:image/png;base64,AA"] },
}) as unknown as Item;

/** The group's own summary line, which is what a bare tool call actually renders
 *  as — one call is still a run of calls, folded behind the summary. */
const groupLine = () => document.querySelector<HTMLElement>(".toolgroup > .tg-line");
/** A call's own header, once the group above it is open. */
const head = () => document.querySelector<HTMLElement>(".tool .hd");

afterEach(() => cleanup());

describe("a file edit in the transcript", () => {
  it("stays folded once its group is open, with the call still readable", async () => {
    render(createElement(FeedRows, { items: [edit("e1")], task, showThinking: false }));
    await act(async () => { groupLine()!.click(); });
    expect(document.querySelector(".tool .bd"), "an edit's diff does not unfold on its own").toBeNull();
    expect(head()!.textContent).toContain("Edit");
    expect(head()!.textContent).toContain("src/lib.rs");
    // The counts are in the header's meta, so what changed is not behind a click.
    expect(head()!.textContent).toContain("+1");
  });

  it("still opens on one click", async () => {
    render(createElement(FeedRows, { items: [edit("e1")], task, showThinking: false }));
    await act(async () => { groupLine()!.click(); });
    await act(async () => { head()!.click(); });
    const bd = document.querySelector<HTMLElement>(".tool .bd");
    expect(bd, "the fold is a real fold, not a removal").toBeTruthy();
    expect(bd!.textContent).toContain("pub const KEEP");
  });

  it("leaves a screenshot alone — a picture is the call's whole output", async () => {
    render(createElement(FeedRows, { items: [shot("s1")], task, showThinking: false }));
    await act(async () => { groupLine()!.click(); });
    expect(document.querySelector(".tool .bd")).toBeTruthy();
  });
});

describe("the diff colours", () => {
  const css = readFileSync("src/App.css", "utf8");
  const session = readFileSync("src/ui/Session.tsx", "utf8");

  it("come from tokens, not hexes, so a theme switch can re-point them", () => {
    // Both modes define the same six. A dark-only set is what produced a
    // pastel-on-near-black diff in light mode the first time.
    const dark = css.slice(css.indexOf(":root {"), css.indexOf('[data-theme="raycast"]'));
    const light = css.slice(css.indexOf('[data-mode="light"]'), css.indexOf('[data-mode="light"][data-theme="raycast"]'));
    for (const block of [dark, light]) {
      for (const t of ["--diff-add:", "--diff-del:", "--diff-hunk:", "--diff-add-bg:", "--diff-del-bg:", "--diff-hunk-bg:"]) {
        expect(block, `${t} is defined for this mode`).toContain(t);
      }
    }
    // A tint, not a slab: 25% is what put the text on the wrong side of the
    // contrast floor in the first place.
    for (const t of ["--diff-add-bg", "--diff-del-bg"]) {
      const v = dark.slice(dark.indexOf(t + ":"));
      const a = Number(/rgba\([^)]*?,\s*([\d.]+)\)/.exec(v)?.[1]);
      expect(a, `${t} stays a tint`).toBeLessThanOrEqual(0.15);
    }
  });

  it("leaves the diff code text neutral and colors its sign", () => {
    const row = session.split("\n").find((l) => l.includes('className={"dline " + l.k}'));
    expect(row, "the diff line is built with the sign in its own span").toBeTruthy();
    expect(row).toContain("dsign");
    expect(row).toContain("dtext");
    expect(row).toContain('l.k === "a" ? "var(--diff-add)" : l.k === "d" ? "var(--diff-del)"');
    expect(row).not.toMatch(/color:\s*"#[0-9a-f]{6}"/i);
  });

  it("colors collapsed per-edit additions and deletions independently", () => {
    const row = session.split("\n").find((l) => l.includes("{diffCounts ?"));
    expect(row).toContain('style={{ color: "var(--diff-add)" }}>+{diffCounts[1]}');
    expect(row).toContain('style={{ color: "var(--diff-del)" }}>−{diffCounts[2]}');
  });

  it("no longer paint the tool body a 25% slab", () => {
    const body = css.split("\n").find((l) => l.startsWith(".tool .bd {"));
    expect(body).not.toContain("--ov-250");
    // The bare `.dline > span` rules belong to the review screen only; left
    // unscoped they restyle the transcript's own spans by position.
    expect(css).not.toMatch(/^\.dline\s*\{/m);
    expect(css).toMatch(/^\.diffview \.dline\b/m);
  });
});
