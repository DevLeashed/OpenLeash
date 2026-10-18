import { describe, expect, it } from "vitest";
import { annotationPayload, historySuggestions, keyIntent, normalizeAddress, previewStyles, rectFrom, styleValueOk, toHex } from "./browserPanelModel";

const k = (key: string, mods: Partial<Record<"ctrlKey" | "shiftKey" | "altKey" | "metaKey", boolean>> = {}) => ({ key, ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, ...mods });

describe("address bar", () => {
  it("normalises what people type without ever searching", () => {
    expect(normalizeAddress("localhost:3000")).toEqual({ url: "http://localhost:3000/" });
    expect(normalizeAddress("localhost:5173/app?x=1")).toEqual({ url: "http://localhost:5173/app?x=1" });
    expect(normalizeAddress("127.0.0.1:8080")).toEqual({ url: "http://127.0.0.1:8080/" });
    expect(normalizeAddress("192.168.1.20")).toEqual({ url: "http://192.168.1.20/" });
    expect(normalizeAddress("devbox:8080")).toEqual({ url: "http://devbox:8080/" });
    expect(normalizeAddress("example.com")).toEqual({ url: "https://example.com/" });
    expect(normalizeAddress("docs.example.com/guide#x")).toEqual({ url: "https://docs.example.com/guide#x" });
    expect(normalizeAddress("  https://Example.org/a ")).toEqual({ url: "https://example.org/a" });
    expect(normalizeAddress("http://localhost")).toEqual({ url: "http://localhost/" });
    for (const bad of ["", "hello", "how to center a div", "javascript:alert(1)", "file:///etc/passwd", "data:text/html,x", "chrome://settings", "about:blank", "ftp://x.test", ".com", "example."]) {
      expect("error" in normalizeAddress(bad), bad).toBe(true);
    }
  });

  it("filters history by what is typed and offers all of it for an untouched address", () => {
    const h = ["https://a.test/docs", "http://localhost:3000/", "https://b.test/"];
    expect(historySuggestions(h, "local", "https://b.test/")).toEqual(["http://localhost:3000/"]);
    expect(historySuggestions(h, "DOCS", "https://b.test/")).toEqual(["https://a.test/docs"]);
    expect(historySuggestions(h, "https://b.test/", "https://b.test/")).toEqual(["https://a.test/docs", "http://localhost:3000/"]);
    expect(historySuggestions(undefined, "x", "")).toEqual([]);
    expect(historySuggestions(Array.from({ length: 20 }, (_, i) => `https://s${i}.test/`), "", "")).toHaveLength(8);
  });
});

describe("keyboard forwarding", () => {
  it("maps keys to the closed set and printable text to type", () => {
    expect(keyIntent(k("a"), false)).toEqual({ kind: "text", text: "a" });
    expect(keyIntent(k("A", { shiftKey: true }), false)).toEqual({ kind: "text", text: "A" });
    expect(keyIntent(k("Enter"), true)).toEqual({ kind: "key", key: "Enter" });
    expect(keyIntent(k("ArrowLeft", { ctrlKey: true, shiftKey: true }), false)).toEqual({ kind: "key", key: "ArrowLeft", modifiers: { ctrl: true, shift: true } });
    expect(keyIntent(k(" "), true)).toEqual({ kind: "text", text: " " });
    expect(keyIntent(k(" "), false)).toEqual({ kind: "key", key: "Space" });
    expect(keyIntent(k("Tab", { shiftKey: true }), false)).toEqual({ kind: "key", key: "Tab", modifiers: { shift: true } });
    // App shortcuts and keys outside the set stay with the app.
    expect(keyIntent(k("f", { ctrlKey: true }), false)).toBeNull();
    expect(keyIntent(k("F12"), false)).toBeNull();
    expect(keyIntent(k("Shift", { shiftKey: true }), false)).toBeNull();
  });
});

describe("annotate helpers", () => {
  const element = { ref: "e7", selector: "h1.title", tag: "H1", role: "heading", name: "Welcome", bbox: { x: 1, y: 2, width: 3, height: 4 }, styles: { "font-size": "32px", color: "rgb(0, 0, 0)" } };

  it("validates style values like the backend", () => {
    for (const ok of ["18px", "rgb(1, 2, 3)", "\"Inter\", sans-serif", "calc(100% - 2px)"]) expect(styleValueOk(ok), ok).toBe(true);
    for (const bad of ["", "red;", "url(x)", "expression(1)", "a{b}", "<b>", "\\61", "@import", "javascript:x", "a\u0007", "x".repeat(201)]) expect(styleValueOk(bad), bad).toBe(false);
  });

  it("converts computed colours for the colour picker", () => {
    expect(toHex("rgb(255, 0, 16)")).toBe("#ff0010");
    expect(toHex("rgba(1, 2, 3, 0.5)")).toBe("#010203");
    expect(toHex("#ABCDEF")).toBe("#abcdef");
    expect(toHex("red")).toBeNull();
  });

  it("builds the wire payload with only changed styles, before → after", () => {
    const payload = annotationPayload([
      { id: 1, comment: "  Bigger  ", bbox: element.bbox, element, styles: { "font-size": "40px", color: "rgb(0, 0, 0)" } },
      { id: 2, comment: "Gap", bbox: { x: 0, y: 10, width: 50, height: 20 }, styles: {} },
    ]);
    expect(payload).toEqual([
      { comment: "Bigger", bbox: element.bbox, element: { ref: "e7", selector: "h1.title", tag: "H1", role: "heading", name: "Welcome" }, styles: [{ property: "font-size", before: "32px", after: "40px" }] },
      { comment: "Gap", bbox: { x: 0, y: 10, width: 50, height: 20 } },
    ]);
    expect(previewStyles({ element, styles: { "font-size": "40px", color: "bad;" } })).toEqual([{ property: "font-size", value: "40px" }]);
    expect(previewStyles({ styles: { color: "red" } })).toEqual([]);
  });

  it("makes a rectangle from a drag in any direction", () => {
    expect(rectFrom({ x: 50, y: 40 }, { x: 10, y: 60 })).toEqual({ x: 10, y: 40, width: 40, height: 20 });
  });
});
