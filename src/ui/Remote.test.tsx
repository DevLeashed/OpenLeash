// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("./primitives/MorphText", () => ({ MorphText: ({ children }: { children: import("react").ReactNode }) => <span>{children}</span> }));

import { RemoteTab } from "./Remote";

const status = {
  enabled: true, lan: false, port: 4411, running: true, error: "", urls: ["http://127.0.0.1:4411"],
  pair_url: "", qr_svg: "", sessions: [], clients: [], code: null,
};

beforeEach(() => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "remote_status") return status;
    if (command === "ssh_hosts_list") return [];
    return null;
  });
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: vi.fn(async () => {}) } });
});
afterEach(cleanup);

it("recommends Tailscale and opens a step-by-step private sharing tutorial", async () => {
  render(<RemoteTab />);
  fireEvent.click(await screen.findByRole("button", { name: /Tailscale tutorial/i }));
  expect(screen.getByRole("dialog", { name: "Tailscale tutorial" })).toBeTruthy();
  expect(screen.getByText("Connect both devices to Tailscale")).toBeTruthy();
  expect(screen.getByRole("link", { name: "Download Tailscale" }).getAttribute("href")).toBe("https://tailscale.com/download");

  fireEvent.click(screen.getByRole("button", { name: /Next/i }));
  expect(screen.getByText("Keep OpenLeash on this computer only")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Next/i }));
  expect(screen.getByText("tailscale serve --bg http://127.0.0.1:4411")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Copy command" }));
  expect(navigator.clipboard.writeText).toHaveBeenCalledWith("tailscale serve --bg http://127.0.0.1:4411");

  fireEvent.click(screen.getByRole("button", { name: /Next/i }));
  expect(screen.getByText("Open it on your other device")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Done" }));
  expect(screen.queryByRole("dialog")).toBeNull();
});
