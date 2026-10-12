// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen, type EventCallback } from "@tauri-apps/api/event";
import type { AppUpdateStatus } from "./api";
import { boot, disposeBoot, get, set } from "./store";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ isFocused: async () => true, isVisible: async () => true, isMinimized: async () => false, onFocusChanged: async () => () => {} }) }));
afterEach(() => { disposeBoot(); vi.resetAllMocks(); });
it("attaches updater events before snapshot, rejects stale snapshots and detaches on disposal", async () => {
  set({ appUpdate: { status: "idle", version: null, error: null, can_install: false } });
  const off = vi.fn();
  let callback!: EventCallback<AppUpdateStatus>;
  vi.mocked(listen).mockImplementation(async (event, handler) => {
    if (event === "ol://app-update") callback = handler as EventCallback<AppUpdateStatus>;
    return off;
  });
  let snapshot!: (status: AppUpdateStatus) => void;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "app_update_status") {
      expect(callback).toBeTypeOf("function");
      return new Promise<AppUpdateStatus>((resolve) => { snapshot = resolve; });
    }
    if (command === "app_boot") return { settings: { model: "", perm: "turbo", effort: 2, project: "", routes: [] }, tasks: [], providers: [], models: [], accounts: [], agents: [] };
    return null;
  });
  await boot();
  callback({ event: "ol://app-update", id: 1, payload: { status: "ready", version: "2.0.0", error: null, can_install: true } });
  snapshot({ status: "checking", version: null, error: null, can_install: true });
  await Promise.resolve();
  expect(get().appUpdate.status).toBe("ready");
  disposeBoot();
  expect(off).toHaveBeenCalled();
});
