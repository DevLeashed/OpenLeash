// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { api, type AllowRule, type Plugins, type Settings } from "../api";
import { set } from "../store";
import { BrowserSettings, normalizeHostPattern, parseSiteRule, siteRule } from "./BrowserSettings";

const defaults: Plugins = {
  github: { enabled: false },
  computer: { enabled: false, settle_ms: 600 },
  browser: { enabled: true, width: 1280, height: 800, persistent_profile: false, headed: false, developer_mode: false, interactions: "ask", download_dir: "" },
};
const store = (allow: AllowRule[]) => set({ settings: { plugins: defaults, allow, mcp: [] } as unknown as Settings });
let saved: Partial<Settings>[] = [];

beforeEach(() => {
  saved = [];
  vi.spyOn(api, "settings").mockImplementation(async (patch: Partial<Settings>) => { saved.push(patch); return { plugins: defaults, allow: patch.allow ?? [], mcp: [] } as unknown as Settings; });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

function open(allow: AllowRule[] = [], save = vi.fn(async () => true)) {
  store(allow);
  render(<BrowserSettings p={defaults} status={{ ok: true, browser: "Chrome" }} save={save} />);
  fireEvent.click(screen.getByRole("button", { name: "Browser Details" }));
  return save;
}

describe("host patterns", () => {
  it("accepts hosts and subdomain wildcards, and nothing broader", () => {
    expect(normalizeHostPattern("Example.COM")).toBe("example.com");
    expect(normalizeHostPattern("*.example.com")).toBe("*.example.com");
    expect(normalizeHostPattern("https://shop.example.com/cart?x")).toBe("shop.example.com");
    expect(normalizeHostPattern("localhost:3000")).toBe("localhost");
    for (const bad of ["", "*.com", "*", "exa mple.com", "-a.com", "a..com", "*.*.com", "a_b.com", "<x>.com"]) expect(normalizeHostPattern(bad), bad).toBeNull();
    expect(normalizeHostPattern("*.example.com", false)).toBeNull();
    expect(normalizeHostPattern("192.168.1.20", false)).toBe("192.168.1.20");
  });
  it("round-trips the rule patterns", () => {
    expect(siteRule("allow", "a.test")).toBe("browser:site a.test");
    expect(siteRule("ask", "a.test")).toBe("browser:ask a.test");
    expect(siteRule("block", "a.test")).toBe("!browser:site a.test");
    expect(parseSiteRule("!browser:site *.a.test")).toEqual({ mode: "block", host: "*.a.test" });
    expect(parseSiteRule("browser:lan nas.local")).toBeNull();
    expect(parseSiteRule("npm run build *")).toBeNull();
  });
});

describe("browser settings", () => {
  it("shows the defaults: on, ask, throwaway headless profile, no developer mode", () => {
    open();
    // Attribute lookups rather than role queries: this page is large, and
    // computing every accessible name made the test crawl under a loaded CI box.
    const switches = Object.fromEntries([...document.querySelectorAll('[role="switch"]')].map((el) => [el.getAttribute("aria-label"), el.getAttribute("aria-checked")]));
    expect(switches).toEqual({ "Browser plugin": "true", "Keep sign-ins between chats": "false", "Show browser window": "false", "Developer mode (raw CDP — every call asks)": "false" });
    expect(screen.getByText(/Browser available · Chrome · headless/)).toBeTruthy();
    expect(document.querySelector('[role="radiogroup"][aria-label="Page interactions"] [aria-checked="true"]')?.getAttribute("aria-label")).toBe("Ask");
    expect(document.body.textContent).toContain("never your real Chrome profile");
    expect(document.body.textContent).toContain("act as you");
  });

  it("saves each setting through the plugin config", () => {
    const save = open();
    fireEvent.click(screen.getByRole("switch", { name: "Keep sign-ins between chats" }));
    expect(save).toHaveBeenLastCalledWith({ ...defaults, browser: { ...defaults.browser, persistent_profile: true } });
    fireEvent.click(within(screen.getByRole("radiogroup", { name: "Page interactions" })).getByRole("radio", { name: "Block" }));
    expect(save).toHaveBeenLastCalledWith({ ...defaults, browser: { ...defaults.browser, interactions: "block" } });
    const dl = screen.getByLabelText("Downloads folder");
    fireEvent.change(dl, { target: { value: " D:/dl " } });
    fireEvent.keyDown(dl, { key: "Enter" });
    expect(save).toHaveBeenLastCalledWith({ ...defaults, browser: { ...defaults.browser, download_dir: "D:/dl" } });
  });

  it("writes site exceptions as permission rules beside the existing ones", async () => {
    const existing = { pattern: "npm test *", project: "" };
    open([existing, { pattern: "browser:site old.test", project: "" }]);
    const host = screen.getByLabelText("Site host");
    fireEvent.change(host, { target: { value: "not a host" } });
    fireEvent.click(screen.getByText("Add site"));
    expect(screen.getByRole("alert").textContent).toContain("example.com");
    expect(saved).toHaveLength(0);
    fireEvent.change(host, { target: { value: "*.Shop.test" } });
    fireEvent.click(within(screen.getByRole("radiogroup", { name: "New site rule" })).getByRole("radio", { name: "Block" }));
    fireEvent.click(screen.getByText("Add site"));
    await vi.waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0]!.allow).toEqual([existing, { pattern: "browser:site old.test", project: "" }, { pattern: "!browser:site *.shop.test", project: "" }]);
    await screen.findByText("*.shop.test");
    // Changing a row's mode rewrites that rule in place; removing drops only it.
    fireEvent.click(within(screen.getByRole("radiogroup", { name: "Rule for old.test" })).getByRole("radio", { name: "Ask" }));
    await vi.waitFor(() => expect(saved).toHaveLength(2));
    expect(saved[1]!.allow![1]).toEqual({ pattern: "browser:ask old.test", project: "" });
    await vi.waitFor(() => expect(within(screen.getByRole("radiogroup", { name: "Rule for old.test" })).getByRole("radio", { name: "Ask" }).getAttribute("aria-checked")).toBe("true"));
    fireEvent.click(screen.getByRole("button", { name: "Remove *.shop.test" }));
    await vi.waitFor(() => expect(saved).toHaveLength(3));
    expect(saved[2]!.allow).toEqual([existing, { pattern: "browser:ask old.test", project: "" }]);
  });

  it("manages allowed LAN hosts", async () => {
    open([{ pattern: "browser:lan nas.local", project: "" }]);
    expect(screen.getByText("nas.local")).toBeTruthy();
    const host = screen.getByLabelText("Local-network host");
    fireEvent.change(host, { target: { value: "*.lan" } });
    fireEvent.click(screen.getByText("Add host"));
    expect(screen.getByRole("alert")).toBeTruthy();
    fireEvent.change(host, { target: { value: "192.168.1.20" } });
    fireEvent.keyDown(host, { key: "Enter" });
    await vi.waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0]!.allow).toEqual([{ pattern: "browser:lan nas.local", project: "" }, { pattern: "browser:lan 192.168.1.20", project: "" }]);
    await screen.findByText("192.168.1.20");
    fireEvent.click(screen.getByRole("button", { name: "Remove nas.local" }));
    await vi.waitFor(() => expect(saved).toHaveLength(2));
    expect(saved[1]!.allow).toEqual([{ pattern: "browser:lan 192.168.1.20", project: "" }]);
  });
});
