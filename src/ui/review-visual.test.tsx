// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { api, type FileDiff, type Settings, type TaskSummary, type VisualEntry } from "../api";
import { set } from "../store";
import { Review } from "./Review";

const task = { id: "review-visual", title: "Restyle the page" } as TaskSummary;
const file: FileDiff = { path: "src/page.css", status: "M", add: 1, del: 1, lines: [{ k: "a", t: "added" }] };
const PNG = "data:image/png;base64,iVBORw0KGgo=";
const dir = "C:/Users/me/AppData/Local/OpenLeash/proof/t/";
const entry: VisualEntry = { url: "http://localhost:5173/", before: dir + "before-0.png", after: dir + "after-0.png", before_at: "2026-10-17T10:00:00Z", after_at: "2026-10-17T10:05:00Z" };

beforeEach(() => {
  vi.spyOn(api, "review").mockResolvedValue({ git: true, files: [file] });
  vi.spyOn(api, "replyImage").mockResolvedValue(PNG);
  set({ task: task.id, tasks: { [task.id]: task }, settings: {} as Settings });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("Review's visual diff", () => {
  it("shows a before/after slider for each captured URL", async () => {
    vi.spyOn(api, "visualDiff").mockResolvedValue({ entries: [entry] });
    const view = render(<Review />);
    expect(await view.findByText("Visual changes (1)")).toBeTruthy();
    expect(view.getByRole("slider", { name: "Before after split" })).toBeTruthy();
    expect(await view.findByAltText("Before")).toBeTruthy();
    expect(await view.findByAltText("After")).toBeTruthy();
    expect(api.visualDiff).toHaveBeenCalledWith(task.id);
    expect(view.getByRole("heading", { name: entry.url })).toBeTruthy();
    // The file diff is still there below it.
    expect(await view.findByRole("button", { name: /Looks good/ })).toBeTruthy();
  });

  it("renders nothing extra when there are no entries", async () => {
    vi.spyOn(api, "visualDiff").mockResolvedValue({ entries: [] });
    const view = render(<Review />);
    await view.findByRole("button", { name: /Looks good/ });
    await waitFor(() => expect(api.visualDiff).toHaveBeenCalled());
    expect(view.queryByText(/Visual changes/)).toBeNull();
    expect(view.queryByRole("slider")).toBeNull();
    expect(view.queryByRole("button", { name: /Recapture after/ })).toBeNull();
  });

  it("stays quiet when the backend has no visual diff", async () => {
    vi.spyOn(api, "visualDiff").mockRejectedValue("unknown command task_visual_diff");
    const view = render(<Review />);
    expect(await view.findByRole("button", { name: /Looks good/ })).toBeTruthy();
    await waitFor(() => expect(api.visualDiff).toHaveBeenCalled());
    expect(view.queryByText(/Visual changes/)).toBeNull();
    expect(view.queryByRole("alert")).toBeNull();
  });

  it("shows the visual section even when there are no file changes", async () => {
    vi.spyOn(api, "review").mockResolvedValue({ git: true, files: [] });
    vi.spyOn(api, "visualDiff").mockResolvedValue({ entries: [entry] });
    const view = render(<Review />);
    expect(await view.findByText("No changes yet.")).toBeTruthy();
    expect(await view.findByText("Visual changes (1)")).toBeTruthy();
  });

  it("recaptures the after shots and renders what comes back", async () => {
    vi.spyOn(api, "visualDiff").mockResolvedValue({ entries: [entry] });
    const recapture = vi.spyOn(api, "visualRecapture").mockResolvedValue({
      entries: [entry, { url: "http://localhost:5173/settings", before: dir + "before-1.png", after: null, before_at: "2026-10-17T10:01:00Z", after_at: null }],
    });
    const view = render(<Review />);
    fireEvent.click(await view.findByRole("button", { name: /Recapture after/ }));
    expect(recapture).toHaveBeenCalledWith(task.id);
    expect(await view.findByText("Visual changes (2)")).toBeTruthy();
    expect(view.getByText(/No after capture yet/)).toBeTruthy();
  });

  it("shows why a recapture failed", async () => {
    vi.spyOn(api, "visualDiff").mockResolvedValue({ entries: [entry] });
    vi.spyOn(api, "visualRecapture").mockRejectedValue("The browser plugin is turned off (Settings → Plugins)");
    const view = render(<Review />);
    fireEvent.click(await view.findByRole("button", { name: /Recapture after/ }));
    expect((await view.findByRole("alert")).textContent).toContain("browser plugin is turned off");
    expect(view.getByText("Visual changes (1)")).toBeTruthy();
    expect((view.getByRole("button", { name: /Recapture after/ }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("discards the visual diff and hides the section", async () => {
    vi.spyOn(api, "visualDiff").mockResolvedValue({ entries: [entry] });
    const clear = vi.spyOn(api, "visualClear").mockResolvedValue(null);
    const view = render(<Review />);
    fireEvent.click(await view.findByRole("button", { name: /Discard/ }));
    expect(clear).toHaveBeenCalledWith(task.id);
    await waitFor(() => expect(view.queryByText(/Visual changes/)).toBeNull());
    expect(view.queryByRole("slider")).toBeNull();
  });
});
