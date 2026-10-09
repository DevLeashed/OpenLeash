// @vitest-environment jsdom
// Claude subscriptions are never offered or shown in the UI. The backend can
// still import and route them, so these tests pin what the screens render, not
// what the store holds: a hidden account must not appear anywhere on the
// accounts or models screens, and a chat already on a hidden model must still
// count as usable, or sending would be blocked by a sign we chose to hide.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";

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

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

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
