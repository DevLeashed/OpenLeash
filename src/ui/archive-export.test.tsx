// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

// The save dialog is a native window the tests cannot open. It hands back a path
// held in a variable rather than a constant, so a test that needs the flow to go
// all the way through — or stop at the dialog — has something to set.
let savePath: string | null = "out/archive.json";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null), save: vi.fn(async () => savePath) }));
vi.mock("./primitives", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./primitives")>();
  return { ...actual, MorphText: ({ children }: { children: React.ReactNode }) => <>{children}</> };
});

import type { Settings, TaskSummary } from "../api";
import { get, set } from "../store";
import { SettingsView } from "./Settings";

/** A chat of the shape the settings page reads: the archived rows sort on
 *  `updated_at` and name their project off it, so a fixture without one makes
 *  the page throw before any button is reached. */
const chat = (id: string, over: Record<string, unknown> = {}) =>
  ({ id, title: id, archived: false, pinned: false, status: "stopped", touched_at: "", updated_at: "", project: "D:/proj", cwd: "D:/proj", ...over }) as unknown as TaskSummary;

/** Settings › Chats is the only screen with the archive, so the tests land there. */
function chatsTab(settings: Settings, tasks: Record<string, TaskSummary>) {
  set({ settings, settingsTab: "chats", tasks });
  render(<SettingsView />);
}

/** The archive's own export row. It is matched by its own title rather than by
 *  the button label: the active-chats row above says "Export…" too, and two
 *  controls sharing one name is how a test ends up pressing the wrong one. */
function archiveExportRow() {
  return [...document.querySelectorAll<HTMLElement>(".srowx")]
    .find((row) => row.textContent?.startsWith("Export archived chats"))!;
}

const base = { paused_all: false } as Settings;

describe("archived chat export", () => {
  // Set before the screen renders: the count the button promises is read off
  // whatever `tasks_export` answers with, and the assertion below is what that
  // number is checked against.
  beforeEach(() => {
    vi.mocked(invoke).mockImplementation(async (command, args) =>
      command === "tasks_export" ? (args as { ids?: string[] })?.ids?.length ?? 0 : null);
  });
  beforeEach(() => {
    // Archived, live, and a running one: an export may only take the first.
    chatsTab(base, {
      a: chat("a", { archived: true, title: "Old build" }),
      b: chat("b", { archived: true, title: "Older build" }),
      c: chat("c", { archived: false, title: "Current work" }),
      d: chat("d", { archived: false, status: "running" }),
    });
  });
  afterEach(() => {
    cleanup();
    // Restoring the mocks puts them back to their original stubs, which is what
    // keeps the next test's `mockImplementation` from stacking on the last one —
    // but call history survives the restore, so a click recorded by one test
    // would still be sitting in `calls` when the next one asserted on it.
    vi.restoreAllMocks();
    vi.clearAllMocks();
    // The toast is store state, not a mock, and restoring mocks does not touch it:
    // a test that asserted "exported 2" would leave that sentence on screen for
    // the next one to trip over.
    set({ toast: null });
    savePath = "out/archive.json";
  });

  it("offers to export the archive once something is archived", () => {
    expect(archiveExportRow().textContent).toContain("2 archived chats");
    expect((archiveExportRow().querySelector("button") as HTMLButtonElement).disabled).toBe(false);
  });

  it("disables the export and says why when nothing is archived", () => {
    chatsTab(base, { c: chat("c"), d: chat("d", { status: "running" }) });
    expect(archiveExportRow().textContent).toContain("Nothing archived");
    expect((archiveExportRow().querySelector("button") as HTMLButtonElement).disabled).toBe(true);
  });

  it("exports exactly the archived chats, leaving the live ones out", async () => {
    // The path the dialog would have handed back: without it the flow stops at
    // the dialog and nothing is written.
    savePath = "out/archive.json";
    // The export runs after the click returns, so the assertions wait for it
    // rather than reading a store the write has not landed in yet.
    await act(async () => { fireEvent.click(archiveExportRow().querySelector("button")!); });
    // The archive, and nothing else — the active chats, including the running one
    // that "export all" would also skip, must not ride along. The ids come off
    // the store at click time, so a chat restored a moment ago is left out too.
    expect(vi.mocked(invoke).mock.calls.find(([command]) => command === "tasks_export")?.[1]).toEqual({ ids: ["a", "b"], path: "out/archive.json" });
    expect(get().toast).toBe("Exported 2 archived chats");
  });

  it("stops at the save dialog without exporting anything", async () => {
    // A cancelled dialog is a null path, and the flow has to honour that rather
    // than write a file nobody chose: no chats leave, and nothing is claimed.
    savePath = null;
    await act(async () => { fireEvent.click(archiveExportRow().querySelector("button")!); });
    expect(vi.mocked(invoke).mock.calls.find(([command]) => command === "tasks_export")).toBeUndefined();
    expect(get().toast).toBeNull();
  });
});
