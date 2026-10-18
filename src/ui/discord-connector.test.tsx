// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { DiscordStatus } from "../connectors";

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), open: vi.fn(), flash: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: mocks.open }));
vi.mock("../store", () => ({
  useStore: (select: (s: unknown) => unknown) => select({ settings: { projects: ["D:\\work\\app"] }, models: [{ id: "anthropic/claude-sonnet", name: "Claude Sonnet", provider: "anthropic", enabled: true }] }),
  flash: mocks.flash,
}));
import { DiscordConnector, discordSummary } from "./DiscordConnector";
import { splitIds } from "../connectors";

const ME = "100000000000000001";
const base = (over: Partial<DiscordStatus> = {}): DiscordStatus => ({
  enabled: false, has_token: false, allowed_users: [], allowed_guilds: [], allowed_channels: [], allow_dms: true, require_mention: true,
  project: "", model: "", perm: "auto", state: "off", error: "", bot_name: "", bot_id: "", invite_url: "", servers: 0, chats: 0, last_ignored: "", ...over,
});

let status: DiscordStatus;
beforeEach(() => {
  vi.clearAllMocks();
  status = base();
  mocks.invoke.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
    if (cmd === "discord_status") return status;
    if (cmd === "discord_configure") { status = { ...status, ...(args.cfg as object) }; return status; }
    if (cmd === "discord_set_token") { status = { ...status, has_token: !!args.token }; return status; }
    throw new Error(`unexpected ${cmd}`);
  });
});
afterEach(cleanup);

const expand = async () => { fireEvent.click(await screen.findByRole("button", { name: "Discord details" })); };

it("starts collapsed, with the state in the header, and expands on click", async () => {
  status = base({ enabled: true, has_token: true, state: "error", error: "Discord rejected the token." });
  render(<DiscordConnector />);
  const toggle = await screen.findByRole("button", { name: "Discord details" });
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByLabelText("Discord bot token")).toBeNull();
  expect(await screen.findByText("Error")).toBeTruthy();
  expect(screen.getByText("Discord rejected the token.")).toBeTruthy();
  expect(screen.getByRole("switch", { name: "Discord bot" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(toggle);
  expect(toggle.getAttribute("aria-expanded")).toBe("true");
  expect(document.getElementById(toggle.getAttribute("aria-controls")!)?.hidden).toBe(false);
  expect(screen.getByLabelText("Discord bot token")).toBeTruthy();
  fireEvent.click(toggle);
  expect(screen.queryByLabelText("Discord bot token")).toBeNull();
});

it("never shows a saved token", async () => {
  status = base({ has_token: true });
  render(<DiscordConnector />);
  await expand();
  const token = await screen.findByLabelText("Discord bot token") as HTMLInputElement;
  expect(token.value).toBe("");
  expect(token.type).toBe("password");
  expect(token.placeholder).toContain("Token saved");
  expect(screen.getByText(/nobody gets in until you add someone/)).toBeTruthy();
});

it("saves the token through its own write-only command", async () => {
  render(<DiscordConnector />);
  await expand();
  const token = await screen.findByLabelText("Discord bot token");
  fireEvent.change(token, { target: { value: "abc.def.ghi" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save token" })); });
  expect(mocks.invoke).toHaveBeenCalledWith("discord_set_token", { token: "abc.def.ghi" });
  expect(mocks.invoke.mock.calls.some(([cmd, a]) => cmd === "discord_configure" && JSON.stringify(a).includes("abc.def"))).toBe(false);
  await waitFor(() => expect((screen.getByLabelText("Discord bot token") as HTMLInputElement).value).toBe(""));
});

it("adds pasted user ids and keeps the rest of the config", async () => {
  status = base({ allowed_guilds: ["200000000000000001"], perm: "allowlist" });
  render(<DiscordConnector />);
  await expand();
  const box = await screen.findByLabelText(/Allowed users/);
  fireEvent.change(box, { target: { value: `${ME}, 100000000000000002` } });
  await act(async () => { fireEvent.keyDown(box, { key: "Enter" }); });
  const call = mocks.invoke.mock.calls.find(([cmd]) => cmd === "discord_configure");
  expect(call?.[1]).toEqual({ cfg: expect.objectContaining({ allowed_users: [ME, "100000000000000002"], allowed_guilds: ["200000000000000001"], perm: "allowlist" }) });
  expect(await screen.findByRole("button", { name: `Remove Allowed users ${ME}` })).toBeTruthy();
});

it("shows the backend's refusal instead of pretending it saved", async () => {
  mocks.invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "discord_status") return status;
    throw new Error("\"alice\" isn't a Discord user id.");
  });
  render(<DiscordConnector />);
  await expand();
  const box = await screen.findByLabelText(/Allowed users/);
  fireEvent.change(box, { target: { value: "alice" } });
  await act(async () => { fireEvent.keyDown(box, { key: "Enter" }); });
  expect((await screen.findByRole("alert")).textContent).toContain("isn't a Discord user id");
  expect((box as HTMLInputElement).value).toBe("alice");
});

it("warns about Full Access and summarizes the connection", async () => {
  status = base({ enabled: true, has_token: true, allowed_users: [ME], perm: "turbo", state: "connected", bot_name: "leash", servers: 2, chats: 1, invite_url: "https://discord.com/oauth2/authorize?client_id=1" });
  render(<DiscordConnector />);
  expect(await screen.findByText("Connected as @leash · 2 servers · 1 chat")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Discord details" }));
  expect(screen.getByRole("note").textContent).toContain("Full Access runs anything");
  expect(screen.getByRole("button", { name: "Invite to a server" })).toBeTruthy();
});

it("summaries and id splitting", () => {
  expect(discordSummary(null)).toBe("Checking…");
  expect(discordSummary(base())).toBe("Not set up");
  expect(discordSummary(base({ enabled: true, state: "incomplete", error: "Add the bot token." }))).toBe("Add the bot token.");
  expect(splitIds(" 1, 2\n3  4 ")).toEqual(["1", "2", "3", "4"]);
});
