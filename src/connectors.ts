// Chat-platform connectors (Settings → Connectors → Chat with your agent).
// Typed IPC for src-tauri/src/connectors/. The bot token is write-only: it is
// sent with `setToken` and never comes back, only `has_token` (Slack:
// `has_app_token` / `has_bot_token`).
import { invoke } from "@tauri-apps/api/core";

export type DiscordState = "off" | "incomplete" | "connecting" | "connected" | "reconnecting" | "error";

export interface DiscordStatus {
  enabled: boolean;
  has_token: boolean;
  allowed_users: string[];
  allowed_guilds: string[];
  allowed_channels: string[];
  allow_dms: boolean;
  require_mention: boolean;
  /** Folder new Discord chats work in. Empty: the app's current project. */
  project: string;
  /** Model for new Discord chats. Empty: the most recently used model. */
  model: string;
  perm: string;
  state: DiscordState;
  error: string;
  bot_name: string;
  bot_id: string;
  invite_url: string;
  servers: number;
  chats: number;
  /** Ids and a reason only, never message text. */
  last_ignored: string;
}

/** Everything `discord_configure` accepts: the status minus the token and the live fields. */
export type DiscordPatch = Pick<DiscordStatus, "enabled" | "allowed_users" | "allowed_guilds" | "allowed_channels" | "allow_dms" | "require_mention" | "project" | "model" | "perm">;

export const patchOf = (s: DiscordStatus): DiscordPatch => ({
  enabled: s.enabled,
  allowed_users: s.allowed_users,
  allowed_guilds: s.allowed_guilds,
  allowed_channels: s.allowed_channels,
  allow_dms: s.allow_dms,
  require_mention: s.require_mention,
  project: s.project,
  model: s.model,
  perm: s.perm,
});

/** Split pasted text into candidate ids: commas, spaces and new lines all separate. */
export const splitIds = (raw: string): string[] => raw.split(/[\s,]+/).map((x) => x.trim()).filter(Boolean);

// ---- Telegram (src-tauri/src/connectors/telegram.rs) ----

export type ConnectorState = DiscordState;

export interface TelegramStatus {
  enabled: boolean;
  has_token: boolean;
  /** Numeric Telegram user ids. */
  allowed_users: string[];
  /** Group chat ids (negative numbers). Empty: any group, when groups are allowed. */
  allowed_chats: string[];
  allow_dms: boolean;
  allow_groups: boolean;
  require_mention: boolean;
  project: string;
  model: string;
  perm: string;
  state: ConnectorState;
  error: string;
  bot_name: string;
  bot_id: string;
  chats: number;
  /** Ids and a reason only, never message text. */
  last_ignored: string;
}

export type TelegramPatch = Pick<TelegramStatus, "enabled" | "allowed_users" | "allowed_chats" | "allow_dms" | "allow_groups" | "require_mention" | "project" | "model" | "perm">;

export const telegramPatchOf = (s: TelegramStatus): TelegramPatch => ({
  enabled: s.enabled,
  allowed_users: s.allowed_users,
  allowed_chats: s.allowed_chats,
  allow_dms: s.allow_dms,
  allow_groups: s.allow_groups,
  require_mention: s.require_mention,
  project: s.project,
  model: s.model,
  perm: s.perm,
});

// ---- Slack (src-tauri/src/connectors/slack.rs) ----

export interface SlackStatus {
  enabled: boolean;
  has_app_token: boolean;
  has_bot_token: boolean;
  /** Slack member ids (U…/W…). */
  allowed_users: string[];
  /** Workspace ids (T…). Empty: any workspace the app is in. */
  allowed_teams: string[];
  /** Channel ids (C…/G…). Empty: any channel the app is in. */
  allowed_channels: string[];
  allow_dms: boolean;
  require_mention: boolean;
  project: string;
  model: string;
  perm: string;
  state: ConnectorState;
  error: string;
  bot_name: string;
  bot_id: string;
  team_name: string;
  chats: number;
  /** Ids and a reason only, never message text. */
  last_ignored: string;
}

export type SlackPatch = Pick<SlackStatus, "enabled" | "allowed_users" | "allowed_teams" | "allowed_channels" | "allow_dms" | "require_mention" | "project" | "model" | "perm">;

export const slackPatchOf = (s: SlackStatus): SlackPatch => ({
  enabled: s.enabled,
  allowed_users: s.allowed_users,
  allowed_teams: s.allowed_teams,
  allowed_channels: s.allowed_channels,
  allow_dms: s.allow_dms,
  require_mention: s.require_mention,
  project: s.project,
  model: s.model,
  perm: s.perm,
});

/** Which Slack token `slackSetToken` sets: the app-level `xapp-…` or the bot `xoxb-…`. */
export type SlackTokenKind = "app" | "bot";

export const connectorsApi = {
  discordStatus: () => invoke<DiscordStatus>("discord_status"),
  discordConfigure: (cfg: DiscordPatch) => invoke<DiscordStatus>("discord_configure", { cfg }),
  discordSetToken: (token: string) => invoke<DiscordStatus>("discord_set_token", { token }),
  discordForgetChats: () => invoke<DiscordStatus>("discord_forget_chats"),
  telegramStatus: () => invoke<TelegramStatus>("telegram_status"),
  telegramConfigure: (cfg: TelegramPatch) => invoke<TelegramStatus>("telegram_configure", { cfg }),
  telegramSetToken: (token: string) => invoke<TelegramStatus>("telegram_set_token", { token }),
  /** Checks the saved token with Telegram; resolves to a short human line. */
  telegramTest: () => invoke<string>("telegram_test"),
  telegramForgetChats: () => invoke<TelegramStatus>("telegram_forget_chats"),
  slackStatus: () => invoke<SlackStatus>("slack_status"),
  slackConfigure: (cfg: SlackPatch) => invoke<SlackStatus>("slack_configure", { cfg }),
  slackSetToken: (kind: SlackTokenKind, token: string) => invoke<SlackStatus>("slack_set_token", { kind, token }),
  /** Checks the saved tokens with Slack; resolves to a short human line. */
  slackTest: () => invoke<string>("slack_test"),
  slackForgetChats: () => invoke<SlackStatus>("slack_forget_chats"),
};
