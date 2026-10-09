/// Saved prompts: parking a prompt from the new-task composer and loading it
/// back with its options. Kept in its own file because it needs its own copy of
/// the store's settings state, which would otherwise leak into ui.test.ts.
import { describe, expect, it, vi, beforeEach } from "vitest";

// The backend merges a settings patch into what's on disk and answers with the
// whole thing, so a write that only carries `saved_prompts` still returns the
// project it left alone. Modelling that matters: `loadPrompt` writes settings
// twice, and a mock that echoed back only the patch would drop the project.
let onDisk: Record<string, any> = {};
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async (cmd: string, args: any) => {
  if (cmd === "settings_update") {
    onDisk = { ...onDisk, ...args.patch };
    return { ...onDisk };
  }
  // `project_info` answers for real folders only: the path a prompt was saved
  // under can have been deleted since, and that has to be a refusal, not a
  // silent switch to a directory that isn't there.
  if (cmd === "project_info") return { exists: !args.path.includes("gone"), git: false, branch: "", branches: [], memory: [] };
  return null;
}) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import type { SavedPrompt, Settings } from "./api";
import { dropPrompt, get, loadPrompt, savePrompt, set } from "./store";

describe("saved prompts", () => {
  // Every path here reports through `flash`, which wants a window for its timer.
  beforeEach(() => {
    (globalThis as any).window ??= { setTimeout: (f: () => void, ms: number) => setTimeout(f, ms) };
    set({ settings: { project: "D:/work/app", saved_prompts: [] } as unknown as Settings, draft: "", attach: {}, view: "home", task: null });
  });
  const p = (extra: Partial<SavedPrompt> = {}): SavedPrompt => ({
    id: "s1", text: "ship the thing", project: "D:/work/app", model: "anthropic/claude-opus-5", route: "subs", assist: "necessary",
    perm: "turbo", effort: 1, plan: true, ultra: true, ultra_wt: false, worktree: true, branch: "feat", agents: ["explore", "general", "review"],
    images: [], created_at: "2026-09-26T00:00:00Z", ...extra,
  });
  const opts = { ...get().home, model: "anthropic/claude-opus-5", effort: 1, perm: "turbo" as const, assist: "necessary" as const, plan: true, ultra: true, worktree: true, branch: "feat", agents: ["explore", "general", "review"] };

  it("parks the prompt with every option the composer was set to, and clears the box", async () => {
    set({ home: opts, draft: "  ship the thing  ", attach: { "new-chat": ["data:image/png;base64,AAA"] } });
    await savePrompt();
    const s = get().settings?.saved_prompts;
    expect(s).toHaveLength(1);
    expect(s![0]).toMatchObject({ text: "ship the thing", model: "anthropic/claude-opus-5", effort: 1, perm: "turbo", assist: "necessary", plan: true, ultra: true, worktree: true, branch: "feat", images: ["data:image/png;base64,AAA"] });
    expect(get().draft).toBe("");
    expect(get().attach["new-chat"]).toEqual([]);
  });

  it("restores the options on load, and takes the prompt off the list", async () => {
    set((st) => ({ settings: { ...st.settings!, saved_prompts: [p()] } }));
    expect(await loadPrompt(p())).toBe(true);
    const h = get().home;
    expect([h.model, h.effort, h.perm, h.assist, h.plan, h.ultra, h.worktree, h.branch, h.route, h.agents]).toEqual(["anthropic/claude-opus-5", 1, "turbo", "necessary", true, true, true, "feat", "subs", ["explore", "general", "review"]]);
    expect(get().draft).toBe("ship the thing");
    expect(get().settings?.saved_prompts).toEqual([]);
  });

  it("opens the folder it was saved in rather than refusing, so Enter can't run it in the wrong one", async () => {
    set((st) => ({ settings: { ...st.settings!, saved_prompts: [p({ project: "D:/work/other" })] } }));
    expect(await loadPrompt(p({ project: "D:/work/other" }))).toBe(true);
    expect(get().settings?.project).toBe("D:/work/other");
    // And it is remembered, so the folder that arrived this way is in the project menu.
    expect(get().settings?.projects).toContain("D:/work/other");
    expect(get().draft).toBe("ship the thing");
    expect(get().settings?.saved_prompts).toEqual([]);
  });

  it("keeps a prompt whose folder is gone on the list instead of opening nothing", async () => {
    set((st) => ({ settings: { ...st.settings!, saved_prompts: [p({ project: "D:/work/gone" })] } }));
    expect(await loadPrompt(p({ project: "D:/work/gone" }))).toBe(false);
    expect(get().settings?.project).toBe("D:/work/app");
    expect(get().settings?.saved_prompts).toHaveLength(1);
    expect(get().draft).toBe("");
  });

  it("keeps the required subagents on when a saved set lost them", async () => {
    set({ home: { ...opts, agents: [] } });
    expect(await loadPrompt(p({ agents: [] }))).toBe(true);
    expect(get().home.agents).toEqual(["explore", "general"]);
  });

  it("deletes without loading", async () => {
    set((st) => ({ settings: { ...st.settings!, saved_prompts: [p(), p({ id: "s2" })] } }));
    await dropPrompt(p());
    expect(get().settings?.saved_prompts?.map((x) => x.id)).toEqual(["s2"]);
  });
});
