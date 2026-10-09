// A model colour was a 9px native colour input on one dot in the /models picker,
// and the default for anything matching `claude|opus|sonnet|haiku` was a literal
// orange in the frontend. Two failures, both of them "I can't set a model's
// colour":
//
//  1. Only subagents had a palette to pick from. A model's colour meant opening
//     the OS colour wheel and dialing a value by eye, so the feature that looked
//     richest on models was the one you could barely reach.
//
//  2. The default keyed off the *name* rather than the provider. Every Claude
//     model painted the same orange no matter who served it — including one
//     reached through OpenRouter — and a brand-new custom model had no colour at
//     all, because the id it was being typed under had nothing to match.
//
// The default is now `ProviderView.color`, which the backend already sends, so
// Anthropic keeps its orange as real data rather than as a second copy of the
// palette in the frontend that can drift from it.
//
// One thing is *not* the provider's colour: a model being added. That one draws
// a random swatch (`randomAccent`), because the provider default made every
// model added under a provider open on the same hue — see the block at the end.
import { describe, expect, it } from "vitest";

import type { ProviderView, Settings } from "./api";
import { modelColor, set } from "./store";
import { ACCENTS, randomAccent } from "./ui/primitives";

const prov = (id: string, color: string): ProviderView =>
  ({ id, color }) as ProviderView;

const settings = (o: Partial<Settings>): Settings => o as Settings;

/** Models, providers and settings are module-level state in the store, so each case sets up its own. */
const withWorld = (providers: ProviderView[], model_colors: Record<string, string> = {}) =>
  set({ providers, models: [], settings: settings({ model_colors }) });

describe("a model's default colour", () => {
  it("is its provider's brand colour, not a Claude literal", () => {
    withWorld([prov("anthropic", "#e8967a"), prov("openrouter", "#b69cff")]);
    // The bug, exactly: the same model name, the same orange, regardless of
    // which provider is actually going to answer the request.
    expect(modelColor("anthropic/claude-opus-5")).toBe("#e8967a");
    expect(modelColor("openrouter/anthropic/claude-opus-5")).toBe("#b69cff");
    // And nothing is hardcoded to Anthropic's brand orange any more.
    expect(modelColor("anthropic/claude-opus-5")).not.toBe("#d97757");
  });

  it("gives every provider its own colour, so models read apart at a glance", () => {
    withWorld([
      prov("anthropic", "#e8967a"), prov("openai", "#e4e4e7"),
      prov("google", "#8ab4ff"), prov("ollama", "#86efac"),
    ]);
    const seen = ["anthropic/claude-opus-5", "openai/gpt-6", "google/gemini-3-pro", "ollama/qwen3-coder"]
      .map((id) => modelColor(id));
    expect(new Set(seen).size, "two models of the same family must not read the same").toBe(seen.length);
  });

  it("falls back to the app violet for a model whose provider we don't know", () => {
    // A custom endpoint, a provider added by a newer build, a model in a saved
    // chat from a provider since removed: it still has to paint.
    withWorld([prov("anthropic", "#e8967a")]);
    expect(modelColor("some-unknown-provider/model")).toBe("var(--violet)");
    expect(modelColor("")).toBe("var(--violet)");
  });

  it("yours wins over the provider's", () => {
    withWorld([prov("anthropic", "#e8967a")], { "anthropic/claude-opus-5": "#4ade80" });
    expect(modelColor("anthropic/claude-opus-5")).toBe("#4ade80");
  });

  it("a route takes its first step's colour", () => {
    set({
      providers: [prov("anthropic", "#e8967a")],
      models: [],
      settings: settings({
        model_colors: { "anthropic/claude-opus-5": "#4ade80" },
        routes: [{ id: "r", name: "R", steps: ["anthropic/claude-opus-5", "openai/gpt-6"], heads: [], all: false, on_exhausted: "pause" }],
      }),
    });
    expect(modelColor("route/r")).toBe("#4ade80");
  });
});

describe("picking a model colour", () => {
  it("offers a palette, like subagents do", () => {
    // The feature that was missing. A model colour had no palette at all, so
    // this is the list a user now has to choose from.
    expect(ACCENTS.length).toBeGreaterThanOrEqual(8);
    expect(new Set(ACCENTS).size, "no duplicate swatches").toBe(ACCENTS.length);
    expect(ACCENTS.every((c) => /^#[0-9a-f]{6}$/.test(c)), "all values are real colours").toBe(true);
  });

  it("every swatch a model can take is one the accent renderer can actually use", () => {
    // `modelColor` output goes straight into `background`, `box-shadow` and a
    // `color-mix`, so a non-hex value would silently paint nothing on half the
    // surfaces. The grid and the renderer have to agree on the format.
    for (const c of ACCENTS) {
      withWorld([prov("anthropic", "#e8967a")], { "anthropic/claude-opus-5": c });
      expect(modelColor("anthropic/claude-opus-5")).toBe(c);
    }
  });

  it("a model the store has never heard of still resolves a colour", () => {
    // Stats panels and old chats name models that are no longer configured.
    // Nothing may throw or return undefined for them.
    withWorld([]);
    for (const id of ["gone/old-model", "route/gone", "openrouter/x"]) {
      expect(typeof modelColor(id)).toBe("string");
      expect(modelColor(id).length).toBeGreaterThan(0);
    }
  });
});

describe("the colour a newly added model opens on", () => {
  // The bug: the accent defaulted to the provider's colour, so every model
  // added under one provider came in the same hue and the picker's dots were a
  // column of identical circles. The provider is already a column in that list,
  // so the dot was not earning anything — except by making models read apart.
  it("is a real swatch, not the provider colour the picker would otherwise show", () => {
    for (let i = 0; i < 200; i++) {
      const c = randomAccent("#b69cff");
      expect(ACCENTS, "the colour comes from the palette the user is offered").toContain(c);
      expect(c, "and is not the provider colour it replaced").not.toBe("#b69cff");
    }
  });

  it("varies, so a second model does not open on the first one's colour", () => {
    // Not a flake test: `randomAccent` walks 16 swatches from a random offset, so
    // 200 draws colliding on one value is not a thing that can happen.
    expect(new Set(Array.from({ length: 200 }, () => randomAccent())).size).toBeGreaterThan(1);
  });

  it("never lands on a colour already in use", () => {
    // The dots the user can already see in the list behind the dialog. Handing a
    // new model one of those hides it among them, which is the same failure as
    // the provider default in a narrower form.
    const taken = ACCENTS.slice(0, 15);
    for (let i = 0; i < 100; i++) {
      expect(taken).not.toContain(randomAccent("#33d6ff", taken));
    }
  });

  it("returns a colour with the palette all taken rather than nothing", () => {
    // Every swatch in use needs 16 handed out and there are 16. Returning nothing
    // here would fall through to the provider colour — the original bug — so the
    // one case that cannot satisfy "unused" still has to paint. It also has to be
    // a swatch the picker can show as selected, which is why this asserts on
    // `ACCENTS` and not just on "not empty".
    expect(ACCENTS).toContain(randomAccent("#33d6ff", ACCENTS));
  });
});
