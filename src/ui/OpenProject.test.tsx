// @vitest-environment jsdom
// The open-project sources: WSL goes to a folder dialog that starts inside the
// distro, and Clone shows the backend's plan before it runs, then opens what it
// cloned. The URL vetting itself lives in Rust (`agent::projects`) and is
// tested there; these pin the wiring and what the dialog promises.
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null), save: vi.fn(async () => null) }));

import type { Settings } from "../api";
import { get, set } from "../store";
import { loadWsl, openFromWsl, OpenProjectDialogs } from "./OpenProject";

const UBUNTU = { name: "Ubuntu", root: "\\\\wsl.localhost\\Ubuntu" };
const DEBIAN = { name: "Debian", root: "\\\\wsl.localhost\\Debian" };
const calls: [string, unknown][] = [];

beforeEach(() => {
  calls.length = 0;
  set({ settings: { project: "", projects: [] } as unknown as Settings, openFrom: null, wsl: [], trustPrompt: false });
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    calls.push([cmd, args]);
    if (cmd === "wsl_list") return [UBUNTU, DEBIAN];
    if (cmd === "wsl_start_dir") return "\\\\wsl.localhost\\Ubuntu\\home\\me";
    if (cmd === "project_info") return { exists: true, decided: true, trusted: true, branch: "main", branches: [], memory: [], docs: [] };
    if (cmd === "settings_update") return { ...get().settings, ...(args as { patch: object }).patch };
    if (cmd === "clone_preview") {
      const url = (args as { url: string }).url;
      if (url.startsWith("-")) throw "That doesn't look like a repository URL.";
      return { url: "https://github.com/o/r", name: "r", dest: "C:\\Users\\me\\openleash\\r", existing: false };
    }
    if (cmd === "clone_repo") return "C:\\Users\\me\\openleash\\r";
    return [];
  });
  vi.mocked(open).mockReset();
});
afterEach(cleanup);

it("lists distros into the store", async () => {
  await loadWsl();
  expect(get().wsl.map((d) => d.name)).toEqual(["Ubuntu", "Debian"]);
});

it("several distros ask which one first", () => {
  set({ wsl: [UBUNTU, DEBIAN] });
  openFromWsl();
  expect(get().openFrom).toBe("wsl");
  render(<OpenProjectDialogs />);
  expect(screen.getByText("Debian")).toBeTruthy();
});

it("one distro opens the folder dialog inside it and opens the pick", async () => {
  set({ wsl: [UBUNTU] });
  vi.mocked(open).mockResolvedValue("\\\\wsl.localhost\\Ubuntu\\home\\me\\app");
  openFromWsl();
  await waitFor(() => expect(get().settings?.project).toBe("\\\\wsl.localhost\\Ubuntu\\home\\me\\app"));
  expect(calls).toContainEqual(["wsl_start_dir", { distro: "Ubuntu" }]);
  expect(vi.mocked(open).mock.calls[0]![0]).toMatchObject({ directory: true, defaultPath: "\\\\wsl.localhost\\Ubuntu\\home\\me" });
  expect(get().openFrom).toBe(null);
});

it("clone previews the destination, then clones and opens it", async () => {
  set({ openFrom: "clone" });
  render(<OpenProjectDialogs />);
  const clone = screen.getByRole("button", { name: "Clone" }) as HTMLButtonElement;
  expect(clone.disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "github.com/o/r/tree/main" } });
  await screen.findByText("C:\\Users\\me\\openleash\\r");
  expect(clone.disabled).toBe(false);
  fireEvent.click(clone);
  await waitFor(() => expect(get().settings?.project).toBe("C:\\Users\\me\\openleash\\r"));
  expect(calls).toContainEqual(["clone_repo", { url: "github.com/o/r/tree/main" }]);
  expect(get().openFrom).toBe(null);
});

it("a refused URL shows why and can't be cloned", async () => {
  set({ openFrom: "clone" });
  render(<OpenProjectDialogs />);
  fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "--upload-pack=x" } });
  await screen.findByRole("alert");
  expect((screen.getByRole("button", { name: "Clone" }) as HTMLButtonElement).disabled).toBe(true);
  expect(calls.some(([c]) => c === "clone_repo")).toBe(false);
});
