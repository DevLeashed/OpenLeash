// @vitest-environment jsdom
// Settings → Plugins has two properties that are easy to lose and impossible to
// notice afterwards:
//
//   1. Every plugin ships OFF. The agent gains tools the moment a plugin is on
//      and the user cannot undo that for a running chat, so "on" has to be
//      something a person did. A default that drifted to `true` would hand every
//      install GitHub writes and a mouse, and nothing else in the UI would say so.
//
//   2. The page is labelled Experimental, and the label sits on the Plugins page
//      header — not on the settings nav entry. The page is where the user is
//      about to switch on real capabilities on this machine, so that is where
//      the label has to be; a badge back on the nav list would clutter every
//      settings visit with a tag for a page they did not open.
//
// The second one is easy to "fix" by deleting a tag that looks decorative, and
// the first by inheriting a truthy default in the `OFF` fallback object, so both
// are pinned here rather than left to review.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("./Connectors", () => ({ AppConnectors: () => <div>App connectors</div> }));

import { api, type Plugins, type PluginsStatus, type Settings } from "../api";
import { set } from "../store";
import { ConnectorsTab, PluginsTab } from "./Plugins";
import { SettingsView } from "./Settings";

/** A config with nothing switched on, as a fresh install's settings.json reads. */
const allOff: Plugins = {
  github: { enabled: false },
  computer: { enabled: false, settle_ms: 600 },
  browser: { enabled: false, width: 1280, height: 800 },
};

const store = (plugins: Plugins | undefined, tab = "plugins") =>
  set({ settings: { plugins, mcp: [], allow: [], settingsTab: tab } as unknown as Settings, settingsTab: tab });

/** `plugins_status` would otherwise resolve to null and leave every Dot in the
 *  "Checking…" state; the values here are only read for their presence. */
const status = {
  has_token: false, can_login: false,
  github: { ok: false, source: null, error: "No token found" },
  computer: { ok: true, monitors: [{ name: "", w: 2560, h: 1440, primary: true }] },
  browser: { ok: true, browser: "Chrome" },
} as unknown as PluginsStatus;

beforeEach(() => vi.spyOn(api, "pluginsStatus").mockResolvedValue(status));

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

/** Every plugin switch on the page, by its accessible name. */
const switches = () => [...document.querySelectorAll('[role="switch"]')];

describe("plugins ship off", () => {
  it("shows every switch off for a config that has nothing enabled", async () => {
    store(allOff);
    render(<PluginsTab />);
    await vi.waitFor(() => expect(switches().length).toBe(2));

    for (const el of switches()) {
      expect(el.getAttribute("aria-checked"), `${el.getAttribute("aria-label")} must start off`).toBe("false");
    }
  });

  it("still shows them off when the backend has said nothing yet", () => {
    // The `OFF` fallback in Plugins.tsx is a second copy of the same default. A
    // truthy field in it would light up a plugin for the one frame before the
    // real settings arrive — and if that copy ever stops being a copy, forever.
    store(undefined as unknown as Plugins);
    render(<PluginsTab />);
    for (const el of switches()) expect(el.getAttribute("aria-checked")).toBe("false");
  });

  it("shows no plugin as on even though one is configured on", async () => {
    // Guards against the test above passing only because the fixtures agree:
    // flipping one must actually move its switch, or "all off" proves nothing.
    store({ ...allOff, computer: { enabled: true, settle_ms: 600 } });
    render(<PluginsTab />);
    await vi.waitFor(() => expect(switches().length).toBe(2));
    expect(switches().find((s) => s.getAttribute("aria-label") === "Computer use")?.getAttribute("aria-checked")).toBe("true");
    expect(switches().find((s) => s.getAttribute("aria-label") === "Browser plugin")?.getAttribute("aria-checked")).toBe("false");
  });

  it("does not render a 0x0 viewport for a config that never set one", () => {
    // A derived Rust Default leaves width/height at 0, which the UI would show
    // verbatim and the browser would clamp to 320x240. The page must never
    // display the degenerate value even if settings.json somehow carries it.
    store({ ...allOff, browser: { enabled: false, width: 0, height: 0 } });
    render(<PluginsTab />);
    fireEvent.click(screen.getByRole("button", { name: "Browser Details" }));
    expect(document.body.textContent).not.toContain("0 × 0");
    expect(document.body.textContent).toContain("1280 × 800");
  });
});

describe("compact details and connectors", () => {
  it("keeps descriptions and settings collapsed with accessible independent buttons", () => {
    store(allOff);
    render(<PluginsTab />);
    const computer = screen.getByRole("button", { name: "Computer use Details" });
    expect(computer.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("Settle time")).toBeNull();
    expect(screen.queryByText("Viewport")).toBeNull();
    fireEvent.click(computer);
    expect(computer.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("Settle time")).toBeTruthy();
    expect(screen.queryByText("Viewport")).toBeNull();
    fireEvent.click(computer);
    expect(screen.queryByText("Settle time")).toBeNull();
  });

  it("shows saved custom viewport rather than a different preset", () => {
    store({ ...allOff, browser: { enabled: false, width: 1366, height: 768 } });
    render(<PluginsTab />);
    fireEvent.click(screen.getByRole("button", { name: "Browser Details" }));
    expect(document.body.textContent).toContain("1366 × 768");
  });

  it("describes readiness without implying the browser is driving", async () => {
    store(allOff);
    render(<PluginsTab />);
    await screen.findByText(/Screen available/);
    expect(screen.getByText(/Browser available/)).toBeTruthy();
    expect(document.body.textContent).not.toContain("Driving");
    expect(screen.queryByText("GitHub")).toBeNull();
  });

  it("moves GitHub to Connectors with an off switch and collapsed authentication", () => {
    store(allOff, "connectors");
    render(<ConnectorsTab />);
    expect(switches()).toHaveLength(1);
    expect(switches()[0]!.getAttribute("aria-checked")).toBe("false");
    expect(screen.queryByRole("button", { name: "Sign in with GitHub" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "GitHub Details" }));
    expect(screen.getByRole("button", { name: "Sign in with GitHub" })).toBeTruthy();
    expect(screen.getByText("App connectors")).toBeTruthy();
  });

  it("blocks duplicate saves while pending and reports failed saves without enabling", async () => {
    store(allOff);
    let reject!: (reason: Error) => void;
    const save = vi.spyOn(api, "settings").mockImplementation(() => new Promise((_resolve, fail) => { reject = fail; }));
    render(<PluginsTab />);
    fireEvent.click(screen.getByRole("switch", { name: "Computer use" }));
    expect(screen.getByRole("status").textContent).toBe("Saving…");
    fireEvent.click(screen.getByRole("switch", { name: "Browser plugin" }));
    expect(save).toHaveBeenCalledTimes(1);
    reject(new Error("disk full"));
    await vi.waitFor(() => expect(screen.queryByRole("status")).toBeNull());
    expect(screen.getByRole("switch", { name: "Computer use" }).getAttribute("aria-checked")).toBe("false");
  });

  it("routes the Connectors settings navigation", () => {
    store(allOff);
    render(<SettingsView />);
    fireEvent.click(screen.getByRole("tab", { name: "Connectors" }));
    expect(screen.getByText("GitHub")).toBeTruthy();
    expect(screen.queryByText("Computer use")).toBeNull();
  });
});

describe("the plugins page is labelled experimental", () => {
  it("carries the tag on the page header, and says what it means", () => {
    store(allOff);
    render(<PluginsTab />);
    const tags = [...document.querySelectorAll(".exptag")];
    expect(tags.length, "the page header must carry the tag").toBeGreaterThan(0);
    expect(tags[0]!.textContent?.toLowerCase()).toContain("experimental");
    // And it has to say what the tag means, not just that it is there.
    expect(document.body.textContent).toContain("Still finding its shape");
    expect(document.body.textContent).toContain("off until you switch it on");
  });

  it("does not tag the Plugins entry in the settings nav", () => {
    store(allOff, "plugins");
    render(<SettingsView />);
    const nav = [...document.querySelectorAll(".stab")];
    const pluginsTab = nav.find((t) => t.textContent?.includes("Plugins"));
    expect(pluginsTab, "the Plugins nav entry must exist").toBeDefined();
    expect(
      pluginsTab!.querySelector(".exptag"),
      "the tag belongs on the page, not the nav entry"
    ).toBeNull();
  });

  it("tags nothing in the settings nav at all", () => {
    store(allOff, "plugins");
    render(<SettingsView />);
    const tagged = [...document.querySelectorAll(".stab")].filter((t) => t.querySelector(".exptag"));
    // The tag is per-page, not a nav decoration: any entry appearing here means
    // the badge has drifted back onto the tab list.
    expect(tagged).toHaveLength(0);
  });
});