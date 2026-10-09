// @vitest-environment jsdom
// A long swarm used to read as a truncation: the panel mounted the first 80
// agents the backend sent, in spawn order, so a 300-agent ultrathread buried the
// dozen still working under everyone who had already finished — and the user saw
// "Showing the first 80 of 300 agents" with no way to see the rest at all.
//
// Two things fix that, and both are pinned here: the live agents sort ahead of
// the settled ones (an agent whose children are still working counts as live,
// however it ended itself), and everything that has ended folds behind a Done
// line that can be opened.
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import type { SubInfo, TaskSummary } from "../api";
import { buildTree, SwarmTree } from "./Session";

const sub = (id: string, status: string, extra: Partial<SubInfo> = {}): SubInfo =>
  ({ id, role: id, task: `work on ${id}`, status, meta: "", model: "m", serving: "", started: null, report: "", background: false, ...extra }) as SubInfo;

const task = (subs: SubInfo[], extra: Partial<TaskSummary> = {}): TaskSummary =>
  ({ id: "t1", title: "chat", status: "running", paused: null, model: "m", cwd: "D:/p", subs, ...extra }) as TaskSummary;

/** Every row the panel actually mounted, live section first, then Done. */
const rendered = () => document.querySelectorAll(".trow");
const text = () => document.body.textContent ?? "";

const doneLine = () => [...document.querySelectorAll(".tgroup .tg-line")].find((b) => b.textContent?.includes("Done"));

afterEach(() => cleanup());

describe("the swarm tree under a long run", () => {
  it("mounts the live agents first and folds the finished ones behind Done", () => {
    // 200 finished, then the 5 still working — the order a real swarm arrives in.
    const subs = [
      ...Array.from({ length: 200 }, (_, i) => sub(`d${i}`, "done")),
      ...Array.from({ length: 5 }, (_, i) => sub(`r${i}`, "running")),
    ];
    render(<SwarmTree task={task(subs)} />);

    // All five live agents are on screen even though the cap is 80 and 205
    // agents spawned: the ones still working are never the ones that get cut.
    for (let i = 0; i < 5; i++) expect(text()).toContain(`work on r${i}`);
    expect(rendered().length).toBe(5);
    // The finished ones are behind one line that counts them, not dropped.
    const line = doneLine()!;
    expect(line).toBeTruthy();
    expect(line.textContent).toContain("200 agents");
    expect(rendered().length, "nothing hides behind the header that is not a row behind it").toBe(5);

    // Opening it puts them back, in the tree they were launched in.
    act(() => { fireEvent.click(line); });
    expect(rendered().length).toBe(205);
    expect(text()).toContain("work on d0");
  });

  it("keeps a parent visible while any of its children are still working", () => {
    // The parent finished and reported, but it launched a child that is still
    // going, so its row still says something. Folding it away would hide the
    // branch the live work is hanging off.
    const { live, quiet } = buildTree([
      sub("p", "done", { parent: "" }),
      sub("c", "running", { parent: "p", depth: 2 }),
      sub("over", "done", { parent: "" }),
    ]);
    expect(live.map((n) => n.sub.id)).toEqual(["p"]);
    expect(live[0]!.kids.map((k) => k.sub.id)).toEqual(["c"]);
    expect(quiet.map((n) => n.sub.id)).toEqual(["over"]);

    render(<SwarmTree task={task([sub("p", "done"), sub("c", "running", { parent: "p", depth: 2 }), sub("over", "done")])} />);
    expect(text()).toContain("work on p");
    expect(text()).toContain("work on c");
    expect(text()).not.toContain("work on over");
    expect(doneLine()!.textContent).toContain("1 agent");
  });

  it("says what it left out when the live swarm alone overflows the cap", () => {
    // 120 live agents: more than the panel reads, and nothing settled to fold.
    const subs = Array.from({ length: 120 }, (_, i) => sub(`r${i}`, "running"));
    render(<SwarmTree task={task(subs)} />);
    expect(rendered().length).toBe(80);
    expect(text()).toContain("80 live of 120 shown");
    // No Done line with nothing behind it.
    expect(doneLine()).toBeUndefined();
  });

  it("leaves a small swarm alone — no Done line until something has finished", () => {
    const subs = [sub("a", "running"), sub("b", "running", { parent: "a", depth: 2 })];
    const { rerender } = render(<SwarmTree task={task(subs)} />);
    expect(rendered().length).toBe(2);
    expect(doneLine()).toBeUndefined();

    // One of them ends: now there is a Done line, and the other is still inline.
    // The parent stays, because it still has a live child under it.
    rerender(<SwarmTree task={task([sub("a", "done"), sub("b", "running", { parent: "a", depth: 2 })])} />);
    expect(text()).toContain("work on b");
    // The parent stays as the branch root, the child with it: neither is done
    // with, and folding either would strand the live one.
    expect(rendered().length).toBe(2);
    expect(doneLine()).toBeUndefined();
  });
});
