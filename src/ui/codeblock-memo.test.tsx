// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import ReactMarkdown from "react-markdown";
import { mdComponents, remarkOl } from "./mdx";

const grammar = vi.hoisted(() => ({
  ready: false,
  subscribers: new Set<() => void>(),
  highlight: vi.fn((_lang: string, code: string) => [{ type: "text" as const, value: code }]),
}));
vi.mock("./highlight", () => ({
  grammarsReady: () => grammar.ready,
  onReady: (cb: () => void) => { grammar.subscribers.add(cb); return () => grammar.subscribers.delete(cb); },
  highlight: grammar.highlight,
}));
beforeEach(() => { grammar.ready = false; grammar.highlight.mockClear(); });
afterEach(cleanup);

it("reuses unchanged code across surrounding markdown reparses while observing grammar readiness", () => {
  const markdown = (tail: string, code = "const x = 1;", lang = "ts") =>
    <ReactMarkdown components={mdComponents} remarkPlugins={[remarkOl]}>{`\`\`\`${lang}\n${code}\n\`\`\`\n\n${tail}`}</ReactMarkdown>;
  const view = render(markdown("First"));
  expect(grammar.highlight).not.toHaveBeenCalled();
  expect(view.container.querySelector("pre code")?.textContent).toBe("const x = 1;");
  act(() => {
    grammar.ready = true;
    for (const cb of [...grammar.subscribers]) cb();
  });
  expect(grammar.highlight).toHaveBeenCalledTimes(1);
  const codeNode = view.container.querySelector("pre code");

  view.rerender(markdown("First and more"));
  view.rerender(markdown("First and more text"));
  expect(grammar.highlight).toHaveBeenCalledTimes(1);
  expect(view.container.querySelector("pre code")).toBe(codeNode);
  expect(view.container.querySelector("p")?.textContent).toBe("First and more text");

  view.rerender(markdown("First and more text", "const x = 2;"));
  expect(grammar.highlight).toHaveBeenCalledTimes(2);
  expect(view.container.querySelector("pre code")?.textContent).toBe("const x = 2;");
  view.rerender(markdown("First and more text", "const x = 2;", "js"));
  expect(grammar.highlight).toHaveBeenCalledTimes(3);
  expect(grammar.highlight).toHaveBeenLastCalledWith("js", "const x = 2;");
  expect(view.container.querySelector("pre code")?.className).toBe("language-js");
});
