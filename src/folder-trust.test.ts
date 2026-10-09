import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import type { Settings } from "./api";
import { get, loadProject, openProject, set } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

beforeEach(() => {
  set({ settings: { project: "c:/work/app", projects: [] } as unknown as Settings, trustPrompt: false });
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    if (cmd === "project_info") return { exists: true, decided: false, trusted: false, branch: "main" };
    if (cmd === "settings_update") return { ...get().settings, ...(args as { patch: object }).patch };
    return [];
  });
});

it("metadata refresh does not ask for trust", async () => {
  await loadProject("c:/work/app");
  expect(get().trustPrompt).toBe(false);
});

it("opening an undecided folder asks for trust", async () => {
  expect(await openProject("c:/work/new")).toBe(true);
  expect(get().trustPrompt).toBe(true);
});

it.each([true, false])("a remembered decision does not prompt again (trusted=%s)", async (trusted) => {
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    if (cmd === "project_info") return { exists: true, decided: true, trusted, branch: "main" };
    if (cmd === "settings_update") return { ...get().settings, ...(args as { patch: object }).patch };
    return [];
  });
  set({ trustPrompt: true });
  expect(await openProject("c:/work/app")).toBe(true);
  expect(get().trustPrompt).toBe(false);
});
