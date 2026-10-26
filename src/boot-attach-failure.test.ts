// @vitest-environment jsdom
// Booting attaches the listener set before asking the backend for anything.
// A registration that fails used to reject out of `boot()` before `api.boot()`
// ran: `ready` never became true, `bootError` was never set, and the window sat
// on "Starting…" for the life of the process with an unhandled rejection and no
// way to retry. These tests pin the visible failure and the retry.
import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { boot, disposeBoot, get } from "./store";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ isFocused: async () => true, isVisible: async () => true, isMinimized: async () => false, onFocusChanged: async () => () => {} }) }));

const bootPayload = {
  settings: { model: "", perm: "turbo", effort: 2, project: "", routes: [] },
  tasks: [], providers: [], models: [], accounts: [], agents: [],
};

afterEach(() => { disposeBoot(); vi.resetAllMocks(); });

it("reports a listener that would not attach, instead of loading for ever", async () => {
  vi.mocked(listen).mockImplementation(async (event) => {
    if (event === "ol://event") throw new Error("IPC is not ready");
    return () => {};
  });
  vi.mocked(invoke).mockResolvedValue(bootPayload);

  await boot();

  expect(get().ready).toBe(false);
  expect(get().bootError).toContain("IPC is not ready");
});

it("tears down the listeners that did attach, so a retry cannot double-deliver", async () => {
  const detached: string[] = [];
  vi.mocked(listen).mockImplementation(async (event) => {
    if (event === "ol://event") throw new Error("IPC is not ready");
    return () => { detached.push(event); };
  });
  vi.mocked(invoke).mockResolvedValue(bootPayload);

  await boot();

  // A half-installed set left behind would deliver every event twice once the
  // retry attaches its own copy — duplicated transcript rows, duplicated toasts.
  expect(detached.length).toBeGreaterThan(0);
});

it("boots on a retry once the listeners attach", async () => {
  let fail = true;
  vi.mocked(listen).mockImplementation(async (event) => {
    if (fail && event === "ol://event") throw new Error("IPC is not ready");
    return () => {};
  });
  vi.mocked(invoke).mockResolvedValue(bootPayload);

  await boot();
  expect(get().bootError).toContain("IPC is not ready");

  // The failed attempt must not be cached: the second call has to run again.
  fail = false;
  await boot();
  expect(get().ready).toBe(true);
  expect(get().bootError).toBeNull();
});
