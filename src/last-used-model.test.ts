// @vitest-environment jsdom
// The new-task screen's model: the one the user last used.
//
// It used to be a choice that did not stick. `setOpts` on the home screen
// wrote the pick into the store and nowhere else, so it was gone on the next
// launch, and the screen came up on the model the build shipped as a default —
// `anthropic/claude-opus-5`, whatever the provider list actually offered. The
// user had to pick a model again for every new task.
//
// Two things have to be true for this to work, and both are pinned here:
//
//  1. Picking a model in the composer persists it, so the next launch starts
//     on it. (The backend writes it too, when a task is created.)
//  2. The screen comes up on a model that is *usable*. The stored default can
//     name a provider whose key was removed or a model that has since been
//     switched off, and preselecting a dead model is the same failure in a
//     different shape: the composer says "Pick a model" and sends nothing.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ isFocused: async () => true, isVisible: async () => true, isMinimized: async () => false, onFocusChanged: async () => () => {} }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import type { ModelInfo, ProviderView, Settings, TaskSummary } from "./api";
import { boot, disposeBoot, get, lastUsedModel, set } from "./store";
import { currentTask, setOpts } from "./ui/Composer";

const model = (id: string, provider: string): ModelInfo => ({
  id, name: id.split("/").pop() ?? id, provider, context: 200_000, output: 32_000,
  input_price: 0, output_price: 0, effort: false, input_types: ["text"], capabilities: [],
  reasoning_levels: [], reasoning_param: "none", custom: false, enabled: true,
});

const provider = (id: string, extra: Partial<ProviderView> = {}): ProviderView =>
  ({ id, name: id, enabled: true, connected: true, has_key: true, account: null, ...extra }) as unknown as ProviderView;

const opus = model("anthropic/claude-opus-5", "anthropic");
const sonnet = model("anthropic/claude-sonnet-5", "anthropic");
const openrouter = model("openrouter/some/model", "openrouter");

/** Settings as the backend would hold them, with the fields the app reads. */
const settings = (extra: Partial<Settings> = {}): Settings =>
  ({ model: opus.id, effort: 2, perm: "turbo", routes: [], recent_models: [], favorite_models: [], project: "D:/work/app", ...extra }) as unknown as Settings;

beforeEach(() => {
  set({
    view: "home", task: null,
    settings: settings(),
    models: [opus, sonnet, openrouter],
    providers: [provider("anthropic"), provider("openrouter")],
    accounts: [],
    home: { ...get().home, model: opus.id, effort: 2 },
  });
});

afterEach(() => { disposeBoot(); vi.resetAllMocks(); });

describe("the model a new task starts on", () => {
  it("keeps the model the user picked, so the next launch starts on it", async () => {
    // The backend merges a patch into what is on disk and answers with the
    // whole thing, so the mock has to do the same: a mock that echoed back
    // only the patch would drop the rest of the settings.
    const onDisk: { patch: Record<string, unknown> } = { patch: { model: opus.id, effort: 2, perm: "turbo" } };
    vi.mocked(invoke).mockImplementation(async (cmd, args) => {
      if (cmd === "settings_update") {
        const patch = (args as { patch: object }).patch as Record<string, unknown>;
        onDisk.patch = { ...onDisk.patch, ...patch };
        return { ...onDisk.patch };
      }
      return [];
    });

    expect(await setOpts({ model: sonnet.id })).toEqual({ ok: true, task: null });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { model: sonnet.id } });
    expect(get().settings?.model).toBe(sonnet.id);
    expect(get().home.model).toBe(sonnet.id);
  });

  it("keeps the reasoning level with the model when the task runs", async () => {
    // The effort is not written by the composer: the slider would make one
    // settings write per arrow key, and two quick presses would race each
    // other's answers. The backend records the level of the task that ran, so
    // the level that comes back is the one the user meant.
    vi.mocked(invoke).mockImplementation(async (cmd, args) => {
      if (cmd === "settings_update") return { ...(args as { patch: object }).patch };
      return [];
    });

    expect(await setOpts({ model: sonnet.id, effort: 1 })).toEqual({ ok: true, task: null });
    expect(invoke).toHaveBeenCalledWith("settings_update", { patch: { model: sonnet.id } });
    expect(get().home.effort).toBe(1);
  });

  it("does not change the new-task model when the write fails", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("write failed"));
    expect(await setOpts({ model: sonnet.id })).toEqual({ ok: false, task: null });
    expect(get().home.model).toBe(opus.id);
  });

  it("preselects the stored default, which is the model last used", () => {
    set({ settings: settings({ model: openrouter.id }) });
    expect(lastUsedModel()).toBe(openrouter.id);
  });

  it("falls back to a usable model when the stored default is no longer reachable", () => {
    // The provider's key is gone, so the stored default names a model that
    // cannot be called. Preselecting it is the same complaint as not
    // remembering the pick at all: the screen asks for a model every time.
    set({
      settings: settings({ model: openrouter.id, recent_models: [openrouter.id, sonnet.id] }),
      providers: [provider("anthropic")],
    });
    expect(lastUsedModel()).toBe(sonnet.id);
  });

  it("skips a model that has been switched off, even on a live provider", () => {
    set({
      settings: settings({ model: sonnet.id, recent_models: [sonnet.id, openrouter.id] }),
      models: [opus, { ...sonnet, enabled: false }, openrouter],
    });
    expect(lastUsedModel()).toBe(openrouter.id);
  });

  it("comes up on the last used model after a restart", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd) => {
      if (cmd === "app_boot") {
        return { settings: settings({ model: sonnet.id }), tasks: [], providers: [provider("anthropic"), provider("openrouter")], models: [opus, sonnet, openrouter], accounts: [], agents: [], routes: [] };
      }
      return [];
    });
    await boot();
    expect(get().home.model).toBe(sonnet.id);
  });

  it("comes up on a usable model after a restart, when the last used one is gone", async () => {
    // The stored default names a provider whose key has since been removed.
    // Preselecting it is the same complaint as not remembering the pick: the
    // composer says "Pick a model" and the user picks one again, every time.
    vi.mocked(invoke).mockImplementation(async (cmd) => {
      if (cmd === "app_boot") {
        return {
          settings: settings({ model: openrouter.id, recent_models: [openrouter.id, sonnet.id] }),
          tasks: [], providers: [provider("anthropic")], models: [opus, sonnet, openrouter], accounts: [], agents: [], routes: [],
        };
      }
      return [];
    });
    await boot();
    expect(get().home.model).toBe(sonnet.id);
  });
});

describe("the Settings picker edits the default, not the last chat", () => {
  // "Default for new tasks" in Settings → Models opens the model picker. The
  // picker resolved its target from "any view but home", so in Settings it
  // found the last chat still in `s.task` and edited *that* instead: the
  // default came up with the wrong tick, and picking a model retargeted a
  // chat the user was not looking at. The default is what the control says it
  // changes, so that is what it has to change.
  const chat = (model: string): TaskSummary => ({
    id: "t1", title: "a chat", status: "stopped", paused: null, unpaused: false, busy: 0,
    model, pending: {}, branch: "main", project: "D:/work/app", ultra: false, subs: [],
    archived: false, unread: false, updated_at: "", touched_at: "", agents: ["explore", "general"],
    perm: "turbo", effort: 2, plan: false, assist: "default", route: "",
  } as unknown as TaskSummary);

  beforeEach(() => {
    set({
      // The chat is on Opus; the new-task default is Sonnet. Different, so the
      // two targets are distinguishable.
      view: "settings", task: "t1", tasks: { t1: chat(opus.id) },
      home: { ...get().home, model: sonnet.id },
    });
  });

  it("does not treat the last chat as the target in Settings", () => {
    expect(currentTask()).toBeNull();
  });

  it("still targets the chat in a chat", () => {
    set({ view: "session" });
    expect(currentTask()?.id).toBe("t1");
  });
});
