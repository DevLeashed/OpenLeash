// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("./Connectors", () => ({ AppConnectors: () => null }));
import { api, type Settings } from "../api";
import { get, set } from "../store";
import { PluginsTab } from "./Plugins";

beforeEach(() => {
  set({ settings: { mcp: [], allow: [{ pattern: "!danger *", project: "" }], perm: "ask" } as unknown as Settings });
  vi.spyOn(api, "pluginsStatus").mockResolvedValue({ computer: { ok: false, monitors: [] }, browser: { ok: false } } as never);
  vi.spyOn(api, "mcpStatus").mockResolvedValue([]);
  vi.spyOn(api, "settings").mockImplementation(async (s) => ({ ...get().settings!, ...s }));
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it.each(["stdio", "http"])("adds a user-confirmed %s custom plugin from Plugins without permission grants", async (transport) => {
  render(<PluginsTab />);
  expect(api.settings).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Add custom plugin" }));
  fireEvent.change(screen.getByLabelText("Server name"), { target: { value: "custom" } });
  if (transport === "http") {
    fireEvent.click(screen.getByRole("radio", { name: "URL" }));
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://example.test/mcp" } });
    fireEvent.change(screen.getByLabelText("Access token"), { target: { value: "dummy-token" } });
  } else fireEvent.change(screen.getByLabelText("Command and arguments"), { target: { value: 'node "path with spaces/server.js"' } });
  expect(api.settings).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Add server" }));
  await screen.findByRole("switch", { name: "Disable custom" });
  const cfg = get().settings!.mcp[0]!;
  expect(cfg).toMatchObject({ name: "custom", transport, enabled: true, auto_approve: [], oauth: false, env: {} });
  expect(get().settings!.allow).toEqual([{ pattern: "!danger *", project: "" }]);
  expect(get().settings!.perm).toBe("ask");
  if (transport === "http") expect(cfg.headers).toEqual({ Authorization: "Bearer dummy-token" });
  else expect([cfg.command, ...cfg.args]).toEqual(["node", "path with spaces/server.js"]);
  fireEvent.click(screen.getByRole("switch", { name: "Disable custom" }));
  await screen.findByRole("switch", { name: "Enable custom" });
  const reconnect = vi.spyOn(api, "mcpReconnect").mockResolvedValue(undefined);
  fireEvent.click(screen.getByRole("button", { name: "Reconnect" }));
  expect(reconnect).toHaveBeenCalledOnce();
  fireEvent.click(screen.getByRole("button", { name: "Remove custom" }));
  await screen.findByText("No custom plugins yet.");
});

it("preserves exact approval revocation and rejects wildcard grants in Plugins", async () => {
  set({ settings: { ...get().settings!, mcp: [{ name: "custom", command: "node", args: [], env: {}, enabled: false, auto_approve: ["read"] }], allow: [{ pattern: "mcp__custom__read *", project: "" }, { pattern: "!danger *", project: "" }] } });
  render(<PluginsTab />);
  fireEvent.click(screen.getByRole("button", { name: "Show tools to pre-approve for custom" }));
  fireEvent.change(screen.getByPlaceholderText("tool name, e.g. read_thing"), { target: { value: "*" } });
  fireEvent.click(screen.getByRole("button", { name: "Pre-approve" }));
  expect(screen.getByRole("alert").textContent).toContain("wildcard");
  expect(api.settings).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Remove read" }));
  await vi.waitFor(() => expect(get().settings!.mcp[0]!.auto_approve).toEqual([]));
  expect(get().settings!.allow).toEqual([{ pattern: "!danger *", project: "" }]);
});
