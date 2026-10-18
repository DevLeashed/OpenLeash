// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { TelegramStatus } from "../connectors";

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), open: vi.fn(), flash: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: mocks.open }));
vi.mock("../store", () => ({
  useStore: (select: (s: unknown) => unknown) => select({ settings: { projects: ["D:\\work\\app"] }, models: [{ id: "anthropic/claude-sonnet", name: "Claude Sonnet", provider: "anthropic", enabled: true }] }),
  flash: mocks.flash,
}));
import { TelegramConnector, telegramSummary } from "./TelegramConnector";

const ME = "123456789";
const TOKEN = "7000000001:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsawQ";
const base = (over: Partial<TelegramStatus> = {}): TelegramStatus => ({
  enabled: false, has_token: false, allowed_users: [], allowed_chats: [], allow_dms: true, allow_groups: false, require_mention: true,
  project: "", model: "", perm: "auto", state: "off", error: "", bot_name: "", bot_id: "", chats: 0, last_ignored: "", ...over,
});

let status: TelegramStatus;
beforeEach(() => {
  vi.clearAllMocks();
  mocks.open.mockResolvedValue(undefined);
  status = base();
  mocks.invoke.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
    if (cmd === "telegram_status") return status;
    if (cmd === "telegram_configure") { status = { ...status, ...(args.cfg as object) }; return status; }
    if (cmd === "telegram_set_token") { status = { ...status, has_token: !!args.token }; return status; }
    if (cmd === "telegram_test") return "Token works: @LeashBot.";
    if (cmd === "telegram_forget_chats") { status = { ...status, chats: 0 }; return status; }
    throw new Error(`unexpected ${cmd}`);
  });
});
afterEach(cleanup);

/** The panel starts collapsed; open its details the way a person would. */
const renderOpen = async () => {
  render(<TelegramConnector />);
  fireEvent.click(await screen.findByRole("button", { name: "Telegram details" }));
};

it("starts collapsed, even before setup is finished", async () => {
  render(<TelegramConnector />);
  const details = await screen.findByRole("button", { name: "Telegram details" });
  await screen.findByText("Not set up", undefined, { timeout: 5000 });
  expect(details.getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByLabelText("Telegram bot token")).toBeNull();
  fireEvent.click(details);
  expect(details.getAttribute("aria-expanded")).toBe("true");
  expect(await screen.findByLabelText("Telegram bot token")).toBeTruthy();
});

it("shows an error in the collapsed header", async () => {
  status = base({ enabled: true, has_token: true, allowed_users: ["12345"], state: "error", error: "Telegram rejected the token." });
  render(<TelegramConnector />);
  expect(await screen.findByText("Error")).toBeTruthy();
  expect(screen.getByText("Telegram rejected the token.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Telegram details" }).getAttribute("aria-expanded")).toBe("false");
});

it("never shows a saved token", async () => {
  status = base({ has_token: true });
  await renderOpen();
  const token = await screen.findByLabelText("Telegram bot token") as HTMLInputElement;
  expect(token.value).toBe("");
  expect(token.type).toBe("password");
  expect(token.placeholder).toContain("Token saved");
  expect(screen.getByText(/nobody gets in until you add someone/)).toBeTruthy();
  expect(document.body.textContent).not.toContain("AAHdq");
  fireEvent.click(screen.getByRole("button", { name: "Open BotFather" }));
  expect(mocks.open).toHaveBeenCalledWith("https://t.me/BotFather");
});

it("saves the token through its own write-only command and clears the box", async () => {
  await renderOpen();
  const token = await screen.findByLabelText("Telegram bot token");
  fireEvent.change(token, { target: { value: TOKEN } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save token" })); });
  expect(mocks.invoke).toHaveBeenCalledWith("telegram_set_token", { token: TOKEN });
  expect(mocks.invoke.mock.calls.some(([cmd, a]) => cmd === "telegram_configure" && JSON.stringify(a).includes("AAHdq"))).toBe(false);
  await waitFor(() => expect((screen.getByLabelText("Telegram bot token") as HTMLInputElement).value).toBe(""));
  expect(mocks.flash).toHaveBeenCalledWith("Bot token saved");
  expect(document.body.textContent).not.toContain("AAHdq");
});

it("tests the saved token and shows the answer", async () => {
  status = base({ has_token: true });
  await renderOpen();
  const btn = await screen.findByRole("button", { name: "Test token" });
  await act(async () => { fireEvent.click(btn); });
  expect(mocks.invoke).toHaveBeenCalledWith("telegram_test");
  expect(await screen.findByText("Token works: @LeashBot.")).toBeTruthy();
});

it("shows a failed token test as an error", async () => {
  status = base({ has_token: true });
  mocks.invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "telegram_status") return status;
    throw new Error("Telegram rejected the bot token.");
  });
  await renderOpen();
  const btn = await screen.findByRole("button", { name: "Test token" });
  await act(async () => { fireEvent.click(btn); });
  expect((await screen.findByRole("alert")).textContent).toContain("rejected the bot token");
});

it("adds pasted user ids and keeps the rest of the config", async () => {
  status = base({ allowed_chats: ["-1001234567890"], allow_groups: true, perm: "allowlist" });
  await renderOpen();
  const box = await screen.findByLabelText(/Allowed users/);
  fireEvent.change(box, { target: { value: `${ME}, 987654321` } });
  await act(async () => { fireEvent.keyDown(box, { key: "Enter" }); });
  const call = mocks.invoke.mock.calls.find(([cmd]) => cmd === "telegram_configure");
  expect(call?.[1]).toEqual({ cfg: expect.objectContaining({ allowed_users: [ME, "987654321"], allowed_chats: ["-1001234567890"], allow_groups: true, perm: "allowlist" }) });
  expect(await screen.findByRole("button", { name: `Remove Allowed users ${ME}` })).toBeTruthy();
});

it("turns groups on, then offers the group list and the mention rule", async () => {
  status = base({ has_token: true, allowed_users: [ME], enabled: true, state: "connected", bot_name: "LeashBot" });
  render(<TelegramConnector />);
  fireEvent.click(await screen.findByRole("button", { name: "Telegram details" }));
  expect(screen.queryByLabelText(/Group chats/)).toBeNull();
  await act(async () => { fireEvent.click(screen.getByRole("switch", { name: "Allow groups" })); });
  const call = mocks.invoke.mock.calls.find(([cmd]) => cmd === "telegram_configure");
  expect(call?.[1]).toEqual({ cfg: expect.objectContaining({ allow_groups: true, allowed_users: [ME], enabled: true }) });
  expect(await screen.findByLabelText(/Group chats/)).toBeTruthy();
  expect(screen.getByRole("switch", { name: "Only when mentioned in groups" })).toBeTruthy();
});

it("shows the backend's refusal instead of pretending it saved", async () => {
  mocks.invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "telegram_status") return status;
    throw new Error("\"@alice\" is a username, and usernames can change hands.");
  });
  await renderOpen();
  const box = await screen.findByLabelText(/Allowed users/);
  fireEvent.change(box, { target: { value: "@alice" } });
  await act(async () => { fireEvent.keyDown(box, { key: "Enter" }); });
  expect((await screen.findByRole("alert")).textContent).toContain("is a username");
  expect((box as HTMLInputElement).value).toBe("@alice");
});

it("warns about Full Access and summarizes the connection", async () => {
  status = base({ enabled: true, has_token: true, allowed_users: [ME], perm: "turbo", state: "connected", bot_name: "LeashBot", chats: 2, last_ignored: "Ignored a message from user 5 in chat 5: the sender isn't in your allowed users." });
  render(<TelegramConnector />);
  expect(await screen.findByText("Connected as @LeashBot · 2 chats")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Telegram details" }));
  expect(screen.getByRole("note").textContent).toContain("Full Access runs anything");
  expect(screen.getByText(/Ignored a message from user 5/)).toBeTruthy();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Start all fresh" })); });
  expect(mocks.invoke).toHaveBeenCalledWith("telegram_forget_chats");
});

it("summaries", () => {
  expect(telegramSummary(null)).toBe("Checking…");
  expect(telegramSummary(base())).toBe("Not set up");
  expect(telegramSummary(base({ has_token: true }))).toBe("Off");
  expect(telegramSummary(base({ enabled: true, state: "incomplete", error: "Add the bot token." }))).toBe("Add the bot token.");
  expect(telegramSummary(base({ enabled: true, state: "connecting" }))).toBe("Connecting…");
  expect(telegramSummary(base({ enabled: true, state: "reconnecting", error: "Another program is polling this bot." }))).toBe("Reconnecting… Another program is polling this bot.");
  expect(telegramSummary(base({ enabled: true, state: "error", error: "Telegram rejected the bot token." }))).toBe("Telegram rejected the bot token.");
  expect(telegramSummary(base({ enabled: true, state: "connected", bot_name: "b", chats: 1 }))).toBe("Connected as @b · 1 chat");
});
