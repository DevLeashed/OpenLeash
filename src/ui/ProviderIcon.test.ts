/// <reference types="node" />
// ProviderIcon is the only place in the app that injects raw HTML, and the key
// it looks up comes from backend provider data. It has to miss cleanly on
// anything that is not a bundled icon.
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { set } from "../store";
import { ProviderIcon } from "./ProviderIcon";

const render = (provider: string, icon?: string) =>
  renderToStaticMarkup(createElement(ProviderIcon, { provider, icon }));

describe("provider icons", () => {
  beforeEach(() => set({ providers: [] }));

  it("draws a bundled icon when the name is one", () => {
    expect(render("anthropic")).toContain("<svg");
  });

  it("falls back to the generic server glyph for an unknown name", () => {
    const html = render("some-provider-nobody-shipped");
    expect(html).toContain("<svg");
    expect(html, "should not have taken the raw-HTML path").not.toContain("provider-icon");
  });

  it("does not inject anything for inherited object keys", () => {
    // The bug this guards: ICON_ASSETS was a plain object literal, so
    // "constructor" and "toString" returned truthy inherited values and were
    // passed to dangerouslySetInnerHTML.
    for (const key of ["constructor", "toString", "__proto__", "hasOwnProperty", "valueOf"]) {
      const html = render(key);
      expect(html, `"${key}" should not reach dangerouslySetInnerHTML`).not.toContain("provider-icon");
      expect(html.toLowerCase()).not.toContain("function");
    }
  });
});
