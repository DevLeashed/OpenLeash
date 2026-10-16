// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ArtifictRecord, parseCollaborationRecord } from "./ArtifictRecord";
import { ImageExplanation } from "./ImageExplanation";
afterEach(cleanup);
const record = { schema: "openleash.collaboration.v1", title: "Architecture", purpose: "Explain real code", stage: "prototype", nodes: [{ id: "ui", title: "UI", description: "Renders", refs: [{ path: "src/app.ts", line: 2 }] }, { id: "core", title: "Core", description: "Runs", refs: [] }], edges: [{ from: "ui", to: "core", reason: "IPC" }], evidence: [{ command: "npm test", result: "12 passed", scope: "UI", path: "Logs/results.txt" }], limitations: ["No native smoke"] };
it("validates bounded records and refuses invented endpoints and unsafe paths", () => {
  expect(parseCollaborationRecord(JSON.stringify(record))).toBeTruthy();
  expect(parseCollaborationRecord(JSON.stringify({ ...record, edges: [{ from: "missing", to: "core", reason: "bad" }] }))).toBeNull();
  expect(parseCollaborationRecord(JSON.stringify({ ...record, nodes: [{ ...record.nodes[0], refs: [{ path: "../secret" }] }] }))).toBeNull();
});
it("explores references and stages discussion without submitting", () => {
  const onDraft = vi.fn();
  render(<ArtifictRecord record={parseCollaborationRecord(JSON.stringify(record))!} onDraft={onDraft} />);
  expect(screen.getByText("src/app.ts:2")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Discuss this component" }));
  expect(onDraft).toHaveBeenCalledWith({ target: { label: "Component ui: UI" } });
  expect(screen.getByText(/not independently certified/)).toBeTruthy();
});
it("stages a bounded screenshot region as private review data", () => {
  const onDraft = vi.fn();
  render(<ImageExplanation source={JSON.stringify({ src: "https://example.org/a.png" })} onFeedbackDraft={onDraft} />);
  fireEvent.click(screen.getByRole("button", { name: "Select screenshot region" }));
  fireEvent.change(screen.getByLabelText("Region x"), { target: { value: "90" } });
  fireEvent.click(screen.getByRole("button", { name: "Stage region for feedback" }));
  expect(onDraft.mock.calls[0]![0].state.imageRegion.width).toBe(10);
  expect(onDraft.mock.calls[0]![0].target.label).toContain("Screenshot region");
});
