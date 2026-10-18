// @vitest-environment jsdom
/// <reference types="node" />
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { readFileSync } from "node:fs";
import { get, set } from "../store";
import { api } from "../api";
import type { AccountView, ProviderView, Settings } from "../api";

vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn() }) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { Sidebar } from "./Chrome";
import { AccountsTab } from "./Accounts";

// Toggling a subscription in the footer bar's menu used to mount/unmount its
// meter, so the bar snapped to new widths. Off meters now stay mounted and
// collapse through CSS, which is what lets the change animate.
const provider = (id: string, name: string) => ({ id, account: { short_name: name, display_name: name } }) as unknown as ProviderView;
const account = (kind: string, extra: Partial<AccountView> = {}) => ({ id: kind, kind, label: kind, email: "", priority: 0, cooldown_until: 0, available: true, enabled: true, caps: [], ...extra }) as unknown as AccountView;
const usage = (used: number[]) => ({ windows: used.map((u, i) => ({ label: i ? "Week" : "5h", used: u, resets_at: 0 })), limited: false, plan: "", fetched: 0, error: "" });
const store = (accounts: AccountView[]) => set({
  sidebar: true, view: "home", task: null, tasks: {}, models: [],
  providers: [provider("codex", "Codex")], accounts,
  settings: { saved_prompts: [], home_usage: ["codex"] } as unknown as Settings,
});

beforeEach(() => {
  window.matchMedia ??= (() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia;
  Element.prototype.getAnimations ??= () => [];
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("footer usage bar", () => {
  it("keeps a hidden subscription's meter mounted but collapsed", () => {
    set({
      sidebar: true, view: "home", task: null, tasks: {},
      providers: [provider("codex", "Codex"), provider("opencode-go", "OpenCode Go")],
      accounts: [account("codex"), account("opencode-go")],
      settings: { saved_prompts: [], home_usage: ["codex"] } as unknown as Settings,
    });
    const html = renderToStaticMarkup(createElement(Sidebar));
    const segs = [...html.matchAll(/class="useg([^"]*)"/g)].map((m) => m[1]!.trim());
    expect(segs).toEqual(["", "off"]);
    expect(html).toMatch(/aria-hidden="true" class="useg off"/);
  });

  it.each([null, usage([])])("does not render missing usage as a full allowance (%j)", async (reading) => {
    store([account("codex", { usage: reading })]);
    const view = render(createElement(Sidebar));
    expect(view.container.querySelector<HTMLElement>(".useg > div")?.style.width).toBe("0%");
    const bar = view.getByLabelText("Usage in this bar");
    fireEvent.focus(bar);
    await waitFor(() => expect(view.getByRole("tooltip").textContent).toContain("Codex: Usage unavailable"));
    expect(view.getByRole("tooltip").textContent).not.toContain("100% left");
    fireEvent.click(bar);
    expect(view.container.querySelector(".umenu-left")?.textContent).toBe("Usage unavailable");
  });

  it("keeps a measured zero distinct from unknown usage", () => {
    store([account("codex", { usage: usage([0]) })]);
    const view = render(createElement(Sidebar));
    expect(view.container.querySelector<HTMLElement>(".useg > div")?.style.width).toBe("100%");
    fireEvent.click(view.getByLabelText("Usage in this bar"));
    expect(view.container.querySelector(".umenu-left")?.textContent).toBe("100% left");
  });

  it("does not inflate a partial pool or combine independent windows", () => {
    store([account("codex", { usage: usage([80, 100]) }), account("codex", { usage: usage([]) })]);
    const view = render(createElement(Sidebar));
    expect(view.container.querySelector<HTMLElement>(".useg > div")?.style.width).toBe("20%");
    fireEvent.click(view.getByLabelText("Usage in this bar"));
    expect(view.container.querySelector(".umenu-left")?.textContent).toBe("20% left");
  });

  it("keeps an unavailable capped pool empty even without a reading", () => {
    store([account("codex", { usage: null, available: false, caps: [{ window: "5h", used: 80, until: 0, holding: true }] })]);
    const view = render(createElement(Sidebar));
    expect(view.container.querySelector(".useg.capped")).not.toBeNull();
    expect(view.container.querySelector<HTMLElement>(".useg > div")?.style.width).toBe("0%");
    fireEvent.click(view.getByLabelText("Usage in this bar"));
    expect(view.container.querySelector(".umenu-left")?.textContent).toBe("0% left");
  });

  it("collapses off meters with a transition instead of removing them", () => {
    const css = readFileSync("src/App.css", "utf8");
    expect(css).toMatch(/\.sbar \.useg \{[^}]*transition:[^}]*flex-grow/);
    expect(css).toMatch(/\.sbar \.useg\.off \{[^}]*flex-grow: 0/);
  });
});

describe("accounts refresh publication", () => {
  it("publishes readings immediately and does not overwrite a newer event after model discovery", async () => {
    const refreshed = [account("codex", { usage: null, label: "Refreshed account" })];
    const newer = [account("codex", { usage: null, label: "Newer account" })];
    store([account("codex", { usage: null })]);
    vi.spyOn(api, "accountsRefresh").mockResolvedValue(refreshed);
    let finishModels!: (models: []) => void;
    const models = vi.spyOn(api, "poolModels").mockImplementation(() => new Promise<[]>((resolve) => { finishModels = resolve; }));
    const view = render(createElement(AccountsTab));
    fireEvent.click(view.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(models).toHaveBeenCalledOnce());
    expect(get().accounts).toBe(refreshed);
    act(() => set({ accounts: newer }));
    await act(async () => finishModels([]));
    expect(get().accounts).toBe(newer);
    expect(view.getByRole("button", { name: "Refresh" }).hasAttribute("disabled")).toBe(false);
  });
});
