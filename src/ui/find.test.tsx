// @vitest-environment jsdom
import { describe, expect, it, beforeEach } from "vitest";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { FindBar } from "./Find";
import { set } from "../store";
import type { Item } from "../api";

const item = (id: string, kind: Item["kind"], text: string, data: any = {}): Item => ({ id, kind, text, data, ts: "2026-01-01T00:00:00Z" });

beforeEach(() => {
  cleanup();
  set({
    task: "t1",
    find: true,
    findSeed: "",
    items: {
      t1: [
        item("a", "user", "add MCP over HTTP"),
        item("b", "text", "The session id rides on every request."),
        item("c", "tool", "Edited a file", { name: "edit_file", input: { path: "src/agent/mcp.rs" } }),
      ],
    },
  } as never);
});

describe("FindBar", () => {
  it("starts empty and says nothing about results", () => {
    render(<FindBar onClose={() => {}} />);
    expect((screen.getByLabelText("Find in this chat") as HTMLInputElement).value).toBe("");
    expect(screen.queryByText(/of \d+/)).toBeNull();
  });

  it("counts the matches it found and marks the term in the preview", () => {
    render(<FindBar onClose={() => {}} />);
    fireEvent.change(screen.getByLabelText("Find in this chat"), { target: { value: "http" } });
    expect(screen.getByText("1 of 1")).toBeTruthy();
    // The preview shows the text as written, so a lowercase query still marks
    // the "HTTP" the agent typed rather than echoing the query back.
    expect(document.querySelector("mark")?.textContent).toBe("HTTP");
  });

  it("says so when a term isn't there, rather than showing 0 of 0", () => {
    render(<FindBar onClose={() => {}} />);
    fireEvent.change(screen.getByLabelText("Find in this chat"), { target: { value: "kubernetes" } });
    expect(screen.getByText("No results")).toBeTruthy();
  });

  it("opens with a seed term already in the box", () => {
    set({ findSeed: "session" } as never);
    render(<FindBar onClose={() => {}} />);
    expect((screen.getByLabelText("Find in this chat") as HTMLInputElement).value).toBe("session");
    expect(screen.getByText("1 of 1")).toBeTruthy();
  });

  it("closes on Escape without running a search", () => {
    let closed = 0;
    render(<FindBar onClose={() => { closed++; }} />);
    fireEvent.keyDown(screen.getByRole("search"), { key: "Escape" });
    expect(closed).toBe(1);
  });
});
