// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import type { AppUpdateStatus, Settings } from "../api";
import { get, set } from "../store";
import { AppVersion } from "./AppVersion";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: () => false }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn(async () => "1.0.0") }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

const update = (status: AppUpdateStatus["status"]): AppUpdateStatus => ({
  status,
  version: "2.0.0",
  error: null,
  can_install: true,
});

/** A settings object with both update switches off, which is the default. */
const settings = (patch: Partial<Settings> = {}) =>
  ({ auto_check_updates: false, auto_update: false, ...patch } as unknown as Settings);

beforeEach(() => {
  set({ appUpdate: update("idle"), settings: settings(), appUpdateBusy: false, appUpdateActionError: null, appUpdateDismissed: null, tasks: {} });
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    if (cmd === "settings_update") {
      const patch = (args as { patch?: Partial<Settings> } | undefined)?.patch ?? {};
      return settings(patch) as never;
    }
    return update("current") as never;
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });

describe("automatic update settings", () => {
  it("shows both switches off by default", () => {
    render(<AppVersion />);
    const check = screen.getByRole("switch", { name: "Check for updates automatically" });
    const download = screen.getByRole("switch", { name: "Update automatically" });
    expect(check.getAttribute("aria-checked")).toBe("false");
    expect(download.getAttribute("aria-checked")).toBe("false");
  });

  it("persists turning the automatic check on", async () => {
    render(<AppVersion />);
    const check = screen.getByRole("switch", { name: "Check for updates automatically" });
    await act(async () => { fireEvent.click(check); });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { auto_check_updates: true } }));
    expect(get().settings?.auto_check_updates).toBe(true);
  });

  it("persists turning the automatic download on", async () => {
    render(<AppVersion />);
    const download = screen.getByRole("switch", { name: "Update automatically" });
    await act(async () => { fireEvent.click(download); });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { auto_update: true } }));
    expect(get().settings?.auto_update).toBe(true);
  });

  it("shows a switch the user already turned on as on", () => {
    set({ settings: settings({ auto_check_updates: true, auto_update: true }) });
    render(<AppVersion />);
    expect(screen.getByRole("switch", { name: "Check for updates automatically" }).getAttribute("aria-checked")).toBe("true");
    expect(screen.getByRole("switch", { name: "Update automatically" }).getAttribute("aria-checked")).toBe("true");
  });

  it("says what the switches do, including the restart", () => {
    render(<AppVersion />);
    // The copy has to match what the app actually does: with both switches off
    // nothing reaches GitHub, and with automatic updates on it installs a
    // release and restarts itself.
    expect(screen.getAllByText(/off by default: nothing is checked, downloaded or installed/i).length).toBeGreaterThan(0);
    expect(screen.getByText(/restarts itself to finish/i)).toBeTruthy();
  });
});
