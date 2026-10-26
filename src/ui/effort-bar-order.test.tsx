// @vitest-environment jsdom
// The effort bar in the model picker, drawn the way the user sees it.
//
// The stored effort is untouched by this: 0 = Max … 4 = Low, the contract with
// Rust's `pick_level`. What these tests pin is the *order the rungs are drawn
// in* — Low on the left, Max on the right — and that the arrow keys walk the
// bar the way it looks.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import type { ModelInfo, ProviderView, Settings } from "../api";
import { get, set } from "../store";
import { Overlays } from "./Overlays";

const claude: ModelInfo = {
  id: "anthropic/claude-opus-5", name: "Claude Opus 5", provider: "anthropic", context: 1_000_000, output: 128_000,
  input_price: 0, output_price: 0, effort: true, input_types: ["text"], capabilities: [],
  reasoning_levels: ["low", "medium", "high", "xhigh", "max"], reasoning_param: "anthropic_effort",
  custom: false, enabled: true,
};

const provider = { id: "anthropic", name: "Anthropic", enabled: true, connected: true, has_key: true, account: null } as unknown as ProviderView;

beforeEach(() => {
  (window as unknown as { matchMedia?: unknown }).matchMedia ??= () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  (Element.prototype as unknown as { getAnimations?: unknown }).getAnimations ??= () => [];
  set({
    models: [claude], providers: [provider], accounts: [],
    settings: { routes: [], mcp: [], projects: [] } as unknown as Settings,
    home: { ...get().home, model: claude.id, effort: 0 },
    menu: "model", menuAnchor: new DOMRect(10, 10, 20, 20), pickFor: null,
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });

/** The effort bar's buttons, left to right, as drawn. */
const drawn = () => within(screen.getByRole("radiogroup", { name: "Thinking effort" })).getAllByRole("radio").map((b) => b.textContent);

describe("the effort bar in the model picker", () => {
  it("draws the rungs low to high, Max last", () => {
    render(<Overlays z={1} />);
    expect(drawn()).toEqual(["low", "medium", "high", "xhigh", "max"]);
  });

  it("marks the level the chat is on", () => {
    set((s) => ({ home: { ...s.home, model: claude.id, effort: 2 } }));
    render(<Overlays z={1} />);
    const on = within(screen.getByRole("radiogroup", { name: "Thinking effort" })).getAllByRole("radio").filter((b) => b.getAttribute("aria-checked") === "true");
    expect(on.map((b) => b.textContent)).toEqual(["high"]);
  });

  it("steps up with the right arrow and down with the left, the way the bar reads", async () => {
    // The chat starts on Low, the left end of the bar, so the right arrow has
    // somewhere to go: it walks right along the bar, towards more thinking.
    set((s) => ({ home: { ...s.home, model: claude.id, effort: 4 } }));
    render(<Overlays z={1} />);
    const bar = screen.getByRole("radiogroup", { name: "Thinking effort" });
    fireEvent.keyDown(bar, { key: "ArrowRight" });
    await waitFor(() => expect(get().home.effort).toBe(3));
    fireEvent.keyDown(bar, { key: "ArrowRight" });
    await waitFor(() => expect(get().home.effort).toBe(2));
    fireEvent.keyDown(bar, { key: "ArrowLeft" });
    await waitFor(() => expect(get().home.effort).toBe(3));
  });

  it("stops at the ends instead of running off the bar", async () => {
    set((s) => ({ home: { ...s.home, model: claude.id, effort: 4 } }));
    render(<Overlays z={1} />);
    const bar = screen.getByRole("radiogroup", { name: "Thinking effort" });
    fireEvent.keyDown(bar, { key: "ArrowLeft" });
    await waitFor(() => expect(get().home.effort).toBe(4));
  });

  it("stops at Max too, the right end of the bar", async () => {
    set((s) => ({ home: { ...s.home, model: claude.id, effort: 0 } }));
    render(<Overlays z={1} />);
    const bar = screen.getByRole("radiogroup", { name: "Thinking effort" });
    fireEvent.keyDown(bar, { key: "ArrowRight" });
    await waitFor(() => expect(get().home.effort).toBe(0));
  });
});
