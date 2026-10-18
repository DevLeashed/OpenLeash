// @vitest-environment jsdom
// The service-plugin cards (Linear, Jira, Slack, Notion, GitLab) in Settings →
// Connectors. What has to hold:
//   - they ship off, and an old settings.json without their keys still renders;
//   - a token is write-only: it goes through `plugin_secret_set`, never through
//     a settings save, and the UI only ever shows whether one is saved;
//   - "Test connection" shows who the credentials authenticate as.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("./Connectors", () => ({ AppConnectors: () => <div>App connectors</div> }));
vi.mock("./DiscordConnector", () => ({ DiscordConnector: () => <div>Discord connector</div> }));

import { api, type Plugins, type PluginsStatus, type Settings } from "../api";
import { set } from "../store";
import { ConnectorsTab } from "./Plugins";
import { SERVICE_SPECS } from "./ServicePlugins";

const old: Plugins = {
  github: { enabled: false },
  computer: { enabled: false, settle_ms: 600 },
  browser: { enabled: false, width: 1280, height: 800, persistent_profile: false, headed: false, developer_mode: false, interactions: "ask", download_dir: "" },
};

const status = {
  has_token: false, can_login: false,
  github: { ok: false, source: null, error: "No token found" },
  computer: { ok: true, monitors: [] },
  browser: { ok: true, browser: "Chrome" },
  secrets: { slack: { token: true, user_token: false } },
} as unknown as PluginsStatus;

const store = (plugins: Plugins) =>
  set({ settings: { plugins, mcp: [], allow: [], settingsTab: "connectors" } as unknown as Settings, settingsTab: "connectors" });

beforeEach(() => vi.spyOn(api, "pluginsStatus").mockResolvedValue(status));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("service plugin cards", () => {
  it("lists the five services in backend order, all off for an old settings file", () => {
    store(old);
    render(<ConnectorsTab />);
    expect(SERVICE_SPECS.map((s) => s.id)).toEqual(["linear", "jira", "slack", "notion", "gitlab"]);
    for (const s of SERVICE_SPECS) {
      expect(screen.getByRole("switch", { name: `${s.name} plugin` }).getAttribute("aria-checked")).toBe("false");
    }
  });

  it("turning one on saves only the switch, never a token", async () => {
    store({ ...old, linear: { enabled: false } });
    const save = vi.spyOn(api, "settings").mockResolvedValue({} as unknown as Settings);
    render(<ConnectorsTab />);
    fireEvent.click(screen.getByRole("switch", { name: "Linear plugin" }));
    await vi.waitFor(() => expect(save).toHaveBeenCalledTimes(1));
    const sent = JSON.stringify(save.mock.calls[0]);
    expect(sent).toContain("\"linear\":{\"enabled\":true}");
    expect(sent).not.toContain("token");
  });

  it("saves a token through plugin_secret_set and shows only that one is saved", async () => {
    store(old);
    const setSecret = vi.spyOn(api, "pluginSecretSet").mockResolvedValue();
    const save = vi.spyOn(api, "settings");
    render(<ConnectorsTab />);
    fireEvent.click(screen.getByRole("button", { name: "Slack Details" }));
    const bot = await screen.findByLabelText("Slack bot token") as HTMLInputElement;
    await vi.waitFor(() => expect(bot.placeholder).toBe("Saved · paste to replace"));
    expect(bot.type).toBe("password");
    expect(bot.value).toBe("");
    const user = screen.getByLabelText("Slack user token") as HTMLInputElement;
    expect(user.placeholder).toContain("xoxp-");
    fireEvent.change(user, { target: { value: "  xoxp-dummy  " } });
    fireEvent.keyDown(user, { key: "Enter" });
    await vi.waitFor(() => expect(setSecret).toHaveBeenCalledWith("slack", "user_token", "xoxp-dummy"));
    expect(save).not.toHaveBeenCalled();
  });

  it("shows the identity from Test connection", async () => {
    store(old);
    vi.spyOn(api, "pluginServiceTest").mockResolvedValue({ ok: true, identity: "Ada · acme", source: "GITLAB_TOKEN" });
    render(<ConnectorsTab />);
    fireEvent.click(screen.getByRole("button", { name: "GitLab Details" }));
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    expect(await screen.findByText(/Connected · Ada · acme · from GITLAB_TOKEN/)).toBeTruthy();
  });

  it("saves a non-secret setting like the Jira site URL through settings", async () => {
    store(old);
    const save = vi.spyOn(api, "settings").mockResolvedValue({} as unknown as Settings);
    render(<ConnectorsTab />);
    fireEvent.click(screen.getByRole("button", { name: "Jira Details" }));
    const site = screen.getByLabelText("Jira site URL");
    fireEvent.change(site, { target: { value: "https://acme.atlassian.net " } });
    fireEvent.keyDown(site, { key: "Enter" });
    await vi.waitFor(() => expect(save).toHaveBeenCalledTimes(1));
    expect(JSON.stringify(save.mock.calls[0])).toContain("\"base_url\":\"https://acme.atlassian.net\"");
  });
});
