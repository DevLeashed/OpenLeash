// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { set } from "../store";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn(async () => "/tmp/out.jsonl") }));

import { TrainingExport, exportTraining } from "./TrainingExport";

describe("training export", () => {
  beforeEach(() => {
    invoke.mockReset();
    set({ trainingExport: null });
  });
  afterEach(() => {
    cleanup();
    set({ trainingExport: null });
  });

  it("shows the format picker only for the chat that asked for it", () => {
    set({ trainingExport: "t1" });
    render(<TrainingExport />);
    expect(screen.getByRole("dialog", { name: "Export training data" })).toBeTruthy();
    expect(screen.getByRole("radio", { name: /OpenRouter/ })).toBeTruthy();
    expect(screen.getByRole("radio", { name: /Claude/ })).toBeTruthy();
  });

  it("writes the OpenRouter shape by default", async () => {
    await exportTraining("t1", "openrouter");
    expect(invoke).toHaveBeenCalledWith("task_training_export", {
      id: "t1",
      path: "/tmp/out.jsonl",
      format: "openrouter",
    });
  });

  it("writes the Claude shape when that is picked", async () => {
    set({ trainingExport: "t1" });
    render(<TrainingExport />);
    fireEvent.click(screen.getByRole("radio", { name: /Claude/ }));
    fireEvent.click(screen.getByRole("button", { name: /Export Claude/ }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("task_training_export", {
        id: "t1",
        path: "/tmp/out.jsonl",
        format: "claude",
      }),
    );
  });
});
