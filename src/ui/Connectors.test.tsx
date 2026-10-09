// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { McpServerCfg } from "../api";
const mocks = vi.hoisted(() => ({ settings: { mcp: [] as McpServerCfg[] }, save: vi.fn(), set: vi.fn(), status: vi.fn(), reconnect: vi.fn(), boot: vi.fn(), start: vi.fn(), wait: vi.fn(), cancel: vi.fn(), disconnect: vi.fn(), open: vi.fn() }));
vi.mock("../store", () => ({ useStore: (select: (s: { settings: typeof mocks.settings }) => unknown) => select({ settings: mocks.settings }), saveSettings: mocks.save, set: mocks.set, flash: vi.fn() }));
vi.mock("../api", () => ({ api: { mcpStatus: mocks.status, mcpReconnect: mocks.reconnect, boot: mocks.boot, mcpOauthStart: mocks.start, mcpOauthWait: mocks.wait, mcpOauthCancel: mocks.cancel, mcpOauthDisconnect: mocks.disconnect } }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: mocks.open }));
import { AppConnectors } from "./Connectors";
beforeEach(() => { vi.clearAllMocks(); mocks.settings.mcp = []; mocks.save.mockResolvedValue(mocks.settings); mocks.status.mockResolvedValue([]); mocks.reconnect.mockResolvedValue(undefined); mocks.boot.mockResolvedValue({ settings: mocks.settings }); mocks.start.mockResolvedValue({ flow_id: "flow", authorization_url: "https://auth.example.test/authorize" }); mocks.wait.mockResolvedValue(undefined); mocks.cancel.mockResolvedValue(undefined); mocks.disconnect.mockResolvedValue(undefined); mocks.open.mockResolvedValue(undefined); });
afterEach(cleanup);
it("keeps credential configuration collapsed until Details is opened", () => {
  render(<AppConnectors />);
  const details = screen.getByRole("button", { name: "Linear details" });
  expect(details.getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByLabelText("Linear API key")).toBeNull();
  fireEvent.click(details);
  expect(details.getAttribute("aria-expanded")).toBe("true");
  expect(document.getElementById(details.getAttribute("aria-controls")!)).not.toBeNull();
  expect(screen.getByLabelText("Linear API key")).toBeTruthy();
  fireEvent.click(details);
  expect(screen.queryByLabelText("Linear API key")).toBeNull();
});
it("requires registered client IDs for Slack and Google and offers real Microsoft CLI", () => {
  render(<AppConnectors />);
  for (const app of ["Slack", "Google Workspace"]) {
    fireEvent.click(screen.getByRole("button", { name: `${app} details` }));
    expect(screen.getByLabelText(`${app} OAuth client ID`)).toBeTruthy();
    expect((screen.getByRole("button", { name: `Sign in to ${app}` }) as HTMLButtonElement).disabled).toBe(true);
  }
  fireEvent.click(screen.getByRole("button", { name: "Microsoft 365 details" }));
  expect(screen.getByRole("button", { name: "Install and connect Microsoft 365" })).toBeTruthy();
});
it("connects Linear without replacing other servers or pre-approving tools", async () => {
  const other: McpServerCfg = { name: "linear", command: "other", args: [], env: {}, enabled: true };
  mocks.settings.mcp = [other];
  render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Linear details" }));
  fireEvent.change(screen.getByLabelText("Linear API key"), { target: { value: "dummy-key" } });
  fireEvent.click(screen.getByRole("button", { name: "Connect Linear" }));
  await waitFor(() => expect(mocks.reconnect).toHaveBeenCalled());
  expect(mocks.save).toHaveBeenCalledWith({ mcp: [other, expect.objectContaining({ name: "linear-2", url: "https://mcp.linear.app/mcp", oauth: false, headers: { Authorization: "Bearer dummy-key" }, auto_approve: [] })] });
});
it("uses Notion's documented official stdio token configuration", async () => {
  render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Notion details" }));
  fireEvent.change(screen.getByLabelText("Notion integration token"), { target: { value: "dummy-notion-token" } });
  fireEvent.click(screen.getByRole("button", { name: "Connect Notion" }));
  await waitFor(() => expect(mocks.save).toHaveBeenCalledWith({ mcp: [expect.objectContaining({ command: "npx", args: ["-y", "@notionhq/notion-mcp-server"], env: { NOTION_TOKEN: "dummy-notion-token" }, auto_approve: [] })] }));
});
it("reports backend connection state and removes only the selected connector", async () => {
  const cfg: McpServerCfg = { name: "work", command: "", args: [], env: {}, enabled: true, transport: "http", url: "https://mcp.linear.app/mcp", headers: { Authorization: "Bearer dummy" } };
  const other = { ...cfg, name: "other", url: "https://example.test/mcp" };
  mocks.settings.mcp = [cfg, other];
  mocks.status.mockResolvedValue([{ name: "work", status: "connected", tools: 3, error: null }]);
  render(<AppConnectors />);
  await screen.findByText("work · Connected · 3 tools");
  fireEvent.click(screen.getByRole("button", { name: "Linear details" }));
  fireEvent.click(screen.getByRole("button", { name: "Disconnect Linear" }));
  await waitFor(() => expect(mocks.save).toHaveBeenCalledWith({ mcp: [other] }));
});
it("saves disabled OAuth configuration, opens authorization and refreshes backend settings", async () => {
  const fresh = { mcp: [{ name: "notion", enabled: true }] };
  mocks.boot.mockResolvedValue({ settings: fresh });
  render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Notion details" }));
  fireEvent.click(screen.getByRole("button", { name: "Sign in to Notion" }));
  await waitFor(() => expect(mocks.set).toHaveBeenCalledWith({ settings: fresh }));
  expect(mocks.save).toHaveBeenCalledWith({ mcp: [expect.objectContaining({ url: "https://mcp.notion.com/mcp", enabled: false, oauth: true, auto_approve: [] })] });
  expect(mocks.open).toHaveBeenCalledWith("https://auth.example.test/authorize");
  expect(mocks.wait).toHaveBeenCalledWith("flow");
  expect(mocks.save).toHaveBeenCalledTimes(1);
});
it("cancels pending sign-in on request and unmount", async () => {
  mocks.wait.mockImplementation(() => new Promise(() => {}));
  const view = render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Linear details" }));
  fireEvent.click(screen.getByRole("button", { name: "Sign in to Linear" }));
  await waitFor(() => expect(mocks.wait).toHaveBeenCalled());
  fireEvent.click(screen.getByRole("button", { name: "Cancel sign-in" }));
  await waitFor(() => expect(mocks.cancel).toHaveBeenCalledWith("flow"));
  fireEvent.click(screen.getByRole("button", { name: "Sign in to Linear" }));
  await waitFor(() => expect(mocks.wait).toHaveBeenCalledTimes(2));
  view.unmount();
  expect(mocks.cancel).toHaveBeenCalledTimes(2);
});
it("shows browser errors and allows retry", async () => {
  mocks.open.mockRejectedValueOnce(new Error("browser unavailable"));
  render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Notion details" }));
  fireEvent.click(screen.getByRole("button", { name: "Sign in to Notion" }));
  await screen.findByText(/Sign-in failed:.*browser unavailable/);
  expect(mocks.cancel).toHaveBeenCalledWith("flow");
  fireEvent.click(screen.getByRole("button", { name: "Sign in to Notion" }));
  await waitFor(() => expect(mocks.wait).toHaveBeenCalled());
});
it("disconnects OAuth through backend token deletion without stale settings save", async () => {
  mocks.settings.mcp = [{ name: "notion", command: "", args: [], env: {}, enabled: true, transport: "http", url: "https://mcp.notion.com/mcp", oauth: true }];
  render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Notion details" }));
  fireEvent.click(screen.getByRole("button", { name: "Disconnect Notion" }));
  await waitFor(() => expect(mocks.boot).toHaveBeenCalled());
  expect(mocks.disconnect).toHaveBeenCalledWith("notion");
  expect(mocks.save).not.toHaveBeenCalled();
});
it("requests explicit public read-only Slack scopes and fixed registered callback", async () => {
  render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Slack details" }));
  fireEvent.change(screen.getByLabelText("Slack OAuth client ID"), { target: { value: "dummy-slack-client" } });
  fireEvent.click(screen.getByRole("button", { name: "Sign in to Slack" }));
  await waitFor(() => expect(mocks.start).toHaveBeenCalledWith("slack", "dummy-slack-client", ["search:read.public", "channels:history", "channels:read", "users:read"], 42817));
});
it("selects verified Workspace endpoints and minimum read-only scopes", async () => {
  render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Google Workspace details" }));
  fireEvent.change(screen.getByLabelText("Workspace service"), { target: { value: "Docs" } });
  fireEvent.change(screen.getByLabelText("Google Workspace OAuth client ID"), { target: { value: "dummy-public-client" } });
  fireEvent.click(screen.getByRole("button", { name: "Sign in to Google Workspace" }));
  await waitFor(() => expect(mocks.start).toHaveBeenCalledWith("google-workspace", "dummy-public-client", ["https://www.googleapis.com/auth/drive.readonly", "https://www.googleapis.com/auth/documents.readonly"], undefined));
  expect(mocks.save).toHaveBeenCalledWith({ mcp: [expect.objectContaining({ url: "https://docsmcp.googleapis.com/mcp/v1", enabled: false, oauth: true })] });
});
it("disables an existing enabled OAuth connector before reconnecting", async () => {
  const cfg: McpServerCfg = { name: "notion", command: "", args: [], env: {}, enabled: true, transport: "http", url: "https://mcp.notion.com/mcp", oauth: true };
  mocks.settings.mcp = [cfg];
  render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Notion details" }));
  fireEvent.click(screen.getByRole("button", { name: "Reconnect Notion" }));
  await waitFor(() => expect(mocks.wait).toHaveBeenCalled());
  expect(mocks.save).toHaveBeenCalledWith({ mcp: [{ ...cfg, enabled: false }] });
});
it("does not reconnect when saving fails", async () => {
  mocks.save.mockResolvedValue(null);
  render(<AppConnectors />);
  fireEvent.click(screen.getByRole("button", { name: "Linear details" }));
  fireEvent.change(screen.getByLabelText("Linear API key"), { target: { value: "dummy" } });
  fireEvent.click(screen.getByRole("button", { name: "Connect Linear" }));
  await waitFor(() => expect(mocks.save).toHaveBeenCalled());
  expect(mocks.reconnect).not.toHaveBeenCalled();
});
