// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";
import { version } from "../../package.json";
import { AppVersion } from "./AppVersion";

vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

describe("app version", () => {
  it("shows the installed binary version, not the frontend metadata", async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    vi.mocked(getVersion).mockResolvedValue("2.3.4-beta.1");
    render(<AppVersion />);
    await waitFor(() => expect(screen.getByRole("status", { name: "App version" }).textContent).toBe("v2.3.4-beta.1"));
    expect(getVersion).toHaveBeenCalledOnce();
    expect(screen.getByText(/Automatic updates are not enabled/)).toBeTruthy();
  });

  it("uses canonical package metadata in a browser preview", async () => {
    vi.mocked(isTauri).mockReturnValue(false);
    render(<AppVersion />);
    await waitFor(() => expect(screen.getByRole("status").textContent).toBe(`v${version}`));
    expect(getVersion).not.toHaveBeenCalled();
  });

  it("does not substitute a possibly incorrect version when native IPC fails", async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    vi.mocked(getVersion).mockRejectedValue(new Error("IPC unavailable"));
    render(<AppVersion />);
    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("Unavailable"));
  });

  it("ignores native metadata that arrives after unmount", async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    let resolve!: (value: string) => void;
    vi.mocked(getVersion).mockReturnValue(new Promise((done) => { resolve = done; }));
    const view = render(<AppVersion />);
    expect(screen.getByRole("status").textContent).toBe("Loading…");
    view.unmount();
    resolve("1.2.3");
    await Promise.resolve();
    expect(screen.queryByRole("status")).toBeNull();
  });
});
