import { describe, expect, it } from "vitest";
import { haystack, searchItems, segments } from "./chatsearch";
import type { Item } from "../api";

const item = (o: Partial<Item>): Item => ({ id: o.id ?? "i1", kind: o.kind ?? "text", text: o.text ?? "", data: o.data ?? {}, ts: o.ts ?? "2026-01-01T00:00:00Z" });

const items: Item[] = [
  item({ id: "a", kind: "user", text: "add MCP over HTTP" }),
  item({ id: "b", kind: "text", text: "I'll check the streamable HTTP transport.\nThe session id has to ride on every request." }),
  item({ id: "c", kind: "tool", text: "Edit src-tauri/src/agent/mcp.rs", data: { name: "edit_file", input: { path: "src-tauri/src/agent/mcp.rs", old_string: "streamable HTTP" } } }),
  item({ id: "d", kind: "text", text: "Done — the URL server now answers over SSE." }),
  item({ id: "e", kind: "tool", text: "Ran cargo test", data: { name: "bash", input: { command: "cargo test --quiet" } } }),
];

describe("searchItems", () => {
  it("finds prose, in transcript order", () => {
    expect(searchItems(items, "transport").map((h) => h.id)).toEqual(["b"]);
    expect(searchItems(items, "SSE").map((h) => h.id)).toEqual(["d"]);
  });

  it("finds a tool's input, not just what the row says", () => {
    // The row labelled "Ran cargo test" doesn't contain the command; the path in
    // an edit is what people actually go looking for.
    expect(searchItems(items, "cargo test").map((h) => h.id)).toEqual(["e"]);
    expect(searchItems(items, "agent/mcp.rs").map((h) => h.id)).toEqual(["c"]);
  });

  it("is case-insensitive, because nobody remembers the casing", () => {
    expect(searchItems(items, "MCP OVER http").map((h) => h.id)).toEqual(["a"]);
    expect(searchItems(items, "mcp").map((h) => h.id)).toEqual(["a", "c"]);
  });

  it("returns nothing for an empty query rather than everything", () => {
    expect(searchItems(items, "")).toEqual([]);
    expect(searchItems(items, "   ")).toEqual([]);
  });

  it("returns nothing when the term isn't there", () => {
    expect(searchItems(items, "kubernetes")).toEqual([]);
  });

  it("reports the matching line and where in it the hit is", () => {
    const [hit] = searchItems(items, "session id");
    expect(hit!.line).toBe("The session id has to ride on every request.");
    expect(hit!.line.slice(hit!.at, hit!.at + "session id".length)).toBe("session id");
  });

  it("counts every matching line of an item", () => {
    const multi = [item({ id: "m", text: "HTTP one\nnot this\nHTTP two" })];
    expect(searchItems(multi, "http")[0]!.count).toBe(2);
  });

  it("does not let one huge item flood the results", () => {
    const huge = [item({ id: "h", text: Array.from({ length: 500 }, (_, i) => `line ${i} needle`).join("\n") })];
    // Only the first 40 lines of an item are indexed, so a runaway blob can't
    // produce thousands of rows in a find bar.
    expect(searchItems(huge, "needle")[0]!.count).toBe(40);
  });

  it("caps results in transcript order without reading the remaining items", () => {
    const tail = item({ id: "unread" });
    Object.defineProperty(tail, "text", { get: () => { throw new Error("scanned past cap"); } });
    expect(searchItems([...items, tail], "http", 2)).toEqual(searchItems(items, "http").slice(0, 2));
    expect(searchItems([tail], "http", 0)).toEqual([]);
  });

  it("keeps full per-item counts at the cap and skips blank lines", () => {
    const text = "\n  \n" + Array.from({ length: 45 }, (_, i) => `needle ${i}\n\n`).join("");
    expect(searchItems([item({ text })], "needle", 1)[0]!.count).toBe(40);
    expect(searchItems([item({ text: "\n\r\n last needle" })], "needle")[0]!.line).toBe("last needle");
  });

  it("skips an empty reply", () => {
    expect(searchItems([item({ id: "x", kind: "text", text: "   " })], "anything")).toEqual([]);
  });
});

describe("haystack", () => {
  it("indexes the text and a tool's string inputs", () => {
    expect(haystack(items[2]!)).toContain("agent/mcp.rs");
    expect(haystack(items[2]!)).toContain("Edit src-tauri");
  });

  it("survives an item with no data", () => {
    expect(() => haystack(item({ text: "plain" }))).not.toThrow();
    expect(haystack(item({ text: "plain" }))).toBe("plain");
  });

  it("does not stringify a tool input into schema noise", () => {
    const withObj = [item({ id: "o", kind: "tool", text: "", data: { name: "bash", input: { command: "ls", opts: { recursive: true } } } })];
    // Only string inputs are indexed; the nested object is not flattened.
    expect(haystack(withObj[0]!)).toBe("ls");
  });
});

describe("segments", () => {
  it("splits a line around the match so the row can mark it", () => {
    expect(segments("the session id matters", 4, "session")).toEqual([["the ", "session", " id matters"]]);
  });

  it("returns the whole line when there is nothing to mark", () => {
    expect(segments("plain text", -1, "")).toEqual([["plain text", "", ""]]);
    expect(segments("plain text", 0, "nope")).toEqual([["plain text", "", ""]]);
  });
});
