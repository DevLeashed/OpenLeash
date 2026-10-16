// @vitest-environment jsdom
// A chat parked on a trigger (`wait_for_event`) is not waiting on you.
//
// The field report: the chat went red and stayed red. Parking a run on a timer,
// a CI run or a PR reports `status: "waiting"` with `waiting_kind: "wake"` —
// the shape a blocking question has — so every surface that colours a chat by
// its status painted the "Needs you" red: a pulsing dot and a red "Waiting"
// badge in the sidebar, a red alert glyph in the header and on the home list.
// Nothing was waiting on the user, and there was no action that could lift it.
//
// The rule pinned here: only a wait the user can end is red. A parked chat reads
// as `parked` — quiet, still, still labelled "Waiting" — which is what the
// sidebar badge already said in words.
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { cleanup, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
// The composer and the browser pane are not what this suite is about, and both
// read half the store: the header renders around them.
vi.mock("./Composer", () => ({ Composer: () => null, PendingStar: () => null }));
vi.mock("./BrowserPanel", () => ({ BrowserPanel: () => null }));

import { createElement } from "react";
import type { Settings, TaskSummary } from "../api";
import { set } from "../store";
import { Sidebar } from "./Chrome";
import { Home } from "./Home";
import { Session } from "./Session";
import { chatStatus, isParked, statusOf } from "./icons";
import { swarmGlow, swarmState } from "./session/Swarm";

const chat = (extra: Partial<TaskSummary> = {}): TaskSummary => ({
  id: "t1", title: "chat one", status: "waiting", waiting_kind: "wake", paused: null, unpaused: false, busy: 0,
  model: "anthropic/claude-opus-5", branch: "main", project: "D:/work/app",
  ultra: false, subs: [], archived: false, unread: false, step: "",
  pending: {}, todos: [], usage: { cost: 0, last_context: 0, cache_read: 0 }, context_window: 200_000,
  updated_at: "2026-09-26T00:00:00Z", touched_at: "2026-09-26T00:00:00Z",
  ...extra,
} as TaskSummary);

/** One chat in the folder the surfaces are showing. */
const parked = (extra: Partial<TaskSummary> = {}) => set({
  tasks: { t1: chat(extra) },
  settings: { project: "D:/work/app" } as unknown as Settings,
  view: "home", task: null, items: { t1: [] },
});

const dotColor = () => document.querySelector<HTMLElement>(".srow .dot")?.style.background ?? "";
const dot = () => document.querySelector<HTMLElement>(".srow .dot");
const badge = () => document.querySelector<HTMLElement>(".srow .badge");

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("a chat parked on a trigger is not a chat waiting on you", () => {
  it("tells the two waits apart, since both arrive as `waiting`", () => {
    // The whole bug in one place: the backend cannot send a separate status.
    expect(isParked(chat()), "parked on a trigger").toBe(true);
    expect(chatStatus(chat())).toBe("parked");
    expect(statusOf(chatStatus(chat()), false).text).toBe("Waiting");
    expect(isParked(chat({ waiting_kind: "question" })), "a question is not parked").toBe(false);
    expect(chatStatus(chat({ waiting_kind: "question" }))).toBe("waiting");
    expect(statusOf("waiting", false).text, "a blocking wait stays what it was").toBe("Needs you");
  });

  it("gives the sidebar dot the quiet colour instead of the red one", () => {
    parked();
    render(createElement(Sidebar));
    expect(dotColor(), "parked, not needs-you").toBe("var(--st-idle)");
    expect(dotColor()).not.toBe("var(--st-wait)");
  });

  it("stops the dot pulsing, because nothing is happening", () => {
    // The pulse promises work in flight. A parked chat has no turn running: the
    // turn ended when it parked.
    parked();
    render(createElement(Sidebar));
    expect(dot()!.className, "a parked chat must not animate").not.toContain("pulse");
  });

  it("keeps the Waiting badge but takes the red out of it", () => {
    parked();
    render(createElement(Sidebar));
    expect(badge()!.textContent).toBe("Waiting");
    expect(badge()!.className).toContain("quiet");
  });

  it("keeps unread blue from hiding a blocking question", () => {
    // The question remains the actionable state; the unread mark is secondary.
    parked({ waiting_kind: "question", unread: true });
    render(createElement(Sidebar));
    expect(dotColor()).toBe("var(--st-wait)");
    expect(badge()!.textContent).toBe("Answer");
  });

  it("shows a paused unread chat as paused, not done/unread blue", () => {
    parked({ status: "running", waiting_kind: null, unread: true, paused: { kind: "manual", reason: "Paused by you", since: "2026-09-26T00:00:00Z" } });
    render(createElement(Sidebar));
    expect(dotColor()).toBe("var(--st-pause)");
    expect(dot()!.className).not.toContain("pulse");
    expect(document.querySelector(".unread-mark"), "pause state takes priority over unread").toBeNull();
  });

  it("leaves a blocking question red and pulsing", () => {
    // The other half, and the one that must not regress: a chat holding a
    // question is still the reddest thing in the sidebar, and what the quiet
    // chip means is exactly that this is not that.
    parked({ waiting_kind: "question" });
    render(createElement(Sidebar));
    expect(dotColor()).toBe("var(--st-wait)");
    expect(dot()!.className).toContain("pulse");
    expect(badge()!.textContent).toBe("Answer");
    expect(badge()!.className).not.toContain("quiet");
  });

  it("never draws a parked chat with the needs-you alert glyph", () => {
    // Home and the session header both read the status straight off the summary,
    // which is where the red circle-alert came from. `var(--st-idle)` is the
    // parked colour; the alert glyph is the shape the red arrives in.
    parked();
    const home = renderToStaticMarkup(createElement(Home));
    expect(home, "the home row says Waiting").toContain("Waiting");
    expect(home).toContain("var(--st-idle)");
    expect(home, "and not with the alert").not.toContain("circle-alert");
  });

  it("gives the chat header the same quiet glyph", () => {
    // The header drew `task.status` straight, so the one icon in the chat read
    // as an alarm for a wait nobody could lift.
    (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver;
    parked();
    set({ view: "session", task: "t1", details: false, find: false });
    const { container } = render(createElement(Session));
    const header = container.querySelector<HTMLElement>(".shdr")!;
    expect(header, "the chat header rendered").toBeTruthy();
    expect(header.innerHTML).toContain("var(--st-idle)");
    expect(header.innerHTML).not.toContain("var(--st-wait)");
  });

  it("settles an ultrathread swarm that parked instead of reddening the header", () => {
    // The underline under the header and the ultrabadge take their colour from
    // the swarm state, which read the raw status. An ultrathread chat parked on
    // a trigger kept a red line under it for the whole wait.
    const t = chat({ ultra: true });
    expect(swarmState(t), "parked, not waiting-on-you").toBe("parked");
    expect(swarmGlow(swarmState(t)), "a parked swarm does not breathe").toBe(false);
    // And a swarm actually waiting on the user is untouched.
    expect(swarmState(chat({ ultra: true, waiting_kind: "approval" }))).toBe("waiting");
  });
});
