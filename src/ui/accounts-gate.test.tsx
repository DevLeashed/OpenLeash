// @vitest-environment jsdom
// Some accounts can only be added after the user says they know the terms are
// a risk (a provider's `terms_gate`), so the app has to make them say so *before*
// they get to the connect dialog.
//
// Claude subscriptions carry such a gate but are hidden from the UI
// (`shownProviders` in store.ts), so these tests use a stand-in gated provider.
// The gate itself is generic and still has to hold for any provider that sets one.
//
// The field bug this pins: the warning used to be a faint line at the bottom of
// the connect form — the one piece of text on the screen that nobody reads when
// they already know what they came to do, sitting directly under an "Import"
// button that does the thing they came for. The fix is a separate blocking
// screen in front of the form.
//
// So the property under test is a *sequence*, not a string: "Add" must not put
// the connect form on screen. `api.accountImport*` must be uncallable until the
// acknowledgement has been clicked. Asserting on the presence of some warning
// text would have passed against the old code.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import { api, type ProviderView, type Settings } from "../api";
import { set } from "../store";
import { AccountsTab } from "./Accounts";

const acct = (id: string, extra: Record<string, unknown> = {}) => ({
  display_name: id, short_name: id, login_file: "~/.x/creds.json", login_command: "x login",
  paste_hint: "Paste credentials", setup_command: "x setup-token", token_prefix: "sk-x", warning: "",
  terms_gate: null, key_login: false, ...extra,
});

/** The real display names, because the group is looked up by its heading. */
const NAMES: Record<string, string> = { gated: "Gated Service", codex: "ChatGPT / Codex" };

/** `extra` overrides the account block, so a caller can pass just
 *  `{ account: { terms_gate } }` instead of restating the whole block. */
const provider = (id: string, extra: Record<string, unknown> = {}): ProviderView => {
  const { account, ...rest } = extra;
  return {
    id, name: id, icon: "codex", mono: "C", color: "#d97757", base_url: "", env: "", chip: "",
    kind: "anthropic", custom: false, insist: false, connected: false, has_key: false,
    key_hint: "", key_hints: [], key_pool: false, base_url_override: "", enabled: true, local: false,
    account: acct(id, { display_name: NAMES[id] ?? id, ...(account ?? {}) } as Record<string, unknown>),
    ...rest,
  } as unknown as ProviderView;
};

const termsGate = {
  title: "This is not what the service is for",
  lede: "The provider's terms do not cover this.",
  points: ["You may not share your Account login information.", "No automated means without an API key."],
  terms_url: "https://www.anthropic.com/legal/consumer-terms",
  accept: "I understand, and I'm doing this at my own risk",
};

const store = (provs: ProviderView[]) => set({
  providers: provs, accounts: [],
  settings: { paused_all: false, paused_reason: "" } as unknown as Settings,
});

/** Click the Add button for the provider group whose heading is `label`.
 *  Matched on `textContent` alone so the lookup does not depend on ProviderIcon's
 *  internals — that renders an SVG whose only text is a `<title>`, which would
 *  make a substring match on the provider id succeed for the wrong row. */
const clickAdd = async (label: string) => {
  const groups = [...document.querySelectorAll(".sgroup")];
  if (!groups.length) throw new Error("no provider groups rendered at all");
  const row = groups.find((g) => g.textContent?.includes(label));
  if (!row) throw new Error(`no group for ${label}; got ${JSON.stringify(groups.map((g) => g.textContent?.slice(0, 60)))}`);
  const btn = [...row.querySelectorAll("button")].find((b) => b.textContent?.startsWith("Add"));
  if (!btn) throw new Error(`no Add button in ${label}`);
  await act(async () => { btn.click(); });
};

const button = (text: string) =>
  [...document.querySelectorAll("button")].find((b) => b.textContent?.includes(text));

describe("adding an account behind a terms gate", () => {
  beforeEach(() => {
    (globalThis as unknown as { window: { setTimeout: typeof setTimeout } }).window ??= { setTimeout };
  });
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  it("puts the terms gate in front of the connect form, not in it", async () => {
    store([provider("gated", { account: { terms_gate: termsGate } })]);
    render(<AccountsTab />);
    await clickAdd("Gated Service");

    // The gate is up...
    expect(document.body.textContent).toContain(termsGate.title);
    expect(document.body.textContent).toContain("own risk");
    expect(button("Add")?.textContent).toBe("Add");
    expect(button("Add anyway")).toBeUndefined();
    const copy = document.body.textContent ?? "";
    expect(copy).not.toContain("This will most likely get your account banned.");
    // ...and what it is gating is not. These are the strings unique to the
    // connect form, so finding either would mean the gate was bypassed.
    expect(document.body.textContent).not.toContain("Import CLI login");
    expect(document.body.textContent).not.toContain("Or paste credentials");
    expect(document.body.textContent).not.toContain("~/.x/creds.json");
  });

  it("requires a second acknowledgement in a separate warning before opening the form", async () => {
    store([provider("gated", { account: { terms_gate: termsGate } })]);
    render(<AccountsTab />);
    await clickAdd("Gated Service");

    await act(async () => { button(termsGate.accept)!.click(); });
    expect(document.body.textContent).toContain("Final warning");
    expect(document.body.textContent).toContain("This will most likely get your account banned. You have been warned.");
    expect(document.body.textContent).not.toContain("Import CLI login");
    expect(button("I understand")).toBeDefined();

    await act(async () => { button("I understand")!.click(); });
    expect(document.body.textContent).toContain("Import CLI login");
    expect(document.body.textContent).not.toContain("Final warning");
    expect(document.body.textContent).not.toContain(termsGate.title);
  });

  it("never reaches the backend without that click", async () => {
    const local = vi.spyOn(api, "accountImportLocal").mockResolvedValue([]);
    const paste = vi.spyOn(api, "accountImport").mockResolvedValue([]);
    store([provider("gated", { account: { terms_gate: termsGate } })]);
    render(<AccountsTab />);
    await clickAdd("Gated Service");

    // The gate is up, so there is no Import or Connect control to press at all.
    expect(button("Import")).toBeUndefined();
    expect(button("Connect")).toBeUndefined();
    expect(local, "no credential may be read before consent").not.toHaveBeenCalled();
    expect(paste).not.toHaveBeenCalled();
  });

  it("cancelling the gate closes the flow rather than reaching the form", async () => {
    store([provider("gated", { account: { terms_gate: termsGate } })]);
    render(<AccountsTab />);
    await clickAdd("Gated Service");
    await act(async () => { button("Cancel")!.click(); });

    expect(document.querySelector('[role="dialog"]'), "the gate must not hand over to the form").toBeNull();
    expect(document.body.textContent).not.toContain("Import CLI login");
  });

  it("cancelling the final warning closes the flow rather than reaching the form", async () => {
    store([provider("gated", { account: { terms_gate: termsGate } })]);
    render(<AccountsTab />);
    await clickAdd("Gated Service");
    await act(async () => { button(termsGate.accept)!.click(); });
    expect(document.body.textContent).toContain("Final warning");
    await act(async () => { button("Cancel")!.click(); });

    expect(document.querySelector('[role="dialog"]'), "the warning must not hand over to the form").toBeNull();
    expect(document.body.textContent).not.toContain("Import CLI login");
  });

  it("shows OpenCode Go as an API-key account, not as OAuth login", async () => {
    const local = vi.spyOn(api, "accountImportLocal").mockResolvedValue([]);
    const paste = vi.spyOn(api, "accountImport").mockResolvedValue([]);
    store([provider("opencode-go", {
      icon: "opencode",
      name: "OpenCode Go",
      account: {
        display_name: "OpenCode Go",
        short_name: "OpenCode",
        login_file: "~/.local/share/opencode/auth.json",
        login_command: "opencode auth login",
        paste_hint: "Paste an OpenCode Go API key (or its auth.json)",
        setup_command: "",
        token_prefix: "",
        warning: "",
        terms_gate: null,
        key_login: true,
      },
    })]);
    render(<AccountsTab />);
    await clickAdd("OpenCode Go");

    expect(document.body.textContent).toContain("Import OpenCode key");
    expect(document.body.textContent).toContain("API key created with your Go subscription");
    expect(document.body.textContent).toContain("Or paste an API key");
    expect(document.body.textContent).toContain("Connect key");
    expect(document.body.textContent).not.toContain("setup-token");
    expect(document.body.textContent).not.toContain("own risk");
    expect(local).not.toHaveBeenCalled();
    expect(paste).not.toHaveBeenCalled();
  });
});