// @vitest-environment jsdom
// Settings → Checks carries three properties that a redesign can quietly lose:
//
//   1. A hook nobody has reviewed is *listed* and says it will not run. It is
//      never hidden: a hook the user wrote that silently disappears is worse
//      than one that visibly refuses, because the only way to find out is to
//      wait for it not to fire.
//   2. Trust is the only way to turn one on, and it is a separate write from
//      the settings save. A row that only offers a switch would let the user
//      "enable" a hook the backend will refuse to run anyway.
//   3. A project hook is read-only. It lives in the repo, so the only local
//      thing anyone can do to it is trust or revoke it — no toggle, no remove.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { api, type Hook, type HookView, type HooksStatus, type Settings } from "../api";
import { set } from "../store";
import { ChecksTab } from "./Hooks";

const view = (h: Hook, extra: Partial<HookView> = {}): HookView => ({ ...h, trusted: false, stale: false, changed_from: null, can_edit: true, ...extra });

const status = (hooks: HookView[]): HooksStatus => ({ project: "D:/work/app", hooks });

const show = (hooks: Hook[], views: HookView[]) => {
  set({ settings: { hooks } as unknown as Settings });
  vi.spyOn(api, "hooksStatus").mockResolvedValue(status(views));
  render(<ChecksTab />);
};

const text = () => document.body.textContent ?? "";

beforeEach(() => vi.spyOn(api, "hooksStatus").mockResolvedValue(status([])));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const unreviewed: Hook = { id: "h1", event: "post_tool", matcher: "edit_file", command: "npx prettier --write .", enabled: true };
const approved: Hook = { id: "h2", event: "pre_tool", matcher: "bash", command: "rm -rf /tmp/x", enabled: true, updated_input: true };
const project: Hook = { id: "p1", event: "post_setup_worktree", matcher: "", command: "./scripts/link-env.sh", enabled: true, source: "project", origin: "D:/work/app" };

describe("hooks that are waiting on review", () => {
  it("says so on the row and in a banner, and never drops the hook", async () => {
    show([unreviewed], [view(unreviewed)]);
    await vi.waitFor(() => expect(text()).toContain("Needs review"));
    expect(text()).toContain("Needs review — it will not run.");
    expect(text()).toContain("1 hook is waiting to be reviewed");
    // The hook is still listed and still editable: reviewable is not hidden.
    expect(text()).toContain("npx prettier --write .");
    expect(document.querySelector('[role="switch"][aria-label="npx prettier --write ."]')).toBeTruthy();
  });

  it("offers Trust and writes it through hooksTrust, not the settings save", async () => {
    const trust = vi.spyOn(api, "hooksTrust").mockResolvedValue({} as Settings);
    show([unreviewed], [view(unreviewed)]);
    const button = await vi.waitFor(() => {
      const b = [...document.querySelectorAll("button")].find((x) => x.textContent === "Trust");
      expect(b).toBeTruthy();
      return b!;
    });
    fireEvent.click(button);
    await vi.waitFor(() => expect(trust).toHaveBeenCalledWith("h1", "", true));
  });

  it("offers Revoke on a trusted hook, and it revokes rather than approves", async () => {
    const trust = vi.spyOn(api, "hooksTrust").mockResolvedValue({} as Settings);
    show([approved], [view(approved, { trusted: true })]);
    expect(await vi.waitFor(() => {
      const b = [...document.querySelectorAll("button")].find((x) => x.textContent === "Revoke");
      expect(b).toBeTruthy();
      return b!;
    })).toBeTruthy();
    expect(text()).toContain("Trusted");
    fireEvent.click([...document.querySelectorAll("button")].find((x) => x.textContent === "Revoke")!);
    await vi.waitFor(() => expect(trust).toHaveBeenCalledWith("h2", "", false));
  });

  it("shows a stale hook's approved command next to the current one", async () => {
    show([unreviewed], [view(unreviewed, { stale: true, changed_from: "npx prettier --write src" })]);
    await vi.waitFor(() => expect(text()).toContain("Changed since you approved it"));
    expect(text()).toContain("Changed since you approved it — it will not run.");
    expect(text()).toContain("You approved");
    expect(text()).toContain("npx prettier --write src");
    expect(text()).toContain("npx prettier --write .");
    // Both halves of the diff are on screen at once, which is the whole point.
    expect(text()).toContain("Now");
  });

  it("marks a pre_tool hook that may rewrite arguments", async () => {
    show([approved], [view(approved, { trusted: true })]);
    await vi.waitFor(() => expect(text()).toContain("rewrites args"));
  });
});

describe("hooks that come from the project", () => {
  it("renders them read-only: only a trust button, no toggle and no remove", async () => {
    const trust = vi.spyOn(api, "hooksTrust").mockResolvedValue({} as Settings);
    show([], [view(project, { source: "project", can_edit: false, origin: "D:/work/app" })]);
    await vi.waitFor(() => expect(text()).toContain("Hooks from this project"));
    fireEvent.click([...document.querySelectorAll("button")].find((b) => b.textContent === "Trust")!);
    await vi.waitFor(() => expect(trust).toHaveBeenCalledWith("p1", "D:/work/app", true));
    expect(text()).toContain("./scripts/link-env.sh");
    expect(text()).toContain(".openleash/hooks.json");
    // Read-only means read-only: a project hook is edited in the repo. Both the
    // enable toggle and the remove button are named for their hook, so a missing
    // pair is a missing affordance and not just a missing element.
    expect(document.querySelector('[role="switch"][aria-label="./scripts/link-env.sh"]')).toBeNull();
    expect(document.querySelector('[aria-label="Remove hook ./scripts/link-env.sh"]')).toBeNull();
    const names = [...document.querySelectorAll("button")].map((b) => b.textContent);
    expect(names).toContain("Trust");
  });

  it("matches hook status by (origin, id) when user and project hooks share an id", async () => {
    const projectCollision: Hook = { ...project, id: "h2", origin: "D:/work/app" };
    show([approved], [view(approved, { trusted: true }), view(projectCollision, { source: "project", origin: "D:/work/app", can_edit: false })]);
    await vi.waitFor(() => expect(text()).toContain("Hooks from this project"));
    const rows = [...document.querySelectorAll(".srowx")];
    const userRow = rows.find((row) => row.textContent?.includes(approved.command));
    const projectRow = rows.find((row) => row.textContent?.includes(projectCollision.command));
    expect(userRow?.textContent).toContain("Trusted");
    expect(userRow?.textContent).toContain("Revoke");
    expect(projectRow?.textContent).toContain("Needs review");
    expect(projectRow?.textContent).not.toContain("Revoke");
  });

  it("counts an unreviewed project hook in the banner even with no user hooks", async () => {
    show([], [view(project, { source: "project", can_edit: false })]);
    await vi.waitFor(() => expect(text()).toContain("1 hook is waiting to be reviewed"));
  });
});

describe("the composer", () => {
  it("hides the matcher for events with no subject and shows it for those with one", () => {
    set({ settings: { hooks: [] } as unknown as Settings });
    render(<ChecksTab />);
    const box = () => document.querySelector<HTMLInputElement>('input[placeholder^="tool regex"]');
    // The default event is post_tool, which does have a subject.
    expect(box()).toBeTruthy();
    fireEvent.click(document.querySelector(".dd")!);
    fireEvent.click([...document.querySelectorAll(".mrow")].find((r) => r.getAttribute("aria-selected") === "false" && r.textContent === "When a run ends")!);
    expect(box()).toBeNull();
  });
});
