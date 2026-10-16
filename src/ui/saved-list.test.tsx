/** The Saved prompts screen's project handling: which prompts it lists, which it
 *  steps back, and the order it puts them in. The switch is the sidebar's "show
 *  all chats" (`all_projects`) — one setting, so the two screens cannot disagree
 *  about whether a prompt from another folder is in view. */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import type { SavedPrompt, Settings } from "../api";
import { set } from "../store";
import { Saved, savedList, savedPromptIsHere, savedSortOf } from "./Saved";

const here = "D:/work/openleash", there = "D:/work/other";

const p = (id: string, project: string, over: Partial<SavedPrompt> = {}): SavedPrompt => ({
  id, text: `prompt ${id}`, project, model: "", route: "", assist: "necessary", perm: "turbo", effort: null,
  plan: false, ultra: false, ultra_wt: false, worktree: false, branch: "", agents: [], images: [],
  created_at: "2026-09-01T00:00:00Z", ...over,
});

/** The screen, with settings as the backend would hand them over. */
const render = (list: SavedPrompt[], project: string, allProjects: boolean, sort = "") => {
  set({ settings: { project, all_projects: allProjects, saved_sort: sort, saved_prompts: list } as unknown as Settings, view: "saved" });
  return renderToStaticMarkup(createElement(Saved));
};

/** Rows, cut at the next row's opening tag so a row's nested buttons don't end it early. */
const rows = (html: string) => html.split('<div class="savedrow').slice(1);

describe("saved prompts from other folders", () => {
  beforeEach(() => set({ view: "saved" }));

  it("hides them while show-all is off, and says how many there are", () => {
    const html = render([p("a", here), p("b", there), p("c", there)], here, false);
    expect(html).toContain("prompt a");
    expect(html).not.toContain("prompt b");
    expect(html).not.toContain("prompt c");
    expect(html).toContain("2 in other folders · show");
  });

  it("lists them stepped back once show-all is on, keeping the folder tag", () => {
    const html = render([p("a", here), p("b", there)], here, true);
    const [own, away] = rows(html);
    expect(own).not.toContain("elsewhere");
    expect(away).toContain("elsewhere");
    // The tag is the reason the row looks like it does, so it stays readable.
    expect(away).toContain("savedtag");
    expect(away).toContain("other</span>");
    expect(html).not.toContain("in other folder");
  });

  it("counts only what it lists, so a hidden prompt can't inflate the total", () => {
    expect(render([p("a", here), p("b", there)], here, false)).toContain("1 waiting to start");
    expect(render([p("a", here), p("b", there)], here, true)).toContain("2 waiting to start");
  });

  it("explains an empty column rather than claiming nothing was ever saved", () => {
    const html = render([p("b", there)], here, false);
    expect(html).toContain("Nothing parked for this folder");
    expect(html).not.toContain("Nothing saved yet");
  });

  it("still offers the screen's own empty state when nothing was ever saved", () => {
    expect(render([], here, false)).toContain("Nothing saved yet");
  });

  // Nothing to disagree with: a chat with no folder of its own counts as the
  // current one (see `chatProjectIsCurrent`), and so does a prompt, so with no
  // folder open none of them are hidden — and none get a folder tag either,
  // because loading one would not switch anywhere.
  it("hides and steps back nothing while no folder is open", () => {
    const html = render([p("a", there)], "", false);
    expect(html).toContain("prompt a");
    expect(html).not.toContain("elsewhere");
    expect(html).not.toContain("savedtag");
  });

  it("compares folders the way the sidebar does, not by name", () => {
    expect(savedPromptIsHere(p("a", "D:/WORK/OPENLEASH/"), here)).toBe(true);
    expect(savedPromptIsHere(p("a", "D:\\work\\openleash"), here)).toBe(true);
    expect(savedPromptIsHere(p("a", "D:/work/openleash-2"), here)).toBe(false);
    expect(rows(render([p("a", "d:/work/openleash/")], here, true))[0]).not.toContain("elsewhere");
  });
});

describe("saved prompt ordering", () => {
  beforeEach(() => set({ view: "saved" }));

  it("offers the control only while show-all is on", () => {
    expect(render([p("a", here)], here, false)).not.toContain("savedsort");
    const on = render([p("a", here)], here, true);
    expect(on).toContain("savedsort");
    // The control reads as the order currently in force.
    expect(on).toContain("As saved");
    expect(render([p("a", here)], here, true, "newest")).toContain("Newest first");
  });

  it("needs no prompt to sort before it is hidden", () => {
    expect(render([], here, true)).not.toContain("savedsort");
  });

  it("keeps the order the prompts were parked in by default", () => {
    const list = [p("a", here), p("b", there), p("c", here)];
    expect(savedList(list, here, true, "").map((x) => x.id)).toEqual(["a", "b", "c"]);
    expect(rows(render(list, here, true))).toHaveLength(3);
  });

  it("sorts newest first", () => {
    const list = [p("old", here, { created_at: "2026-01-01T00:00:00Z" }), p("new", here, { created_at: "2026-09-01T00:00:00Z" })];
    expect(savedList(list, here, true, "newest").map((x) => x.id)).toEqual(["new", "old"]);
  });

  it("sorts by prompt text", () => {
    const list = [p("b", here, { text: "zebra" }), p("a", here, { text: "Apple" }), p("c", here, { text: "mango" })];
    // Case-insensitive, so "Apple" leads rather than sorting after the capitals rule.
    expect(savedList(list, here, true, "az").map((x) => x.id)).toEqual(["a", "c", "b"]);
  });

  // "pushed at the bottom": this folder's own prompts stay on top, the other
  // folders' follow in folder order, and each group keeps the parked order.
  it("pushes the other folders to the bottom, grouped", () => {
    const list = [p("t1", there), p("h1", here), p("z1", "D:/work/zzz"), p("h2", here), p("n1", "")];
    expect(savedList(list, here, true, "folder").map((x) => x.id)).toEqual(["h1", "h2", "n1", "t1", "z1"]);
  });

  // The order is the user's own setting, so it is honoured here too: hiding the
  // control is not a reason to silently reorder what they are looking at.
  it("applies a stored order even where the control is not offered", () => {
    const list = [p("old", here, { created_at: "2026-01-01T00:00:00Z" }), p("new", here, { created_at: "2026-09-01T00:00:00Z" })];
    expect(savedList(list, here, false, "newest").map((x) => x.id)).toEqual(["new", "old"]);
  });

  it("falls back to the stored order for a value it does not know", () => {
    expect(savedSortOf(undefined)).toBe("");
    expect(savedSortOf("")).toBe("");
    expect(savedSortOf("by-size")).toBe("");
    expect(savedSortOf("folder")).toBe("folder");
    expect(savedList([p("a", here), p("b", here)], here, true, savedSortOf("by-size")).map((x) => x.id)).toEqual(["a", "b"]);
  });
});
