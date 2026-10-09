// @vitest-environment jsdom
// Finished background commands used to sit in the Details panel as an open
// column of identical green-tick rows, pushing the live jobs down it. A chat
// that had run tests, a build and a dev server looked like it had thirty things
// going, when one was still running.
//
// They are receipts, not work in progress: a job whose process is gone has no
// Stop button left to reach for and nothing about it can change. So they fold
// behind a Done line, exactly as the swarm tree's finished agents already do —
// one shape for "this is over, here is the record" wherever it appears.
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { createElement } from "react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import type { Bg, TaskSummary } from "../../api";
import { BgCommands } from "../Session";

const bg = (id: string, running: boolean, extra: Partial<Bg> = {}): Bg =>
  ({ id, cmd: `npm run ${id}`, started: "2026-01-01T00:00:00Z", last_line: "ready", running, exit: running ? null : "exit 0", ...extra }) as Bg;

const task = { id: "t1", title: "chat", status: "running", paused: null, model: "m", cwd: "D:/p", subs: [], ultra: false } as unknown as TaskSummary;

/** Every command row the panel actually mounted, live and folded alike. */
const rows = () => [...document.querySelectorAll(".drow")].map((r) => r.textContent ?? "").join(" ");
const doneLine = () => [...document.querySelectorAll<HTMLElement>(".tg-line")].find((b) => b.textContent?.includes("Done"));

afterEach(() => cleanup());

describe("background commands in the details panel", () => {
  it("keeps the running job visible and folds the finished ones behind Done", async () => {
    render(createElement(BgCommands, { task, bgs: [bg("dev", true), bg("test", false), bg("build", false)] }));
    // The one job you could still act on is on screen without scrolling.
    expect(rows()).toContain("npm run dev");
    expect(document.querySelector('[aria-label="Stop"]'), "the one job you can still act on is the one left in reach").toBeTruthy();
    // The receipts are not, and the line says exactly how many are behind it.
    expect(rows()).not.toContain("npm run test");
    expect(rows()).not.toContain("npm run build");
    expect(doneLine()?.textContent).toContain("2 commands");

    await act(async () => { doneLine()!.click(); });
    expect(rows(), "opening the fold shows what is behind it").toContain("npm run test");
  });

  it("counts a command that came back non-zero as one that did not finish", async () => {
    // The call that started the job succeeded; the process it launched is what
    // failed, and that is the only thing that says how it went.
    render(createElement(BgCommands, { task, bgs: [bg("bad", false, { exit: "exit 1", last_line: "boom" }), bg("ok", false)] }));
    expect(doneLine()?.textContent).toContain("2 commands");
    expect(doneLine()?.textContent).toContain("1 did not finish");
    await act(async () => { doneLine()!.click(); });
    expect(rows()).toContain("exit 1 · boom");
  });

  it("counts a force-paused command as unfinished too", async () => {
    render(createElement(BgCommands, { task, bgs: [bg("killed", false, { exit: "killed" })] }));
    // Never ran to completion, and the agent was told to rerun it — the one the
    // user most needs to notice, so it cannot read as a clean exit.
    expect(doneLine()?.textContent).toContain("1 did not finish");
  });

  it("shows nothing finished when nothing has", () => {
    render(createElement(BgCommands, { task, bgs: [bg("dev", true)] }));
    expect(doneLine(), "a fold over an empty set would be a lie").toBeUndefined();
    expect(rows()).toContain("npm run dev");
  });
});