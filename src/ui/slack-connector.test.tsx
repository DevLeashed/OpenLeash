// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { SlackStatus } from "../connectors";

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), open: vi.fn(), flash: vi.fn(), copy: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: mocks.open }));
vi.mock("./clipboard", () => ({ copyText: mocks.copy }));
vi.mock("../store", () => ({
  useStore: (select: (s: unknown) => unknown) => select({ settings: { projects: ["D:\\work\\app"] }, models: [{ id: "anthropic/claude-sonnet", name: "Claude Sonnet", provider: "anthropic", enabled: true }] }),
  flash: mocks.flash,
}));
import { SLACK_MANIFEST, SlackConnector, slackSummary } from "./SlackConnector";

const ME = "U0ME00001";
const APP = "dummy-app-level-token";
const BOT = "dummy-bot-token";
const base = (over: Partial<SlackStatus> = {}): SlackStatus => ({
  enabled: false, has_app_token: false, has_bot_token: false, allowed_users: [], allowed_teams: [], allowed_channels: [], allow_dms: true, require_mention: true,
  project: "", model: "", perm: "auto", state: "off", error: "", bot_name: "", bot_id: "", team_name: "", chats: 0, last_ignored: "", ...over,
});

let status: SlackStatus;
beforeEach(() => {
  vi.clearAllMocks();
  status = base();
  mocks.invoke.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
    if (cmd === "slack_status") return status;
    if (cmd === "slack_configure") { status = { ...status, ...(args.cfg as object) }; return status; }
    if (cmd === "slack_set_token") { status = { ...status, [args.kind === "app" ? "has_app_token" : "has_bot_token"]: !!args.token }; return status; }
    if (cmd === "slack_test") return "Tokens work: @openleash in Acme.";
    if (cmd === "slack_forget_chats") { status = { ...status, chats: 0 }; return status; }
    throw new Error(`unexpected ${cmd}`);
  });
});
afterEach(cleanup);

const expand = async () => { fireEvent.click(await screen.findByRole("button", { name: "Slack details" })); };

it("starts collapsed with the state in the header and expands on click", async () => {
  status = base({ enabled: true, has_app_token: true, has_bot_token: true, allowed_users: [ME], state: "incomplete", error: "Add the bot token (xoxb-…)." });
  render(<SlackConnector />);
  const toggle = await screen.findByRole("button", { name: "Slack details" });
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByLabelText("Slack bot token")).toBeNull();
  expect(await screen.findByText("Setup needed")).toBeTruthy();
  fireEvent.click(toggle);
  expect(toggle.getAttribute("aria-expanded")).toBe("true");
  expect(screen.getByLabelText("Slack bot token")).toBeTruthy();
});

it("opens setup for an unconfigured app and never shows a saved token", async () => {
  status = base({ has_app_token: true, has_bot_token: true });
  render(<SlackConnector />);
  await expand();
  for (const label of ["Slack app-level token", "Slack bot token"]) {
    const box = await screen.findByLabelText(label) as HTMLInputElement;
    expect(box.value).toBe("");
    expect(box.type).toBe("password");
    expect(box.placeholder).toContain("Token saved");
  }
  expect(screen.getByText(/nobody gets in until you add someone/)).toBeTruthy();
  expect(document.body.textContent).not.toMatch(/xox[bp]-\d|xapp-\d/);
});

it("saves each token through the write-only command and clears its box", async () => {
  render(<SlackConnector />);
  await expand();
  const app = await screen.findByLabelText("Slack app-level token") as HTMLInputElement;
  const bot = screen.getByLabelText("Slack bot token") as HTMLInputElement;
  fireEvent.change(app, { target: { value: ` ${APP} ` } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save Slack app-level token" })); });
  expect(mocks.invoke).toHaveBeenCalledWith("slack_set_token", { kind: "app", token: APP });
  await waitFor(() => expect(app.value).toBe(""));
  fireEvent.change(bot, { target: { value: BOT } });
  await act(async () => { fireEvent.keyDown(bot, { key: "Enter" }); });
  expect(mocks.invoke).toHaveBeenCalledWith("slack_set_token", { kind: "bot", token: BOT });
  await waitFor(() => expect(bot.value).toBe(""));
  expect(mocks.invoke.mock.calls.some(([cmd, a]) => cmd === "slack_configure" && /xapp-|xoxb-/.test(JSON.stringify(a)))).toBe(false);
  // Removing sends an empty token for that kind only.
  const remove = await screen.findByRole("button", { name: "Remove Slack bot token" });
  await act(async () => { fireEvent.click(remove); });
  expect(mocks.invoke).toHaveBeenCalledWith("slack_set_token", { kind: "bot", token: "" });
});

it("adds pasted member ids and keeps the rest of the config", async () => {
  status = base({ allowed_teams: ["T0TEAM001"], perm: "allowlist", require_mention: false });
  render(<SlackConnector />);
  await expand();
  const box = await screen.findByLabelText(/Allowed users/);
  fireEvent.change(box, { target: { value: `${ME}, U0OTHER01` } });
  await act(async () => { fireEvent.keyDown(box, { key: "Enter" }); });
  const call = mocks.invoke.mock.calls.find(([cmd]) => cmd === "slack_configure");
  expect(call?.[1]).toEqual({ cfg: { enabled: false, allowed_users: [ME, "U0OTHER01"], allowed_teams: ["T0TEAM001"], allowed_channels: [], allow_dms: true, require_mention: false, project: "", model: "", perm: "allowlist" } });
  expect(await screen.findByRole("button", { name: `Remove Allowed users ${ME}` })).toBeTruthy();
  await act(async () => { fireEvent.click(screen.getByRole("switch", { name: "Allow direct messages" })); });
  expect(mocks.invoke).toHaveBeenLastCalledWith("slack_configure", { cfg: expect.objectContaining({ allow_dms: false, allowed_users: [ME, "U0OTHER01"] }) });
});

it("shows the backend's refusal instead of pretending it saved", async () => {
  mocks.invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "slack_status") return status;
    throw new Error("\"alice\" isn't a Slack member id.");
  });
  render(<SlackConnector />);
  await expand();
  const box = await screen.findByLabelText(/Allowed users/);
  fireEvent.change(box, { target: { value: "alice" } });
  await act(async () => { fireEvent.keyDown(box, { key: "Enter" }); });
  expect((await screen.findByRole("alert")).textContent).toContain("isn't a Slack member id");
  expect((box as HTMLInputElement).value).toBe("alice");
});

it("tests the tokens and shows the answer or the error", async () => {
  status = base({ has_app_token: true, has_bot_token: true });
  render(<SlackConnector />);
  await expand();
  const testBtn = await screen.findByRole("button", { name: "Test tokens" });
  await act(async () => { fireEvent.click(testBtn); });
  expect(mocks.invoke.mock.calls.some(([cmd]) => cmd === "slack_test")).toBe(true);
  expect(await screen.findByText("Tokens work: @openleash in Acme.")).toBeTruthy();
  mocks.invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "slack_status") return status;
    throw new Error("Slack rejected the bot token (invalid_auth).");
  });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Test tokens" })); });
  expect((await screen.findByRole("alert")).textContent).toContain("invalid_auth");
});

it("can't test without both tokens", async () => {
  status = base({ has_bot_token: true });
  render(<SlackConnector />);
  await expand();
  expect((await screen.findByRole("button", { name: "Test tokens" }) as HTMLButtonElement).disabled).toBe(true);
});

it("copies the manifest and opens the Slack apps page", async () => {
  render(<SlackConnector />);
  await expand();
  fireEvent.click(await screen.findByRole("button", { name: "Copy manifest" }));
  expect(mocks.copy).toHaveBeenCalledWith(SLACK_MANIFEST, expect.anything());
  mocks.open.mockResolvedValue(undefined);
  fireEvent.click(screen.getByRole("button", { name: "Open Slack apps" }));
  expect(mocks.open).toHaveBeenCalledWith("https://api.slack.com/apps");
  expect(screen.getByLabelText("Slack app manifest").textContent).toContain("socket_mode_enabled");
});

it("the manifest asks for Socket Mode and only the scopes the connector uses", () => {
  const m = JSON.parse(SLACK_MANIFEST);
  expect(m.settings.socket_mode_enabled).toBe(true);
  expect(m.settings.token_rotation_enabled).toBe(false);
  expect(m.settings.org_deploy_enabled).toBe(false);
  expect(m.features.app_home).toEqual({ messages_tab_enabled: true, messages_tab_read_only_enabled: false });
  expect(m.features.bot_user.always_online).toBe(true);
  expect([...m.oauth_config.scopes.bot].sort()).toEqual(["app_mentions:read", "channels:history", "channels:read", "chat:write", "groups:history", "groups:read", "im:history", "im:read", "mpim:history", "users:read"]);
  expect(m.oauth_config.scopes.user).toBeUndefined();
  expect(m.settings.event_subscriptions.bot_events).toEqual(["app_mention", "message.im", "message.channels", "message.groups", "message.mpim"]);
  expect(m.features.slash_commands).toBeUndefined();
});

it("warns about Full Access, documents the ! commands and summarizes the connection", async () => {
  status = base({ enabled: true, has_app_token: true, has_bot_token: true, allowed_users: [ME], perm: "turbo", state: "connected", bot_name: "openleash", team_name: "Acme", chats: 2, last_ignored: "Ignored a message from user U0X in channel C0Y: the author isn't in your allowed users." });
  render(<SlackConnector />);
  expect(await screen.findByText("Connected as @openleash in Acme · 2 chats")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Slack details" }));
  expect(screen.getByRole("note").textContent).toContain("Full Access runs anything");
  expect(document.body.textContent).toContain("!new");
  expect(screen.getByText(/Ignored a message from user U0X/)).toBeTruthy();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Start all fresh" })); });
  expect(mocks.invoke.mock.calls.some(([cmd]) => cmd === "slack_forget_chats")).toBe(true);
});

it("summaries", () => {
  expect(slackSummary(null)).toBe("Checking…");
  expect(slackSummary(base())).toBe("Not set up");
  expect(slackSummary(base({ has_bot_token: true }))).toBe("Off");
  expect(slackSummary(base({ enabled: true, state: "incomplete", error: "Add the app-level token (xapp-…)." }))).toBe("Add the app-level token (xapp-…).");
  expect(slackSummary(base({ enabled: true, state: "connecting" }))).toBe("Connecting…");
  expect(slackSummary(base({ enabled: true, state: "reconnecting", error: "Slack closed the connection." }))).toBe("Reconnecting… Slack closed the connection.");
  expect(slackSummary(base({ enabled: true, state: "error", error: "" }))).toBe("Stopped with an error");
  expect(slackSummary(base({ enabled: true, state: "connected", bot_name: "ol", chats: 1 }))).toBe("Connected as @ol · 1 chat");
});
