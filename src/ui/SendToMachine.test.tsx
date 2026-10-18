// @vitest-environment jsdom
// "Send to another machine": pick a saved SSH host, see what the other side
// answered, send, and see the result. The transport and the import's
// sanitizing are Rust (`remote::transfer`, `agent::transfer`) and tested
// there; these pin the dialog's states and the arguments it sends.
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null), save: vi.fn(async () => null) }));

import { codeWarning, SendDialog } from "./SendToMachine";

const HOST = { id: "h1", name: "Desk", target: "me@desk", connected: false, local_url: null, error: "" };
const calls: [string, unknown][] = [];
let online = true;
let sendFails = false;

beforeEach(() => {
  calls.length = 0;
  online = true;
  sendFails = false;
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    calls.push([cmd, args]);
    if (cmd === "ssh_hosts_list") return [HOST];
    if (cmd === "transfer_check") return { git: true, branch: "feat", dirty: 2, ahead: 1 };
    if (cmd === "transfer_open") return online
      ? { link: "L1", host_name: "Desk", online: true, reason: "", probe: { host: "desk", os: "Linux", home: "/home/me", projects: [], best: "/home/me/proj" } }
      : { link: "L1", host_name: "Desk", online: false, reason: "OpenLeash doesn't seem to be running on Desk.", probe: null };
    if (cmd === "transfer_send") {
      if (sendFails) throw "Desk couldn't import it: disk full";
      return online
        ? { status: "imported", message: "", remote: { task_id: "n1", title: "t", project: "/home/me/proj", mapped_by: "argument", warning: "", from_host: "me", moved: true, resumed: false } }
        : { status: "pending", message: "waiting in the imports folder on Desk", remote: null };
    }
    return null;
  });
});
afterEach(cleanup);

it("warns when the code won't travel with the chat", () => {
  expect(codeWarning({ git: true, branch: "feat", dirty: 2, ahead: 1 })).toMatch(/2 uncommitted changes and 1 unpushed commit on feat/);
  expect(codeWarning({ git: true, branch: "feat", dirty: 0, ahead: null })).toMatch(/no upstream/);
  expect(codeWarning({ git: true, branch: "feat", dirty: 0, ahead: 0 })).toBe("");
  expect(codeWarning({ git: false, branch: "", dirty: 3, ahead: null })).toBe("");
});

it("connects, suggests the matching folder, and transfers the chat", async () => {
  render(<SendDialog taskId="t1" title="Port the parser" running />);
  const host = await screen.findByText("Desk");
  expect((await screen.findByRole("note")).textContent).toMatch(/uncommitted/);
  // Nothing to send until a machine answers.
  expect((screen.getByRole("button", { name: "Copy" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(host);
  await screen.findByText(/Connected to Desk \(desk, Linux\)/);
  expect((screen.getByLabelText(/Project folder on Desk/) as HTMLInputElement).value).toBe("/home/me/proj");
  fireEvent.click(screen.getByRole("radio", { name: "Transfer" }));
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(screen.getByRole("button", { name: "Transfer" }));
  await screen.findByText("Transferred to Desk");
  expect(screen.getByText(/archived here/)).toBeTruthy();
  expect(calls.find(([c]) => c === "transfer_send")?.[1]).toEqual({ link: "L1", taskId: "t1", mode: "move", project: "/home/me/proj", resume: true });
});

it("offers to leave the chat when OpenLeash isn't running there", async () => {
  online = false;
  render(<SendDialog taskId="t1" title="x" running={false} />);
  fireEvent.click(await screen.findByText("Desk"));
  await screen.findByText(/isn't reachable on Desk/);
  expect((screen.getByRole("button", { name: "Browse…" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.queryByRole("checkbox")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Copy" }));
  await screen.findByText("Sent to Desk");
  expect(screen.getByText(/waiting in the imports folder/)).toBeTruthy();
});

it("keeps a transferred chat unarchived when the import isn't confirmed", async () => {
  online = false;
  render(<SendDialog taskId="t1" title="x" running={false} />);
  fireEvent.click(await screen.findByText("Desk"));
  await screen.findByText(/isn't reachable on Desk/);
  fireEvent.click(screen.getByRole("radio", { name: "Transfer" }));
  fireEvent.click(screen.getByRole("button", { name: "Transfer" }));
  await screen.findByText("Sent to Desk");
  expect(screen.queryByText(/was archived here/)).toBeNull();
  expect(screen.getByText(/not archived here/).textContent).toMatch(/archive it yourself/);
});

it("closes a link that opens after the dialog is gone", async () => {
  let resolveOpen: (v: unknown) => void = () => {};
  const base = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    if (cmd === "transfer_open") {
      calls.push([cmd, args]);
      return new Promise((r) => { resolveOpen = r; });
    }
    return base(cmd, args);
  });
  const errors = vi.spyOn(console, "error").mockImplementation(() => {});
  const { unmount } = render(<SendDialog taskId="t1" title="x" running={false} />);
  fireEvent.click(await screen.findByText("Desk"));
  await waitFor(() => expect(calls.some(([c]) => c === "transfer_open")).toBe(true));
  unmount();
  resolveOpen({ link: "LATE", host_name: "Desk", online: true, reason: "", probe: null });
  await waitFor(() => expect(calls).toContainEqual(["transfer_close", { link: "LATE" }]));
  expect(errors).not.toHaveBeenCalled();
  errors.mockRestore();
});

it("shows a failed send and stays ready to retry", async () => {
  sendFails = true;
  render(<SendDialog taskId="t1" title="x" running={false} />);
  fireEvent.click(await screen.findByText("Desk"));
  await screen.findByText(/Connected to Desk/);
  fireEvent.click(screen.getByRole("button", { name: "Copy" }));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toMatch(/disk full/));
  expect((screen.getByRole("button", { name: "Copy" }) as HTMLButtonElement).disabled).toBe(false);
});
