// @vitest-environment jsdom
// `model-color.test.ts` pins `randomAccent` on its own. This one pins the half
// that decides what the user actually sees: that the colour is *written*, not
// just drawn.
//
// The bug, end to end. A model added under a provider had no accent of its own,
// so it fell back to `ProviderView.color` — every model added under one provider
// came in the same hue and the picker's dots were a column of identical circles.
// Drawing a random swatch was not enough on its own: `modelColor` is a pure
// read, and the dialog only ever persisted a colour the user had clicked. A
// random default that was never saved would look fixed in the dialog and still
// resolve to the provider colour in the picker, the sidebar and the spinner —
// the same wall of dots with an extra step.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));

import { api, type ProviderView, type Settings } from "./api";
import { modelColor, set } from "./store";
import { ModelsTab } from "./ui/ModelManager";
import { ACCENTS } from "./ui/primitives";

/** The provider colour the new model's default must not come out as. */
const PROVIDER = "#b69cff";
const MODEL = "openrouter/some-new-model";

const world = (extra: Partial<Settings> = {}) => {
  set({
    providers: [{ id: "openrouter", name: "OpenRouter", color: PROVIDER, kind: "openai", custom: false, connected: true, has_key: true, enabled: true, local: false }] as ProviderView[],
    models: [],
    accounts: [],
    tasks: {},
    settings: { project: "D:/work/app", model_colors: {}, routes: [], ...extra } as unknown as Settings,
    home: { model: "openrouter/x", effort: 3, plan: false, ultra: false, ultra_wt: false, ultra_x: null, perm: "ask", worktree: false, branch: "", subagents: true, assist: "default", agents: [], route: "" } as never,
  });
};

/** The `model_colors` map of the last settings write, or null if none was made. */
const written = (): Record<string, string> | null => {
  const calls = (api.settings as unknown as { mock: { calls: [Record<string, unknown>][] } }).mock.calls;
  return (calls.at(-1)?.[0]?.model_colors ?? null) as Record<string, string> | null;
};

/** Open Settings → Models, expand the provider, open its Add model dialog. */
const openAddModel = () => {
  render(<ModelsTab />);
  fireEvent.click(document.querySelector(".pcard .srowx")!);
  const add = [...document.querySelectorAll("button")].find((b) => b.textContent?.includes("Add model"));
  if (!add) throw new Error(`no Add model button; got ${JSON.stringify([...document.querySelectorAll("button")].map((b) => b.textContent))}`);
  fireEvent.click(add);
  return document.querySelector(".modal") as HTMLElement;
};

/** Type an id and save, then wait for the write that persists the accent. */
const saveModel = async (id: string) => {
  const modal = openAddModel();
  fireEvent.change(modal.querySelector("input.mono")!, { target: { value: id } });
  fireEvent.click([...modal.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Save")!);
  await vi.waitFor(() => expect(written(), "the accent was persisted, not just drawn").not.toBeNull());
  return written()!;
};

beforeEach(() => {
  world();
  // `saveSettings` publishes whatever the backend hands back, so a `null` reply
  // would blank the store mid-render and take `modelInfo` down with it. Echo the
  // patch instead, which is also closer to what the real command returns.
  vi.spyOn(api, "settings").mockImplementation(async (patch) => patch as Settings);
  vi.spyOn(api, "modelSave").mockResolvedValue([]);
  vi.spyOn(api, "providerModels").mockResolvedValue([]);
});

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("adding a model", () => {
  it("saves an accent of its own rather than leaving it on the provider", async () => {
    const colors = await saveModel("some-new-model");
    expect(Object.keys(colors), "keyed by the full model id").toEqual([MODEL]);
    expect(ACCENTS, "a swatch the picker can show as selected").toContain(colors[MODEL]);
    expect(colors[MODEL], "not the provider colour this replaced").not.toBe(PROVIDER);
  });

  it("shows the accent it is about to save, and marks it in the grid", () => {
    // The swatch the dialog previews has to be the one it writes. If the grid
    // showed the provider's colour as selected, the user would save a colour the
    // dialog had said it was not choosing.
    const modal = openAddModel();
    const on = [...modal.querySelectorAll(".cpicker .swatch")].filter((s) => s.classList.contains("on"));
    expect(on.length, "exactly one swatch reads as selected").toBe(1);
    expect(on[0]!.getAttribute("aria-label"), "a real swatch, not the provider").toMatch(/^#[0-9a-f]{6}$/);
    expect(on[0]!.getAttribute("aria-label")).not.toBe(PROVIDER);
  });

  it("does not reuse a colour already set on another model", async () => {
    // The dots already in the list behind the dialog. Handing the new model one
    // of those hides it among them — the original failure in a narrower form.
    world({ model_colors: { "openrouter/gpt-6": "#f472b6" } });
    const colors = await saveModel("some-new-model");
    expect(colors["openrouter/gpt-6"], "the colour already set survives the write").toBe("#f472b6");
    expect(colors[MODEL]).not.toBe("#f472b6");
  });

  it("lets the user pick a different colour instead", async () => {
    // The drawn swatch is a starting point, not a decision: a click on another
    // one has to win, or this would be a colour you cannot change.
    const modal = openAddModel();
    const drawn = modal.querySelector(".cpicker .swatch.on")!.getAttribute("aria-label");
    const wanted = ACCENTS.find((c) => c !== PROVIDER && c !== drawn)!;
    fireEvent.click([...modal.querySelectorAll(".cpicker .swatch")].find((s) => s.getAttribute("aria-label") === wanted)!);
    fireEvent.change(modal.querySelector("input.mono")!, { target: { value: "some-new-model" } });
    fireEvent.click([...modal.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Save")!);
    await vi.waitFor(() => expect(written()).not.toBeNull());
    expect(written()![MODEL]).toBe(wanted);
  });

  it("resets back to the provider's colour on request", async () => {
    // The Reset affordance still has to mean something, or there is no way back
    // to the old default once a random one has been drawn. Clearing it writes
    // nothing at all: the map is already empty, and writing an empty map would
    // be a pointless round-trip. `modelColor` then falls through to the provider,
    // which is exactly what "no accent of its own" means.
    const modal = openAddModel();
    fireEvent.click(modal.querySelector(".cpicker-reset")!);
    expect(modal.querySelector(".cpicker .swatch.on")!.getAttribute("aria-label"), "back to the provider swatch").toBe(PROVIDER);
    fireEvent.change(modal.querySelector("input.mono")!, { target: { value: "some-new-model" } });
    fireEvent.click([...modal.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Save")!);
    await vi.waitFor(() => expect(api.modelSave, "the model itself is still saved").toHaveBeenCalled());
    expect(written(), "no accent written for a reset model").toBeNull();
    // And the store is left such that this model reads as the provider's.
    expect(modelColor(MODEL)).toBe(PROVIDER);
  });

  it("paints the saved model in its own colour, not the provider's", () => {
    // The end of the chain: `modelColor` is what the picker dot, the sidebar dot
    // and the working spinner all read. A colour that was saved but not resolved
    // here would leave every surface still on the provider's hue.
    world({ model_colors: { [MODEL]: "#fb923c" } });
    expect(modelColor(MODEL)).toBe("#fb923c");
    expect(modelColor(MODEL)).not.toBe(PROVIDER);
  });

  it("still leaves a model with no accent of its own on the provider's colour", () => {
    // The fallback this change deliberately did not remove: a model added before
    // this build, or one whose colour the user reset, has to keep painting.
    world();
    expect(modelColor("openrouter/added-by-an-older-build")).toBe(PROVIDER);
  });
});