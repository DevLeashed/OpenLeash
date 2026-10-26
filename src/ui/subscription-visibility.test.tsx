// @vitest-environment jsdom
// Claude subscriptions are never offered or shown in the UI. The backend can
// still import and route them, so these tests pin what the screens render, not
// what the store holds: a hidden account must not appear anywhere on the
// accounts or models screens, and a chat already on a hidden model must still
// count as usable, or sending would be blocked by a sign we chose to hide.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import { type AccountView, type ModelInfo, type ProviderView } from "../api";
import { get, set, shownAccounts, shownProviders } from "../store";
import { AccountsTab } from "./Accounts";
import { ModelsTab } from "./ModelManager";

const acct = (id: string, kind: string): AccountView => ({
  id, kind, label: `${kind} 1`, email: "", priority: 0, enabled: true, source: "cli",
  disabled_reason: "", usage: null, cooldown_until: 0, cooldown_reason: "", available: true, active: false, expires_at: 0,
}) as unknown as AccountView;

/** An account-backed provider, shaped the way the backend describes one. */
const subscription = (id: string, name: string): ProviderView => ({
  id, name, icon: id, mono: "C", color: "#10a37f", base_url: "", env: "", chip: "",
  kind: "anthropic", custom: false, insist: false, connected: false, has_key: false,
  key_hint: "", key_hints: [], key_pool: false, base_url_override: "", enabled: true, local: false,
  account: { display_name: name, short_name: name, login_file: "~/.x", login_command: "x login", paste_hint: "", setup_command: "", token_prefix: "", warning: "", terms_gate: null, key_login: false },
}) as unknown as ProviderView;

/** The Anthropic API-key provider: not a subscription, so it must stay visible. */
const apiKey: ProviderView = {
  id: "anthropic", name: "Anthropic", icon: "anthropic", mono: "A", color: "#e8967a", base_url: "https://api.anthropic.com", env: "ANTHROPIC_API_KEY", chip: "",
  kind: "anthropic", custom: false, insist: false, connected: true, has_key: true, key_hint: "…abcd", key_hints: [], key_pool: false,
  base_url_override: "", enabled: true, local: false, account: null,
} as unknown as ProviderView;

/** Kimi Code: a subscription whose credential is a pasted console key, with
 * no CLI login file behind it — the connect dialog is paste-only. */
const kimiCode: ProviderView = {
  id: "kimi-code", name: "Kimi Code", icon: "moonshot", mono: "Ki", color: "#60a5fa", base_url: "https://api.kimi.ai/coding/v1", env: "KIMI_API_KEY", chip: "",
  kind: "openai", custom: false, insist: false, connected: false, has_key: false,
  key_hint: "", key_hints: [], key_pool: false, base_url_override: "", enabled: true, local: false,
  account: { display_name: "Kimi Code", short_name: "Kimi", login_file: "", login_command: "create a key at kimi.com/code/console", paste_hint: "Paste a Kimi Code API key", setup_command: "", token_prefix: "", warning: "", terms_gate: null, key_login: true },
} as unknown as ProviderView;

describe("which providers and accounts the UI may show", () => {
  it("drops the Claude subscription and keeps the other providers, including the Anthropic API key", () => {
    const list = [subscription("claude", "Claude (subscription)"), subscription("codex", "ChatGPT (Codex)"), apiKey];
    expect(shownProviders(list).map((p) => p.id)).toEqual(["codex", "anthropic"]);
  });

  it("drops Claude accounts and keeps the rest", () => {
    expect(shownAccounts([acct("a1", "claude"), acct("a2", "codex")]).map((a) => a.id)).toEqual(["a2"]);
  });
});

// `MorphText` and the tooltips reach for APIs jsdom lacks; same shim the other
// screen tests use.
beforeEach(() => {
  (window as unknown as { matchMedia?: unknown }).matchMedia ??= () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  (Element.prototype as unknown as { getAnimations?: unknown }).getAnimations ??= () => [];
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  // The opt-in is read once at boot; a test that turns it on must not leak it
  // into the tests that assert the default hidden behaviour.
  set({ claudeAllowed: false });
});

// The opt-in is a file named `ALLOW_CLAUDE` beside the executable, decided by
// the backend at boot (`claude_allowed` in src-tauri/src/lib.rs) and carried in
// `Boot.claude_allowed`. A file — not a setting — so an unmarked install stays
// closed even when somebody else's settings are restored into it.
describe("opting into the Claude subscription", () => {
  const claudeSub = subscription("claude", "Claude (subscription)");

  it("keeps Claude off screen while the install has not opted in", () => {
    set({ claudeAllowed: false });
    expect(shownProviders([claudeSub, subscription("codex", "ChatGPT (Codex)")]).map((p) => p.id)).toEqual(["codex"]);
    expect(shownAccounts([acct("a1", "claude"), acct("a2", "codex")]).map((a) => a.id)).toEqual(["a2"]);
  });

  it("offers the Claude provider and its accounts once ALLOW_CLAUDE is present", () => {
    set({ claudeAllowed: true });
    expect(shownProviders([claudeSub, subscription("codex", "ChatGPT (Codex)")]).map((p) => p.id)).toEqual(["claude", "codex"]);
    expect(shownAccounts([acct("a1", "claude"), acct("a2", "codex")]).map((a) => a.id)).toEqual(["a1", "a2"]);
  });

  it("puts the Claude connect flow back on the accounts screen", () => {
    set({
      claudeAllowed: true,
      providers: [claudeSub],
      accounts: [acct("a1", "claude")],
      settingsTab: "accounts",
    });
    render(<AccountsTab />);
    const text = document.body.textContent ?? "";
    expect(text).toContain("Claude (subscription)");
    expect(text).toContain("1 of 1 available");
    expect(button$("Add")).toBeDefined();
  });
});

/** First button whose label includes `text`, the way the other screen tests look. */
function button$(text: string) {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.includes(text));
}

describe("the accounts screen", () => {
  it("does not mention Claude at all, even when Claude accounts were imported earlier", () => {
    set({
      providers: [subscription("claude", "Claude (subscription)"), subscription("codex", "ChatGPT (Codex)")],
      accounts: [acct("a1", "claude"), acct("a2", "codex")],
      settingsTab: "accounts",
    });
    render(<AccountsTab />);
    const text = document.body.textContent ?? "";
    expect(text.toLowerCase()).not.toContain("claude");
    expect(document.querySelectorAll(".acct")).toHaveLength(1);
    expect(text).toContain("ChatGPT (Codex)");
  });

  it("shows Antigravity's single CLI-managed login without an account import flow", () => {
    set({
      providers: [{ ...apiKey, id: "antigravity", name: "Antigravity", icon: "antigravity", local: true, connected: true }],
      accounts: [],
      settingsTab: "accounts",
    });
    render(<AccountsTab />);
    const text = document.body.textContent ?? "";
    expect(text).toContain("Google Antigravity (agy)");
    expect(text).toContain("Checking CLI login…");
    expect(button$("Add")).toBeUndefined();
  });

  it("offers Kimi Code like any other subscription, not a hidden one", () => {
    set({ providers: [kimiCode], accounts: [], settingsTab: "accounts" });
    render(<AccountsTab />);
    const text = document.body.textContent ?? "";
    expect(text).toContain("Kimi Code");
    expect(text).toContain("No accounts yet");
    expect(button$("Add")).toBeDefined();
  });

  it("connects Kimi Code by pasted key alone — there is no CLI login to import", () => {
    set({ providers: [kimiCode], accounts: [], settingsTab: "accounts" });
    const view = render(<AccountsTab />);
    fireEvent.click(view.getByRole("button", { name: "Add" }));
    const text = document.body.textContent ?? "";
    expect(text).toContain("Connect Kimi Code");
    expect(text).toContain("Paste your Kimi API key");
    expect(text).toContain("Connect key");
    // The local-import block must be gone, not merely disabled: its button
    // would try to read a login file that never exists for this kind.
    expect(text).not.toContain("Import");
  });
});

describe("the models screen", () => {
  it("does not list a Claude subscription, but a chat already on one stays ready", () => {
    const opus: ModelInfo = {
      id: "claude/claude-opus-5", name: "Claude Opus 5", provider: "claude", context: 200_000, output: 64_000, input_price: 0, output_price: 0,
      effort: false, input_types: ["text"], capabilities: [], reasoning_levels: [], reasoning_param: "none", custom: false, enabled: true,
    };
    set({
      providers: [subscription("claude", "Claude (subscription)"), apiKey],
      accounts: [acct("a1", "claude")],
      models: [opus],
      home: { ...get().home, model: opus.id },
    });
    render(<ModelsTab />);
    const text = document.body.textContent ?? "";
    expect(text).not.toContain("Claude (subscription)");
    // The default-model button names the current model, so readiness is still
    // judged on the real provider: no "Choose model" or "Add provider" fallback.
    expect(text).toContain("Claude Opus 5");
    expect(text).not.toContain("Choose model");
  });
});
