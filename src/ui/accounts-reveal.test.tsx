// @vitest-environment jsdom
// Account identifiers stay hidden until an explicit reveal: either the
// per-account context menu or the warning-gated toolbar action.
//
// `reviewer_privacy.rs` pins the address out of everything that is screenshotted
// or pasted (the serving label, the transcript, chat exports). A settings page
// that rendered `account.email` in the row by default would put it back into every
// screenshot of the accounts screen, so the default state is masked and the full
// address only exists after the user asks for it. A regression here is silent:
// the page still works, it just leaks on every glance.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import { type AccountView, type ProviderView, type Settings } from "../api";
import { set } from "../store";
import { AccountsTab } from "./Accounts";

const EMAIL = "someone@example.com";

const acct = (id: string): AccountView => ({
  id, kind: "codex", label: "ChatGPT 1", email: "", priority: 0, enabled: true, source: "cli",
  disabled_reason: "", usage: null, cooldown_until: 0, cooldown_reason: "", available: true, active: false, expires_at: 0,
}) as unknown as AccountView;

const provider: ProviderView = {
  id: "codex", name: "Codex", icon: "codex", mono: "C", color: "#10a37f", base_url: "", env: "", chip: "",
  kind: "openai", custom: false, insist: false, connected: true, has_key: false,
  key_hint: "", key_hints: [], key_pool: false, base_url_override: "", enabled: true, local: false,
  account: {
    display_name: "ChatGPT / Codex", short_name: "Codex", login_file: "~/.x/creds.json", login_command: "x login",
    paste_hint: "Paste credentials", setup_command: "", token_prefix: "sk-", warning: "", terms_gate: null, key_login: false,
  },
} as unknown as ProviderView;

const store = (accounts: AccountView[]) => set({
  providers: [provider], accounts,
  settings: { paused_all: false, paused_reason: "" } as unknown as Settings,
  settingsTab: "accounts",
});

/** Open the row's context menu and return the revealed email row's text. */
const rightClick = () => {
  const row = document.querySelector(".acct");
  if (!row) throw new Error("no account row rendered");
  fireEvent.contextMenu(row, { clientX: 40, clientY: 40 });
};

const rows = () => [...document.querySelectorAll(".pop.ctx .mrow")].map((r) => r.textContent ?? "");

// `MorphText` (used for the account state label) pulls in torph, which reaches
// for `matchMedia` on mount. jsdom has none — the same shim the other session
// tests use.
beforeEach(() => {
  (window as unknown as { matchMedia?: unknown }).matchMedia ??= () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  (Element.prototype as unknown as { getAnimations?: unknown }).getAnimations ??= () => [];
});

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("revealing account details from the toolbar", () => {
  it("requires confirmation, supports hiding, and resets on remount", () => {
    store([{ ...acct("a1"), email: EMAIL }]);
    const view = render(<AccountsTab />);
    fireEvent.click(view.getByRole("button", { name: "Reveal" }));
    expect(document.body.textContent).toContain("Anyone viewing your screen");
    expect(document.querySelector(".acct")?.textContent).not.toContain(EMAIL);
    fireEvent.click(view.getByRole("button", { name: "Cancel" }));
    expect(document.body.textContent).not.toContain(EMAIL);
    fireEvent.click(view.getByRole("button", { name: "Reveal" }));
    fireEvent.click(view.getByRole("button", { name: "Reveal details" }));
    expect(document.querySelector(".acct")?.textContent).toContain(EMAIL);
    fireEvent.click(view.getByRole("button", { name: "Hide" }));
    expect(document.body.textContent).not.toContain(EMAIL);
    fireEvent.click(view.getByRole("button", { name: "Reveal" }));
    fireEvent.click(view.getByRole("button", { name: "Reveal details" }));
    view.unmount();
    render(<AccountsTab />);
    expect(document.body.textContent).not.toContain(EMAIL);
  });

  it("shows recorded emails, partial OpenCode keys, and missing-email feedback", () => {
    set({ providers: [provider, { ...provider, id: "opencode-go" }], accounts: [
      { ...acct("a1"), email: EMAIL },
      { ...acct("a2"), email: "second@example.com" },
      { ...acct("a3") },
      { ...acct("a4"), kind: "opencode-go", key_hint: "••••abcd" },
    ] });
    const view = render(<AccountsTab />);
    expect(document.body.textContent).not.toContain("••••abcd");
    fireEvent.click(view.getByRole("button", { name: "Reveal" }));
    fireEvent.click(view.getByRole("button", { name: "Reveal details" }));
    const text = [...document.querySelectorAll(".acct")].map((row) => row.textContent).join(" ");
    expect(text).toContain(EMAIL);
    expect(text).toContain("second@example.com");
    expect(text).toContain("No email recorded");
    expect(text).toContain("••••abcd");
  });
});

describe("revealing an account email", () => {
  it("keeps the address off the page until it is asked for", () => {
    store([{ ...acct("a1"), email: EMAIL }]);
    render(<AccountsTab />);
    expect(document.body.textContent, "the row must not print the address").not.toContain(EMAIL);
  });

  it("masks it in the menu, then shows it on click", () => {
    store([{ ...acct("a1"), email: EMAIL }]);
    render(<AccountsTab />);
    rightClick();

    const reveal = rows().find((r) => r.includes("Reveal"));
    expect(reveal, "the menu must offer Reveal").toBeDefined();
    // Present but not readable: the local part is elided and the domain kept, so
    // the reveal itself is not the leak.
    expect(reveal).not.toContain(EMAIL);
    expect(reveal).toContain("example.com");

    fireEvent.click([...document.querySelectorAll(".pop.ctx .mrow")].find((r) => r.textContent?.includes("Reveal"))!);
    expect(document.body.textContent).toContain(EMAIL);
  });

  it("says so rather than offering a reveal that reveals nothing", () => {
    // An API-key account has no sign-in address. A menu row that looks live and
    // does nothing reads as a bug, so it has to admit there is nothing there.
    store([{ ...acct("a1"), email: "" }]);
    render(<AccountsTab />);
    rightClick();
    expect(rows().find((r) => r.includes("Reveal"))).toContain("No email recorded");
  });
});
