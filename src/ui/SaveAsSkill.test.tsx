// @vitest-environment jsdom
// "Save as skill…": a user-only path from a chat to a SKILL.md. The drafting,
// redaction and writing are Rust (`skill_draft_from_chat`,
// `skill_save_from_chat`) and tested there; these pin what the dialog sends,
// when it sends it, and the confirmations it asks for before overwriting a
// skill or saving text that still looks like it holds a secret.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn() }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null), save: vi.fn(async () => null) }));

import type { SkillDraft, SkillSaveResult, TaskSummary } from "../api";
import { get, set } from "../store";
import { openSaveAsSkill, SaveAsSkill, skillNameProblem, skillPath } from "./SaveAsSkill";
import { Sidebar } from "./Chrome";

const DRAFT: SkillDraft = {
  name: "port-parser", description: "Port a parser to the new AST and when to do it.", body: "# Steps\n1. Read the grammar.",
  warnings: ["Redacted 2 API-key-looking strings (sk-…)"], model: "anthropic/claude-opus-5",
  project: "D:\\repo", project_trusted: true, user_dir: "C:\\Users\\x\\.openleash\\skills", project_dir: "D:\\repo\\.openleash\\skills",
};
const SAVED: SkillSaveResult = { saved: true, exists: false, secrets: [], path: "C:\\Users\\x\\.openleash\\skills\\port-parser\\SKILL.md", note: "" };
const TASK = { id: "t1", title: "Port the parser", model: "anthropic/claude-opus-5", pending: {}, status: "idle", archived: false, pinned: false, project: "", order: 0, touched_at: "", updated_at: "" } as unknown as TaskSummary;

const calls: [string, unknown][] = [];
let draft: SkillDraft = DRAFT;
/** Answers to successive `skill_save_from_chat` calls; the last one repeats. */
let saves: (SkillSaveResult | string)[] = [SAVED];

const saveReqs = () => calls.filter(([c]) => c === "skill_save_from_chat").map(([, a]) => (a as { req: Record<string, unknown> }).req);

beforeEach(() => {
  calls.length = 0;
  draft = DRAFT;
  saves = [SAVED];
  set({ tasks: { t1: TASK }, skillFrom: null, skills: [], toast: null, settings: { project: "D:\\repo" } as never });
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    calls.push([cmd, args]);
    if (cmd === "skill_draft_from_chat") return draft;
    if (cmd === "skill_save_from_chat") {
      const r = saves.length > 1 ? saves.shift()! : saves[0]!;
      if (typeof r === "string") throw r;
      return r;
    }
    if (cmd === "skills_list") return [{ name: "port-parser", description: "", source: "user", path: SAVED.path, enabled: true, files: 1 }];
    return null;
  });
});
afterEach(() => { cleanup(); set({ skillFrom: null }); });

const openAndDraft = async () => {
  openSaveAsSkill("t1");
  render(<SaveAsSkill />);
  fireEvent.click(screen.getByRole("button", { name: "Draft skill" }));
  return (await screen.findByLabelText("Name")) as HTMLInputElement;
};
const saveBtn = () => screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;

describe("name and path helpers", () => {
  it("mirrors the backend's name rule", () => {
    expect(skillNameProblem("port-parser")).toBe("");
    expect(skillNameProblem("a")).toBe("");
    expect(skillNameProblem("")).toMatch(/name/);
    expect(skillNameProblem("Port Parser")).toMatch(/lowercase/);
    expect(skillNameProblem("-x")).toMatch(/lowercase/);
    expect(skillNameProblem("x-")).toMatch(/lowercase/);
    expect(skillNameProblem("a".repeat(64))).toBe("");
    expect(skillNameProblem("a".repeat(65))).toMatch(/64/);
  });
  it("joins with the separator the folder uses", () => {
    expect(skillPath("C:\\u\\skills", "x")).toBe("C:\\u\\skills\\x\\SKILL.md");
    expect(skillPath("/home/u/skills/", "x")).toBe("/home/u/skills/x/SKILL.md");
  });
});

it("opens from the palette helper without starting a draft", async () => {
  openSaveAsSkill("t1");
  expect(get().skillFrom).toBe("t1");
  render(<SaveAsSkill />);
  expect(screen.getByRole("button", { name: "Draft skill" })).toBeTruthy();
  expect(screen.getByText(/Distil/)).toBeTruthy();
  await new Promise((r) => setTimeout(r, 20));
  expect(calls.some(([c]) => c === "skill_draft_from_chat")).toBe(false);
});

it("offers Save as skill… in the chat's context menu, and only opens the box", async () => {
  set({ sidebar: true, view: "home", task: null, picked: {}, selecting: false, settings: { project: "", all_projects: true } as never });
  render(<Sidebar />);
  fireEvent.contextMenu(screen.getByText("Port the parser"), { clientX: 40, clientY: 40 });
  const row = [...document.querySelectorAll(".pop.ctx .mrow")].find((r) => r.textContent === "Save as skill…");
  expect(row).toBeTruthy();
  fireEvent.click(row!);
  expect(get().skillFrom).toBe("t1");
  await new Promise((r) => setTimeout(r, 20));
  expect(calls.some(([c]) => c === "skill_draft_from_chat")).toBe(false);
});

it("drafts on click with the chat id, then saves the edited values", async () => {
  const name = await openAndDraft();
  expect(calls.find(([c]) => c === "skill_draft_from_chat")?.[1]).toEqual({ id: "t1", model: null });
  expect(screen.getByText(/Redacted 2 API-key-looking strings/)).toBeTruthy();
  expect(name.value).toBe("port-parser");
  fireEvent.change(name, { target: { value: "parser-port" } });
  fireEvent.change(screen.getByLabelText(/Description/), { target: { value: "Edited description" } });
  fireEvent.change(screen.getByLabelText(/Instructions/), { target: { value: "Edited body" } });
  expect(screen.getByText("C:\\Users\\x\\.openleash\\skills\\parser-port\\SKILL.md")).toBeTruthy();
  fireEvent.click(screen.getByRole("radio", { name: "This project" }));
  expect(screen.getByText("D:\\repo\\.openleash\\skills\\parser-port\\SKILL.md")).toBeTruthy();
  fireEvent.click(saveBtn());
  await screen.findByText("Saved skill parser-port");
  expect(saveReqs()).toEqual([{ id: "t1", name: "parser-port", description: "Edited description", body: "Edited body", scope: "project", overwrite: false, allow_secrets: false }]);
});

it("refreshes the skills list and says so once saved", async () => {
  await openAndDraft();
  fireEvent.click(saveBtn());
  await screen.findByText("Saved skill port-parser");
  expect(calls.find(([c]) => c === "skills_list")?.[1]).toEqual({ project: "D:\\repo" });
  await waitFor(() => expect(get().skills.map((s) => s.name)).toEqual(["port-parser"]));
  expect(get().toast).toBe("Saved skill port-parser");
  expect(screen.getByText(SAVED.path)).toBeTruthy();
});

it("disables Save and says why while the name is invalid", async () => {
  const name = await openAndDraft();
  expect(saveBtn().disabled).toBe(false);
  fireEvent.change(name, { target: { value: "Bad Name" } });
  expect(screen.getByText(/lowercase letters, digits and hyphens/)).toBeTruthy();
  expect(saveBtn().disabled).toBe(true);
  fireEvent.click(saveBtn());
  expect(saveReqs()).toHaveLength(0);
});

it("asks before replacing an existing skill, then resends with overwrite", async () => {
  saves = [{ saved: false, exists: true, secrets: [], path: SAVED.path, note: "" }, SAVED];
  await openAndDraft();
  fireEvent.click(saveBtn());
  const box = await screen.findByRole("group", { name: "Replace existing skill" });
  expect(box.textContent).toMatch(/already exists at .*SKILL\.md\. Replace its SKILL\.md\?/);
  fireEvent.click(within(box).getByRole("button", { name: "Replace" }));
  await screen.findByText("Saved skill port-parser");
  expect(saveReqs().map((r) => [r.overwrite, r.allow_secrets])).toEqual([[false, false], [true, false]]);
});

it("asks before saving text that looks like it has secrets, and keeps an earlier overwrite", async () => {
  saves = [
    { saved: false, exists: true, secrets: [], path: SAVED.path, note: "" },
    { saved: false, exists: false, secrets: ["an OpenAI-style key (sk-…)"], path: SAVED.path, note: "" },
    { ...SAVED, note: "Saved, but this folder is untrusted." },
  ];
  await openAndDraft();
  fireEvent.click(saveBtn());
  fireEvent.click(within(await screen.findByRole("group", { name: "Replace existing skill" })).getByRole("button", { name: "Replace" }));
  const box = await screen.findByRole("group", { name: "Possible secrets" });
  expect(box.textContent).toMatch(/still looks like it contains secrets/);
  expect(box.textContent).toMatch(/OpenAI-style key/);
  fireEvent.click(within(box).getByRole("button", { name: "Save anyway" }));
  await screen.findByText("Saved skill port-parser");
  expect(screen.getByText(/this folder is untrusted/)).toBeTruthy();
  expect(saveReqs().map((r) => [r.overwrite, r.allow_secrets])).toEqual([[false, false], [true, false], [true, true]]);
});

it("cancelling a confirmation writes nothing", async () => {
  saves = [{ saved: false, exists: false, secrets: ["a GitHub token (ghp_…)"], path: SAVED.path, note: "" }];
  await openAndDraft();
  fireEvent.click(saveBtn());
  const box = await screen.findByRole("group", { name: "Possible secrets" });
  fireEvent.click(within(box).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("group", { name: "Possible secrets" })).toBeNull();
  expect(saveReqs()).toHaveLength(1);
});

it("shows a backend error inline and stays open", async () => {
  saves = ["the description contains invisible characters"];
  await openAndDraft();
  fireEvent.click(saveBtn());
  await waitFor(() => expect(screen.getByRole("alert").textContent).toMatch(/invisible characters/));
  expect(get().skillFrom).toBe("t1");
  expect(saveBtn().disabled).toBe(false);
});

it("disables the project option when the chat has no project", async () => {
  draft = { ...DRAFT, project: "", project_dir: "" };
  await openAndDraft();
  expect((screen.getByRole("radio", { name: "This project" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(saveBtn());
  await screen.findByText("Saved skill port-parser");
  expect(saveReqs()[0]?.scope).toBe("user");
});

it("notes that project skills in an untrusted folder won't load", async () => {
  draft = { ...DRAFT, project_trusted: false };
  await openAndDraft();
  expect(screen.queryByText(/isn't trusted/)).toBeNull();
  fireEvent.click(screen.getByRole("radio", { name: "This project" }));
  expect(screen.getByText(/isn't trusted/)).toBeTruthy();
});

it("cancels a running draft from the button and when the box closes", async () => {
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    calls.push([cmd, args]);
    if (cmd === "skill_draft_from_chat") return new Promise(() => {});
    return null;
  });
  openSaveAsSkill("t1");
  render(<SaveAsSkill />);
  fireEvent.click(screen.getByRole("button", { name: "Draft skill" }));
  fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
  expect(calls).toContainEqual(["skill_draft_cancel", { id: "t1" }]);
  expect(screen.getByRole("button", { name: "Draft skill" })).toBeTruthy();
  calls.length = 0;
  fireEvent.click(screen.getByRole("button", { name: "Draft skill" }));
  await waitFor(() => expect(calls.some(([c]) => c === "skill_draft_from_chat")).toBe(true));
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  await waitFor(() => expect(calls).toContainEqual(["skill_draft_cancel", { id: "t1" }]));
});

// User-only: slash commands are reachable from connector messages (Discord and
// the like go through the composer's dispatcher), so none of this may be wired
// there. Source-level, in the style of palette-coverage.test.ts.
it("is not reachable from the composer's slash commands", async () => {
  const { readFileSync } = await import("node:fs");
  const composer = readFileSync("src/ui/Composer.tsx", "utf8");
  expect(composer).not.toMatch(/SaveAsSkill|skillFrom|skillDraftFromChat|skillSaveFromChat/);
  const store = readFileSync("src/store.ts", "utf8");
  expect(store).not.toMatch(/skillDraftFromChat|skillSaveFromChat/);
});
