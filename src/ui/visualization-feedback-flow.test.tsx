// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ArtifictWorkspace } from "./ArtifictWorkspace";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));
vi.mock("./Visualization", () => ({ Visualization: ({ onFeedbackDraft }: { onFeedbackDraft?: (draft: unknown) => void }) => <button onClick={() => onFeedbackDraft?.({ target: { selector: "button", label: "Save" }, state: { choice: "compact" } })}>Emit demo feedback</button> }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
const v = { id: "v1", parentId: null, createdAt: "2026-10-07T10:00:00Z", content: "<button>Save</button>", decisions: [], constraints: [], codeRefs: [] };
const artifict = { id: "art-1", title: "Demo", kind: "html", currentVersionId: v.id, createdAt: v.createdAt, updatedAt: v.createdAt, versions: [v], annotations: [], feedback: [] };

it("requires human review before a prototype's suggested target/state enters feedback fields", async () => {
  invoke.mockImplementation(async (command: string) => command === "artifict_list" ? [{ id: artifict.id, title: artifict.title, kind: artifict.kind, currentVersionId: v.id, createdAt: v.createdAt, updatedAt: v.createdAt }] : structuredClone(artifict));
  render(<ArtifictWorkspace taskId="task" taskTitle="Test" />);
  await screen.findByLabelText("Interactive state JSON");
  fireEvent.click(screen.getByRole("button", { name: "Emit demo feedback" }));
  const proposal = await screen.findByRole("status");
  expect(proposal.textContent).toContain("Preview proposal");
  expect((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value).toBe("");
  expect(invoke).not.toHaveBeenCalledWith("artifict_feedback_submit", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "Use fields" }));
  expect((screen.getByLabelText("Comment selector") as HTMLInputElement).value).toBe("button");
  expect((screen.getByLabelText("Comment target") as HTMLInputElement).value).toBe("Save");
  expect(JSON.parse((screen.getByLabelText("Interactive state JSON") as HTMLTextAreaElement).value)).toEqual({ choice: "compact" });
  fireEvent.change(screen.getByLabelText("Follow up for agent"), { target: { value: "Please update it" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifict_feedback_submit", expect.objectContaining({ taskId: "task", artifictId: "art-1", versionId: "v1", interactiveState: { choice: "compact" } })));
});
