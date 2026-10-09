// @vitest-environment jsdom
import { existsSync, readFileSync } from "node:fs";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { MorphText } from "./MorphText";

// Vite extracts CSS imports into a bundled stylesheet in production. Load those
// rules explicitly in jsdom, which does not load imported CSS. The optional file
// lets this regression also run against the original, App.css-only primitive.
const styles = ["src/App.css", "src/ui/primitives/MorphText.css"]
  .filter((path) => existsSync(path)).map((path) => readFileSync(path, "utf8")).join("\n");
let stylesheet: HTMLStyleElement;

beforeEach(() => {
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener() {}, removeEventListener() {} })));
  (Element.prototype as unknown as { getAnimations?: unknown }).getAnimations ??= () => [];
  stylesheet = document.createElement("style");
  stylesheet.textContent = styles;
  document.head.appendChild(stylesheet);

  // A Tauri CSP can reject torph's unnonced, mount-time stylesheet. Render the
  // real engine/DOM but prevent that stylesheet from supplying any rules.
  const append = document.head.appendChild.bind(document.head);
  vi.spyOn(document.head, "appendChild").mockImplementation(<T extends Node>(node: T): T => {
    if (node instanceof HTMLStyleElement && node.dataset.torph === "true") return node;
    return append(node);
  });
});

afterEach(() => {
  cleanup();
  stylesheet.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function visibleText(node: Node): string {
  if (node instanceof Element) {
    const style = getComputedStyle(node);
    if (style.display === "none" || style.visibility === "hidden" || style.clipPath === "inset(50%)") return "";
  }
  return node.nodeType === Node.TEXT_NODE ? node.textContent ?? "" : [...node.childNodes].map(visibleText).join("");
}

function expectSingleText(root: Element, text: string) {
  expect(visibleText(root).replace(/\u00a0/g, " ")).toBe(text);
  const accessible = root.querySelector("[torph-sr]");
  expect(accessible?.textContent).toBe(text);
  expect(accessible?.hasAttribute("aria-hidden")).toBe(false);
  const style = getComputedStyle(accessible!);
  expect(style.position).toBe("absolute");
  expect(style.width).toBe("1px");
  expect(style.height).toBe("1px");
  expect(style.overflow).toBe("hidden");
  expect(style.clipPath).toBe("inset(50%)");
  expect(style.userSelect).toBe("none");
  // Visually clipped, not removed from the accessibility tree.
  expect(style.display).not.toBe("none");
  expect(style.visibility).not.toBe("hidden");
  expect(root.querySelectorAll("[torph-sr]")).toHaveLength(1);
  expect(root.querySelectorAll('[torph-item][aria-hidden="true"]').length).toBeGreaterThan(0);
}

describe("MorphText without the engine's injected stylesheet", () => {
  it.each(["59%", "64%", "Serving now", "Ready", "Off", "Copied", "3 tools"])("renders %s once while preserving its accessible text", (text) => {
    const view = render(<MorphText>{text}</MorphText>);
    expect(document.head.querySelector("style[data-torph]")).toBeNull();
    expectSingleText(view.container.firstElementChild!, text);
  });

  it("keeps structural animation styles for numeric slots and custom block roots", () => {
    const view = render(<MorphText as="div" className="sval" style={{ color: "red" }}>$1,204</MorphText>);
    const root = view.container.firstElementChild!;
    expect(root.tagName).toBe("DIV");
    expect(root.classList.contains("sval")).toBe(true);
    expect(getComputedStyle(root).color).toBe("rgb(255, 0, 0)");
    expect(getComputedStyle(root).position).toBe("relative");
    expect(getComputedStyle(root).display).toBe("inline-block");
    expectSingleText(root, "$1,204");
    const slot = root.querySelector("[torph-slot]")!;
    expect(getComputedStyle(slot).clipPath).toBe("inset(0 -100vw)");
    expect(getComputedStyle(slot).display).toBe("inline-block");
    expect(getComputedStyle(slot.firstElementChild!).display).toBe("inline-block");
  });

  it("preserves alignment supplied by a caller's class", () => {
    const view = render(<MorphText className="task-state">Ready</MorphText>);
    expect(getComputedStyle(view.container.firstElementChild!).textAlign).toBe("right");
    expectSingleText(view.container.firstElementChild!, "Ready");
  });

  it("keeps one accessible copy after StrictMode effect remounts", () => {
    const view = render(<StrictMode><MorphText className="task-state">Ready</MorphText></StrictMode>);
    expectSingleText(view.container.firstElementChild!, "Ready");
    view.unmount();
    const remount = render(<MorphText>Off</MorphText>);
    expectSingleText(remount.container.firstElementChild!, "Off");
  });

  it("updates plain text once when reduced motion is requested", () => {
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: true, addEventListener() {}, removeEventListener() {} })));
    const view = render(<MorphText>Serving now</MorphText>);
    expect(visibleText(view.container)).toBe("Serving now");
    expect(view.container.querySelector("[torph-sr]")).toBeNull();
    view.rerender(<MorphText>Off</MorphText>);
    expect(visibleText(view.container)).toBe("Off");
  });
});
