// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { api, ATTRIBUTIONS, type FileDiff, type Settings, type TaskSummary } from "../api";
import { set } from "../store";
import { Review } from "./Review";

const task = { id: "review-layout", title: "Fix layout", branch: "feature/" + "long-branch-".repeat(20) } as TaskSummary;
const file: FileDiff = { path: "src/" + "long-path/".repeat(20) + "Review.tsx", status: "M", add: 1, del: 1, lines: [{ k: "a", t: "added" }] };
const css = readFileSync("src/App.css", "utf8");
const rule = (selector: string) => {
  const start = css.indexOf(selector + " {");
  return start < 0 ? "" : css.slice(start + selector.length + 2, css.indexOf("}", start));
};

beforeEach(() => {
  vi.spyOn(api, "review").mockResolvedValue({ git: true, files: [file] });
  set({ task: task.id, tasks: { [task.id]: task }, settings: {} as Settings });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("review controls keep their text inside their bounds", () => {
  it("keeps attribution labels visible and explanations in the picker", async () => {
    const view = render(<Review />);
    const picker = await view.findByRole("combobox", { name: "Commit attribution" });
    expect(picker.querySelector(".ddv")?.textContent).toBe("Co-authored-by");
    expect(picker.querySelector(".ddh")).toBeNull();
    fireEvent.click(picker);
    expect(view.getByRole("listbox").textContent).toContain(ATTRIBUTIONS[0]!.hint);
    expect(view.getByRole("textbox", { name: "Commit message" })).toBeTruthy();
  });

  it("bounds long branch and file names without squeezing the actions", async () => {
    const view = render(<Review />);
    await view.findByRole("button", { name: /Looks good/ });
    expect(view.container.querySelector(".review-branch")?.getAttribute("title")).toBe(task.branch);
    expect(view.container.querySelector(".review-file-path")?.getAttribute("title")).toBe(file.path);
    expect(view.container.querySelector(".review-commit")?.contains(view.getByRole("button", { name: /Commit/ }))).toBe(true);
  });

  // jsdom has no layout. Pin the flex/overflow contract separately so a fixed
  // single-row height or an unbounded filename cannot quietly come back.
  it("allows the toolbar to wrap and grow at narrow widths and higher zoom", () => {
    expect(rule(".shdr.review-header")).toMatch(/flex-wrap:\s*wrap/);
    expect(rule(".shdr.review-header")).toMatch(/height:\s*auto/);
    expect(rule(".review-commit")).toMatch(/flex-wrap:\s*wrap/);
    expect(rule(".review-title")).toMatch(/white-space:\s*nowrap/);
    expect(rule(".review-branch")).toMatch(/text-overflow:\s*ellipsis/);
    expect(rule(".review-file-path")).toMatch(/min-width:\s*0/);
    expect(rule(".review-file-path")).toMatch(/text-overflow:\s*ellipsis/);
    expect(rule(".review-file-header > .btn")).toMatch(/flex:\s*none/);
  });
});
