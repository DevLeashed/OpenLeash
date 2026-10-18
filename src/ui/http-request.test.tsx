// @vitest-environment jsdom
// The `http_request` tool: its transcript row reads as the request it made
// (method, then host and path), and its approval card shows the redacted
// request the backend built, line for line, so the user approves what will be
// sent rather than a one-line title.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));

import type { Item, TaskSummary } from "../api";
import { FeedRows, ToolItem } from "./Session";

const task = { id: "http", title: "Call the API", status: "waiting", subs: [], cwd: "D:/p" } as unknown as TaskSummary;

const httpRow = (input: Record<string, unknown>, extra: Record<string, unknown> = {}, id = "t1"): Item => ({
  id, kind: "tool", text: "", ts: "2026-10-17T00:00:00Z",
  data: {
    name: "http_request", status: "done", input,
    output: "HTTP 201 Created · POST https://api.example.com/v1/items\ncontent-type: application/json\n\n{\"id\":7}",
    ...extra,
  },
});

const DETAIL = "POST https://api.example.com/v1/items?token=[redacted]\nAuthorization: [redacted]\nContent-Type: application/json\n\n{\"name\":\"widget\"}";

const approval = (data: Record<string, unknown> = {}): Item => ({
  id: "approval-http", kind: "approval", text: "", ts: "2026-10-17T00:00:00Z",
  data: {
    kind: "http", tool: "http_request", title: "HTTP POST api.example.com", detail: DETAIL,
    reason: "Sends a request to a host this project has not allowed.", rule: "http_request:POST api.example.com",
    ...data,
  },
});

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("an http_request row", () => {
  it("is labelled with the method and host + path, query folded", () => {
    const { container } = render(<ToolItem it={httpRow({ method: "post", url: "https://api.example.com/v1/items?token=[redacted]" })} cwd="D:/p" />);
    expect(container.querySelector(".verb")?.textContent).toBe("POST");
    expect(container.querySelector(".tgt")?.textContent).toBe("api.example.com/v1/items?…");
    // The status the result opens with, where other rows keep their meta.
    const metas = [...container.querySelectorAll(".hd .meta")].map((n) => n.textContent);
    expect(metas).toEqual(["201"]);
  });

  it("defaults to GET, drops a bare root path, and keeps an unparseable url as sent", () => {
    const { container } = render(<>
      <ToolItem it={httpRow({ url: "https://example.org/" }, { output: "" })} cwd="" />
      <ToolItem it={httpRow({ method: "delete", url: "not a url" }, { output: "" }, "t2")} cwd="" />
    </>);
    const verbs = [...container.querySelectorAll(".verb")].map((n) => n.textContent);
    const targets = [...container.querySelectorAll(".tgt")].map((n) => n.textContent);
    expect(verbs).toEqual(["GET", "DELETE"]);
    expect(targets).toEqual(["example.org", "not a url"]);
  });

  it("reads as a request in the folded group line, not a lowercased verb", () => {
    render(<FeedRows items={[httpRow({ method: "POST", url: "https://api.example.com/v1/items" })]} task={task} showThinking={false} />);
    expect(screen.getByText(/Sent POST to api\.example\.com\/v1\/items/)).toBeTruthy();
  });
});

describe("an http approval card", () => {
  it("shows the title and the whole redacted request, newlines intact", () => {
    const { container } = render(<FeedRows items={[approval()]} task={task} showThinking={false} />);
    expect(screen.getByText("Needs your approval")).toBeTruthy();
    const box = container.querySelector(".cmdbox");
    expect(box?.textContent).toBe(`HTTP POST api.example.com\n${DETAIL}`);
    // The secrets stay redacted; the card never invents a value.
    expect(box?.textContent).not.toMatch(/token=(?!\[redacted\])/);
    expect(screen.getByRole("button", { name: /Always allow/ }).textContent).toContain("http_request:POST api.example.com");
  });

  it("summarises a resolved request by its title", () => {
    const { container } = render(<FeedRows items={[approval({ resolved: "once" })]} task={task} showThinking={false} />);
    const line = container.querySelector(".resolved");
    expect(line?.textContent).toContain("Allowed once");
    expect(line?.textContent).toContain("HTTP POST api.example.com");
    expect(line?.textContent).not.toContain("Authorization");
  });
});
