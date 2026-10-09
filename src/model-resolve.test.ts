// A sub-agent's model used to be stamped on it once, when it was spawned, and
// never re-read. Changing the chat's model mid-task therefore moved the main
// agent and left every sub-agent already running on the old one — and the panels
// that named a sub-agent's model read that same stored value, so they showed a
// model nothing was actually running on.
//
// `subModel` is the one place the UI asks that question, and it has to answer it
// the way the backend's `resolve_model` does. The precedence is pinned here
// because a divergence between the two is invisible until a task goes quiet: the
// panel names one model while the agent spends the money on another.
import { describe, expect, it } from "vitest";

import type { SubInfo, TaskSummary, UltraX } from "./api";
import { chatModel, subModel } from "./store";

const sub = (extra: Partial<SubInfo> = {}): SubInfo =>
  ({ id: "s1", role: "explore", task: "", status: "running", meta: "", model: "", serving: "", started: null, report: "", background: false, depth: 1, ...extra }) as SubInfo;

const task = (extra: Partial<TaskSummary> = {}): TaskSummary =>
  ({ id: "t1", title: "", status: "running", paused: null, model: "chat/model", pending: {}, subs: [], ...extra }) as TaskSummary;

const ladder = (models: string[]): UltraX => ({ layers: models.map((model) => ({ model, effort: null, fanout: 0 })), max_running: 0, max_total: 0, wt: false }) as UltraX;

describe("which model a sub-agent is on", () => {
  it("follows the chat, because that is what an empty model means", () => {
    // The regression itself: `sub.model` is empty for an agent nobody chose a
    // model for, and that is "on the chat's model", not "on no model".
    expect(subModel(task(), sub())).toBe("chat/model");
  });

  it("keeps a model picked for that agent", () => {
    expect(subModel(task(), sub({ model: "picked/model" }))).toBe("picked/model");
  });

  it("puts a model picked for the agent above the chat's and the ladder's", () => {
    const t = task({ model: "chat/model", ultra_x: ladder(["layer/1", "layer/2"]) });
    // depth 1 -> layer 1, and the agent's own choice still wins over it.
    expect(subModel(t, sub({ model: "picked/model", depth: 1 }))).toBe("picked/model");
    expect(subModel(t, sub({ depth: 1 }))).toBe("layer/1");
    expect(subModel(t, sub({ depth: 2 }))).toBe("layer/2");
  });

  it("prefers the chat over the ladder when a layer names no model", () => {
    const t = task({ model: "chat/model", ultra_x: ladder(["", ""]) });
    expect(subModel(t, sub({ depth: 1 }))).toBe("chat/model");
  });

  it("moves with a swap the user has queued for the chat", () => {
    // The queue is what the composer's `*` badge is: the chat is on its way to
    // another model, and an agent following the chat goes with it.
    const t = task({ model: "chat/model", pending: { model: "next/model" } });
    expect(subModel(t, sub())).toBe("next/model");
    expect(chatModel(t)).toBe("next/model");
    // An agent with a model of its own is not moved by the chat's swap.
    expect(subModel(t, sub({ model: "picked/model" }))).toBe("picked/model");
  });

  it("treats a chat with nothing queued as being on its live model", () => {
    expect(chatModel(task({ model: "chat/model", pending: {} }))).toBe("chat/model");
  });
});
