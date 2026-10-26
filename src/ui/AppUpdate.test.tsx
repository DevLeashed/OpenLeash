// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { AppUpdateStatus, TaskSummary } from "../api";
import { get, receiveAppUpdate, set } from "../store";
import { AppUpdateBanner, RELEASES_URL } from "./AppUpdate";
import { AppVersion } from "./AppVersion";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: () => false }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
const update = (status: AppUpdateStatus["status"], can_install = true): AppUpdateStatus => ({ status, version: "2.0.0", error: null, can_install });
beforeEach(() => { set({ appUpdate: update("idle"), appUpdateBusy: false, appUpdateActionError: null, appUpdateDismissed: null, tasks: {} }); vi.mocked(invoke).mockResolvedValue(update("current")); });
afterEach(() => { cleanup(); vi.clearAllMocks(); });
describe("application updater", () => {
  it("opens only the fixed manual release URL on unsupported builds", () => {
    receiveAppUpdate(update("available", false));
    render(<AppUpdateBanner />);
    expect(screen.queryByRole("button", { name: "Restart to update" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Update now" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Open latest release" }));
    expect(openUrl).toHaveBeenCalledWith(RELEASES_URL);
    expect(invoke).not.toHaveBeenCalled();
  });
  it("never installs automatically and requires an explicit restart action", async () => {
    receiveAppUpdate(update("ready"));
    render(<AppUpdateBanner />);
    expect(screen.getByText("Save your work before restarting.")).toBeTruthy();
    expect(invoke).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Restart to update" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("app_update_install"));
  });
  it.each(["running", "waiting", "busy"])("blocks restart for %s tasks", (kind) => {
    set({ tasks: { active: { status: kind === "busy" ? "idle" : kind, busy: kind === "busy" ? 1 : 0 } as TaskSummary } });
    receiveAppUpdate(update("ready"));
    render(<AppUpdateBanner />);
    const button = screen.getByRole("button", { name: "Finish or stop tasks first" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.click(button);
    expect(invoke).not.toHaveBeenCalled();
  });
  it("keeps Later dismissed for repeat events but resurfaces a new status/version", () => {
    receiveAppUpdate(update("downloading"));
    render(<AppUpdateBanner />);
    fireEvent.click(screen.getByRole("button", { name: "Later" }));
    act(() => receiveAppUpdate(update("downloading")));
    expect(screen.queryByRole("region")).toBeNull();
    act(() => receiveAppUpdate(update("ready")));
    expect(screen.getByRole("region", { name: "Application update" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Later" }));
    act(() => receiveAppUpdate({ ...update("ready"), version: "3.0.0" }));
    expect(screen.getByRole("region")).toBeTruthy();
  });
  it("shows errors only in About and checks through typed IPC", async () => {
    receiveAppUpdate({ ...update("error", false), error: "Signature verification failed" });
    render(<><AppUpdateBanner /><AppVersion /></>);
    expect(screen.getByRole("region", { name: "Application update" })).toBeTruthy();
    expect(screen.getAllByRole("alert").some((node) => node.textContent?.includes("Signature verification failed"))).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("app_update_check"));
    await waitFor(() => expect(get().appUpdate.status).toBe("current"));
  });
  it("allows retrying a download without installing", async () => {
    receiveAppUpdate(update("available"));
    render(<AppVersion />);
    fireEvent.click(screen.getByRole("button", { name: "Download only" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("app_update_download"));
    expect(invoke).not.toHaveBeenCalledWith("app_update_install");
  });
  it("offers a separate download action once a check finds a release", async () => {
    // A check only finds the release. Fetching the package is a separate
    // step the user asks for, so the two are two buttons.
    receiveAppUpdate(update("available"));
    render(<AppVersion />);
    expect(screen.getByRole("button", { name: "Update now" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Download only" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry download" })).toBeNull();
  });
  it("updates with one click through a single backend call", async () => {
    // The check, the download and the restart are chained inside the backend so
    // they cannot interleave with the background pass or a second click. The
    // click must not assemble the sequence out of separate commands itself.
    receiveAppUpdate(update("available"));
    vi.mocked(invoke).mockResolvedValue(update("ready"));
    render(<AppVersion />);
    fireEvent.click(screen.getByRole("button", { name: "Update now" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("app_update_now"));
    expect(invoke).not.toHaveBeenCalledWith("app_update_check");
    expect(invoke).not.toHaveBeenCalledWith("app_update_download");
    expect(invoke).not.toHaveBeenCalledWith("app_update_install");
    await waitFor(() => expect(get().appUpdate.status).toBe("ready"));
  });
  it("leaves the restart to the user when the one click could only download", async () => {
    // The backend refuses to close the app while work is running and answers
    // with the downloaded release, so the click ends on the explicit restart.
    vi.mocked(invoke).mockResolvedValue(update("ready"));
    receiveAppUpdate(update("available"));
    render(<AppUpdateBanner />);
    fireEvent.click(screen.getByRole("button", { name: "Update now" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("app_update_now"));
    await waitFor(() => expect(screen.getByRole("button", { name: "Restart to update" })).toBeTruthy());
    expect(invoke).not.toHaveBeenCalledWith("app_update_install");
  });
  it("does not start a download when the user asks to check for updates", async () => {
    // The check command is the only thing the user pressed. Nothing may be
    // fetched on the strength of it.
    vi.mocked(invoke).mockResolvedValue(update("available"));
    receiveAppUpdate(update("idle"));
    render(<AppVersion />);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("app_update_check"));
    expect(invoke).not.toHaveBeenCalledWith("app_update_download");
  });
  it("retains ready state and reports an authoritative backend install rejection", async () => {
    vi.mocked(invoke).mockRejectedValue("Finish or stop tasks first");
    receiveAppUpdate(update("ready"));
    render(<AppVersion />);
    fireEvent.click(screen.getByRole("button", { name: "Restart to update" }));
    await waitFor(() => expect(screen.getAllByRole("alert").some((node) => node.textContent === "Finish or stop tasks first")).toBe(true));
    expect(get().appUpdate.status).toBe("ready");
  });
});
