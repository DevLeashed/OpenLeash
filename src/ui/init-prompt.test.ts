import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { INIT_PROMPT, SLASH } from "./Composer";

describe("/init project-instructions interview", () => {
  it("investigates first and asks only about information the repository cannot answer", () => {
    expect(INIT_PROMPT).toContain("First inspect the repository, manifests, docs, and existing instruction files");
    expect(INIT_PROMPT).toContain("Do not ask me about anything you can discover in the code or existing docs");
    expect(INIT_PROMPT).toContain("only material project-specific questions the repository cannot answer");
    expect(INIT_PROMPT).toContain("no more than five questions");
    expect(INIT_PROMPT).toContain("skip the interview if no such questions remain");
  });

  it("updates an existing instruction file in place and creates a root file only when none exist", () => {
    for (const name of ["OPENLEASH.md", "AGENTS.md", "CLAUDE.md", ".openleash/instructions.md"]) {
      expect(INIT_PROMPT).toContain(name);
    }
    expect(INIT_PROMPT).toContain("update the most relevant existing file in place");
    expect(INIT_PROMPT).toContain("preserve its useful content");
    expect(INIT_PROMPT).toContain("do not replace it wholesale or create a duplicate");
    expect(INIT_PROMPT).toContain("Create root `OPENLEASH.md` only if none exists");
  });

  it("advertises the interview behavior in slash autocomplete", () => {
    expect(SLASH.find((command) => command.cmd === "/init")?.desc).toBe("Interview you before writing project instructions");
  });
});
