// @vitest-environment jsdom
// "Open over SSH…": pick a saved host (or add one), test it, browse its
// folders with the shared picker, and open the pick as ssh://<id>/<path>.
// The connection, probe and listing are Rust's (remote/ssh_projects.rs) and
// tested there; these pin the dialog's states and the URI it opens.
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null), save: vi.fn(async () => null) }));

import type { Settings } from "../api";
import { get, set } from "../store";
import { OpenProjectDialogs, openOverSsh, useOpenSources } from "./OpenProject";

const BOX = { id: "a1b2c3d4e5", name: "Dev box", target: "me@devbox", connected: false, local_url: null, error: "" };
const OK = { home: "/home/me", os: "Linux", has_git: true, has_rg: false, has_bash: true };
const calls: [string, unknown][] = [];
let hosts = [BOX];
let testResult: (() => unknown) = () => OK;

beforeEach(() => {
  calls.length = 0;
  hosts = [BOX];
  testResult = () => OK;
  set({ settings: { project: "", projects: [] } as unknown as Settings, openFrom: null, sshHosts: null, trustPrompt: false, view: "home" });
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    calls.push([cmd, args]);
    const a = args as Record<string, unknown>;
    if (cmd === "ssh_hosts_list") return hosts;
    if (cmd === "ssh_host_save") {
      hosts = [...hosts, { ...BOX, id: "new0000001", name: a.name as string, target: a.target as string }];
      return hosts;
    }
    if (cmd === "ssh_host_test") return testResult();
    if (cmd === "ssh_list_dir") {
      const p = (a.path as string | null) ?? "/home/me";
      return { path: p, parent: p === "/" ? null : "/home", roots: ["/home/me", "/"], truncated: false,
        entries: p === "/home/me" ? [{ name: "app", path: "/home/me/app", dir: true }] : [] };
    }
    if (cmd === "project_info") return { exists: true, decided: true, trusted: true, git: true, branch: "main", branches: [], memory: [], docs: [] };
    if (cmd === "settings_update") return { ...get().settings, ...(a as { patch: object }).patch };
    return [];
  });
});
afterEach(cleanup);

it("is offered in the shared open-project list", () => {
  let ids: string[] = [];
  function Probe() { ids = useOpenSources(false).map((s) => s.id); return null; }
  render(<Probe />);
  expect(ids).toContain("ssh");
  openOverSsh();
  expect(get().openFrom).toBe("ssh");
});

it("lists saved hosts, tests one, browses, and opens the ssh:// URI", async () => {
  set({ openFrom: "ssh" });
  render(<OpenProjectDialogs />);
  fireEvent.click(await screen.findByText("Dev box"));
  await screen.findByText(/Connected · Linux/);
  expect(screen.getByText("/home/me")).toBeTruthy();
  expect(screen.getByText(/Not installed there: rg/)).toBeTruthy();
  expect(calls).toContainEqual(["ssh_host_test", { id: "a1b2c3d4e5" }]);
  fireEvent.click(screen.getByRole("button", { name: "Browse folders" }));
  // The shared picker, listing through the SSH host, starting at its home.
  fireEvent.click(await screen.findByText("app"));
  expect(calls).toContainEqual(["ssh_list_dir", { id: "a1b2c3d4e5", path: "/home/me", dirsOnly: true }]);
  expect(screen.getByText("On Dev box over SSH")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Choose" }));
  await waitFor(() => expect(get().settings?.project).toBe("ssh://a1b2c3d4e5/home/me/app"));
  expect(calls).toContainEqual(["project_info", { path: "ssh://a1b2c3d4e5/home/me/app" }]);
  expect(get().openFrom).toBe(null);
});

it("a failed test shows the error and can be retried", async () => {
  testResult = () => { throw "Key authentication to me@devbox failed. … password prompts aren't supported"; };
  set({ openFrom: "ssh" });
  render(<OpenProjectDialogs />);
  fireEvent.click(await screen.findByText("Dev box"));
  expect((await screen.findByRole("alert")).textContent).toContain("password prompts aren't supported");
  expect(screen.queryByRole("button", { name: "Browse folders" })).toBeNull();
  testResult = () => OK;
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  await screen.findByRole("button", { name: "Browse folders" });
  expect(calls.filter(([c]) => c === "ssh_host_test")).toHaveLength(2);
});

it("shows a spinner while testing", async () => {
  let release: (v: unknown) => void = () => {};
  testResult = () => new Promise((r) => { release = r; });
  set({ openFrom: "ssh" });
  render(<OpenProjectDialogs />);
  fireEvent.click(await screen.findByText("Dev box"));
  expect(await screen.findByText(/Connecting and checking/)).toBeTruthy();
  release(OK);
  await screen.findByText(/Connected · Linux/);
});

it("adds a host inline and tests it straight away", async () => {
  hosts = [];
  set({ openFrom: "ssh" });
  render(<OpenProjectDialogs />);
  await screen.findByText(/No SSH hosts saved yet/);
  fireEvent.change(screen.getByLabelText("Host name"), { target: { value: "Build" } });
  fireEvent.change(screen.getByLabelText("SSH destination"), { target: { value: "me@build" } });
  fireEvent.click(screen.getByRole("button", { name: "Add" }));
  await screen.findByText(/Connected · Linux/);
  expect(calls).toContainEqual(["ssh_host_save", { id: null, name: "Build", target: "me@build" }]);
  expect(calls).toContainEqual(["ssh_host_test", { id: "new0000001" }]);
  expect(get().sshHosts?.map((h) => h.name)).toEqual(["Build"]);
});

it("links to Settings → Remote", async () => {
  set({ openFrom: "ssh" });
  render(<OpenProjectDialogs />);
  fireEvent.click(screen.getByText("Manage hosts in Settings → Remote"));
  expect(get().openFrom).toBe(null);
  expect(get().view).toBe("settings");
  expect(get().settingsTab).toBe("remote");
});

it("names an SSH project by folder and saved host name, with a server icon", async () => {
  const { ProjectName } = await import("./ProjectName");
  render(<span data-testid="n"><ProjectName path="ssh://a1b2c3d4e5/home/me/app" /></span>);
  await waitFor(() => expect(screen.getByTestId("n").textContent).toBe("app · Dev box"));
  expect(screen.getByLabelText("SSH")).toBeTruthy();
  expect(calls.filter(([c]) => c === "ssh_hosts_list")).toHaveLength(1);
  cleanup();
  render(<span data-testid="n"><ProjectName path="D:\work\app" /></span>);
  expect(screen.getByTestId("n").textContent).toBe("app");
  expect(screen.queryByLabelText("SSH")).toBeNull();
});
