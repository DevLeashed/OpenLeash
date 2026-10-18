// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));

import type { Settings } from "../api";
import { get, set } from "../store";
import { Overlays } from "./Overlays";

const branches = ["main", "develop"];

beforeEach(() => {
  set({
    settings: { project: "D:/work/app", projects: [], mcp: [], routes: [] } as unknown as Settings,
    projectInfo: { path: "D:/work/app", exists: true, git: true, branch: "main", branches, memory: [], docs: [], disabled_docs: [], trusted: true, decided: true },
    home: { ...get().home, branch: "main" },
    menu: "branch",
    menuAnchor: new DOMRect(10, 10, 20, 20),
  });
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "project_branch_create") return ["main", "develop", "feature/search"];
    return null;
  });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("creating a branch from the base-branch picker", () => {
  it("offers New branch and selects the created branch as the new-chat base", async () => {
    render(<Overlays z={1} />);

    fireEvent.click(screen.getByRole("button", { name: /New branch/ }));
    fireEvent.change(screen.getByRole("textbox", { name: "New branch name" }), { target: { value: "feature/search" } });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => expect(get().home.branch).toBe("feature/search"));
    expect(get().projectInfo?.branches).toEqual(["main", "develop", "feature/search"]);
    expect(invoke).toHaveBeenCalledWith("project_branch_create", {
      path: "D:/work/app", name: "feature/search", base: "main",
    });
    expect(get().menu).toBeNull();
  });

  it("keeps the form open and shows the backend error when creation fails", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "project_branch_create") throw "branch already exists";
      return null;
    });
    render(<Overlays z={1} />);

    fireEvent.click(screen.getByRole("button", { name: /New branch/ }));
    fireEvent.change(screen.getByRole("textbox", { name: "New branch name" }), { target: { value: "main" } });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    expect((await screen.findByRole("alert")).textContent).toContain("branch already exists");
    expect(get().menu).toBe("branch");
  });
});
