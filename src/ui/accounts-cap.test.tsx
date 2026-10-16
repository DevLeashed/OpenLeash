// @vitest-environment jsdom
// Caps: double-clicking a usage line sets a ceiling the harness will not spend
// past, and the line says so afterwards.
//
// Two things are worth pinning here. The first is that a cap is *not* a bench:
// it lives in settings beside the account and nothing in the router's own
// bookkeeping clears it, so the only way it can be wrong is by not being drawn —
// a cap nobody can see reads as the harness having stopped for no reason. The
// second is that the dialog sends a *duration* rather than a deadline, so a
// phone browser with a wrong clock cannot set a cap that has already expired.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import { type AccountView, type AcctUsage, type ProviderView, type Settings, type UsageCap } from "../api";
import { set } from "../store";
import { AccountsTab, capSummary, liveCap } from "./Accounts";

const provider: ProviderView = {
  id: "codex", name: "Codex", icon: "codex", mono: "C", color: "#10a37f", base_url: "", env: "", chip: "",
  kind: "openai", custom: false, insist: false, connected: true, has_key: false,
  key_hint: "", key_hints: [], key_pool: false, base_url_override: "", enabled: true, local: false,
  account: {
    display_name: "ChatGPT / Codex", short_name: "Codex", login_file: "~/.x/creds.json", login_command: "x login",
    paste_hint: "Paste credentials", setup_command: "", token_prefix: "sk-", warning: "", terms_gate: null, key_login: false,
  },
} as unknown as ProviderView;

const usage = (label: string, used: number): AcctUsage => ({ windows: [{ label, used, resets_at: 0 }], limited: false, plan: "", fetched: 0, error: "" });

const acct = (id: string, over: Partial<AccountView> = {}): AccountView => ({
  id, kind: "codex", label: "ChatGPT " + id, email: "", priority: 0, enabled: true, source: "cli",
  disabled_reason: "", usage: usage("5h", 20), cooldown_until: 0, cooldown_reason: "", available: true, active: false, expires_at: 0,
  caps: [], ...over,
} as unknown as AccountView);

const store = (accounts: AccountView[]) => set({
  providers: [provider],
  accounts,
  settings: { paused_all: false, paused_reason: "" } as unknown as Settings,
  settingsTab: "accounts",
});

const secs = (offset: number) => Math.floor(Date.now() / 1000) + offset;

/** The dialog's apply button, whatever it is currently called. */
const apply = () => [...document.querySelectorAll("button")].find((b) => b.textContent === "Set cap" || b.textContent === "Update cap")!;

// `MorphText` (used for the percentages) pulls in torph, which reaches for
// `matchMedia` on mount. jsdom has none — the same shim the other session tests
// use.
beforeEach(() => {
  (window as unknown as { matchMedia?: unknown }).matchMedia ??= () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  (Element.prototype as unknown as { getAnimations?: unknown }).getAnimations ??= () => [];
  invoke.mockResolvedValue([]);
});

afterEach(() => { cleanup(); vi.clearAllMocks(); });

describe("a cap on a usage line", () => {
  it("draws itself as a badge while it stands, and as held once it is reached", () => {
    const armed: UsageCap = { window: "5h", used: 50, until: 0, holding: false };
    store([acct("a1", { caps: [armed], usage: usage("5h", 30) })]);
    const view = render(<AccountsTab />);
    const row = () => document.querySelector(".acct")!;
    expect(row().textContent).toContain("cap 50%");
    expect(row().querySelector(".capbadge.held")).toBeNull();
    expect(row().querySelector(".capmark")).toBeTruthy();
    view.unmount();

    store([acct("a1", { caps: [{ ...armed, holding: true }], usage: usage("5h", 62), available: false })]);
    render(<AccountsTab />);
    expect(row().textContent).toContain("capped");
    // The account is not "Ready" while its own cap is what is holding it back.
    expect(row().textContent).not.toContain("Ready");
    expect(row().querySelector(".capmark.held")).toBeTruthy();
  });

  it("caps the one account a row's line belongs to, permanently by default", () => {
    store([acct("a1"), acct("a2")]);
    render(<AccountsTab />);
    fireEvent.doubleClick(document.querySelector(".acct .ubar")!);
    expect(document.body.textContent).toContain("Cap usage · 5h");
    expect(document.body.textContent, "the dialog has to name who it caps").toContain("ChatGPT a1");
    fireEvent.click(apply());
    expect(invoke).toHaveBeenCalledWith("account_cap_set", { ids: ["a1"], window: "5h", used: 80, secs: 0 });
  });

  it("sends a duration, not a deadline, and only counts it down for a temporary cap", () => {
    store([acct("a1")]);
    render(<AccountsTab />);
    fireEvent.doubleClick(document.querySelector(".acct .ubar")!);
    // Permanent asks for no time at all.
    expect(document.querySelector(".capslider")).toBeTruthy();
    expect([...document.querySelectorAll("button")].some((b) => b.textContent === "4 hours")).toBe(false);
    fireEvent.click(document.querySelector('[role="radio"][aria-label="Temporary"]')!);
    fireEvent.click(document.querySelector('[role="radio"][aria-label="4 hours"]')!);
    fireEvent.click(apply());
    expect(invoke).toHaveBeenCalledWith("account_cap_set", { ids: ["a1"], window: "5h", used: 80, secs: 4 * 3600 });
  });

  it("lifts an existing cap from the same line it was set on", () => {
    store([acct("a1", { caps: [{ window: "5h", used: 50, until: 0, holding: false }], usage: usage("5h", 30) })]);
    render(<AccountsTab />);
    fireEvent.doubleClick(document.querySelector(".acct .ubar")!);
    // The dialog opens on the cap that is there rather than on the default.
    expect(document.body.textContent).toContain("Stop at 50% used");
    fireEvent.click([...document.querySelectorAll("button")].find((b) => b.textContent === "Remove cap")!);
    expect(invoke).toHaveBeenCalledWith("account_cap_clear", { ids: ["a1"], window: "5h" });
  });

  it("caps every account of a provider from its All line", () => {
    store([acct("a1"), acct("a2")]);
    render(<AccountsTab />);
    fireEvent.doubleClick(document.querySelector(".srowx .ubar")!);
    expect(document.body.textContent).toContain("each of the 2 ChatGPT / Codex accounts");
    fireEvent.click(apply());
    expect(invoke).toHaveBeenCalledWith("account_cap_set", { ids: ["a1", "a2"], window: "5h", used: 80, secs: 0 });
  });

  it("shows a cap that outlived the usage reading, so it can still be lifted", () => {
    // A cap stands on the account, not on a reading: a provider that stops
    // reporting the window would otherwise leave a cap with no line to lift it
    // from — invisible, and still holding the account back the moment usage is
    // reported again.
    store([acct("a1", { usage: null, caps: [{ window: "Week", used: 50, until: 0, holding: false }] })]);
    render(<AccountsTab />);
    const row = document.querySelector(".acct")!;
    expect(row.textContent).toContain("Usage not read yet");
    expect(row.textContent).toContain("Week · cap 50%");
    fireEvent.click(row.querySelector(".capbadge")!);
    expect(document.body.textContent).toContain("Cap usage · Week");
    expect(document.body.textContent, "with no reading there is nothing to claim").not.toContain("Now at 0% used");
  });

  it("says how many accounts carry a cap when they do not agree on one", () => {
    const cap = (used: number): UsageCap => ({ window: "5h", used, until: 0, holding: false });
    store([acct("a1", { caps: [cap(50)] }), acct("a2", { caps: [cap(70)] })]);
    const view = render(<AccountsTab />);
    expect(document.querySelector(".srowx .capnote")?.textContent).toBe("2 capped");
    expect(document.querySelector(".srowx .capbadge")).toBeNull();
    view.unmount();

    // Agreeing on one cap is the case the header can draw as a single line.
    store([acct("a1", { caps: [cap(50)] }), acct("a2", { caps: [cap(50)] })]);
    render(<AccountsTab />);
    expect(document.querySelector(".srowx .capbadge")?.textContent).toContain("cap 50%");
    expect(document.querySelector(".srowx .capnote")).toBeNull();
  });
});

describe("reading caps off an account", () => {
  it("treats a temporary cap that has run out as no cap at all", () => {
    expect(liveCap(acct("a1", { caps: [{ window: "5h", used: 50, until: 0, holding: false }] }), "5h")).toBeTruthy();
    expect(liveCap(acct("a1", { caps: [{ window: "5h", used: 50, until: secs(600), holding: false }] }), "5h")).toBeTruthy();
    expect(liveCap(acct("a1", { caps: [{ window: "5h", used: 50, until: secs(-1), holding: false }] }), "5h")).toBeNull();
    // The label is the window's identity: a cap on another window is not this one.
    expect(liveCap(acct("a1", { caps: [{ window: "Week", used: 50, until: 0, holding: false }] }), "5h")).toBeNull();
    // And an account from a backend that predates caps simply has none.
    expect(liveCap(acct("a1"), "5h")).toBeNull();
  });

  it("only calls a cap shared when every account carries the same one", () => {
    const cap = (used: number, until = 0): UsageCap => ({ window: "5h", used, until, holding: false });
    const one = acct("a1", { caps: [cap(50)] });
    const same = acct("a2", { caps: [cap(50)] });
    const other = acct("a2", { caps: [cap(70)] });
    const none = acct("a2");
    expect(capSummary([one, same], "5h")).toEqual({ shared: cap(50), n: 2 });
    expect(capSummary([one, other], "5h")).toEqual({ shared: null, n: 2 });
    expect(capSummary([one, none], "5h")).toEqual({ shared: null, n: 1 });
    expect(capSummary([none, none], "5h")).toEqual({ shared: null, n: 0 });
    // One account held while its neighbour still works is not the pool capped:
    // the aggregated line keeps reading as a cap ahead of the usage, and the row
    // bars are where the held account says so.
    const held = acct("a1", { caps: [{ ...cap(50), holding: true }] });
    expect(capSummary([held, same], "5h").shared?.holding).toBe(false);
    expect(capSummary([held, acct("a2", { caps: [{ ...cap(50), holding: true }] })], "5h").shared?.holding).toBe(true);
  });
});
