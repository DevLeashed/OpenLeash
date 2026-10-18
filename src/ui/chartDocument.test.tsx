// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AssistantMarkdown, visualizationParts } from "./Visualization";
import { buildChartBody, CHART_LIBRARY_SRC, CHART_MAX_HEIGHT, CHART_MIN_HEIGHT, inertJson, parseChartSpec } from "./chartDocument";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke, convertFileSrc: (id: string) => `http://openleash-viz.localhost/${id}` }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const option = { title: { text: "Weekly revenue" }, xAxis: { type: "category", data: ["Mon", "Tue"] }, yAxis: {}, series: [{ type: "bar", data: [3, 5] }] };
const fence = (s: string) => `\`\`\`openleash-chart\n${s}\n\`\`\``;

describe("openleash-chart blocks", () => {
  it("parses only exact, closed chart fences and shares the reply's block budget", () => {
    expect(visualizationParts(fence(JSON.stringify(option))).map(p => p.kind)).toEqual(["chart"]);
    for (const text of ["```openleash-chart extra\n{}\n```", "```json\n{}\n```", "```openleash-charts\n{}\n```"]) expect(visualizationParts(text).every(p => p.kind === "markdown")).toBe(true);
    expect(visualizationParts("```openleash-chart\n{}").map(p => p.kind)).toEqual(["placeholder"]);
    const mixed = visualizationParts([...Array.from({ length: 5 }, () => fence("{}")), ...Array.from({ length: 4 }, () => "```openleash-viz\n<p>x</p>\n```")].join("\n"));
    expect(mixed.filter(p => p.kind === "chart")).toHaveLength(5);
    expect(mixed.filter(p => p.kind === "visualization")).toHaveLength(3);
    expect(mixed.filter(p => p.kind === "placeholder")).toHaveLength(1);
  });

  it("validates the spec and strips host-only hints from the option", () => {
    expect(parseChartSpec("{nope")).toHaveProperty("error");
    expect(parseChartSpec("[1,2]")).toHaveProperty("error");
    expect(parseChartSpec("null")).toHaveProperty("error");
    const spec = parseChartSpec(JSON.stringify({ ...option, openleash: { height: 99999, renderer: "svg", title: "Override" } }));
    if ("error" in spec) throw new Error(spec.error);
    expect(spec.option).not.toHaveProperty("openleash");
    expect(spec).toMatchObject({ height: CHART_MAX_HEIGHT, renderer: "svg", title: "Override" });
    const plain = parseChartSpec(JSON.stringify({ ...option, openleash: { height: 1, renderer: "webgl" } }));
    if ("error" in plain) throw new Error(plain.error);
    expect(plain).toMatchObject({ height: CHART_MIN_HEIGHT, renderer: "canvas", title: "Weekly revenue" });
    const arrayTitle = parseChartSpec(JSON.stringify({ title: [{ text: "First" }, { text: "Second" }] }));
    expect(arrayTitle).toMatchObject({ title: "First" });
    expect(parseChartSpec("{}")).toMatchObject({ title: "Chart", height: 420 });
  });

  it("carries the spec as inert JSON that strings cannot break out of", () => {
    const hostile = "</script><script>parent.alert(1)</script><!--\u2028";
    const spec = parseChartSpec(JSON.stringify({ title: { text: hostile }, series: [{ type: "pie", name: hostile, data: [] }] }));
    if ("error" in spec) throw new Error(spec.error);
    const body = buildChartBody(spec);
    expect(body).not.toContain("<script>parent.alert");
    expect(body.match(/<\/script>/g)).toHaveLength(3); // the spec, the library and the boot script close once each
    expect(body).toContain(`<script src="${CHART_LIBRARY_SRC}"></script>`);
    expect(body).toContain('aria-label="&lt;/script&gt;&lt;script&gt;parent.alert(1)&lt;/script&gt;&lt;!--');
    // Round-trips exactly: escaping is JSON-level, not lossy.
    const json = /<script type="application\/json" id="openleash-chart-spec">([\s\S]*?)<\/script>/.exec(body)![1]!;
    expect(JSON.parse(json).option.series[0].name).toBe(hostile);
    expect(inertJson({ a: "<&>" })).toBe('{"a":"\\u003c\\u0026\\u003e"}');
  });

  it("runs the generated page but shows and copies the JSON spec", async () => {
    invoke.mockResolvedValueOnce("one").mockResolvedValue(undefined);
    const source = JSON.stringify({ ...option, openleash: { height: 560 } }, null, 2);
    render(<AssistantMarkdown text={fence(source)} />);
    await waitFor(() => expect(document.querySelector("iframe")?.getAttribute("src")).toContain("one"));
    const published = invoke.mock.calls.find(([command]) => command === "visualization_publish")![1].source as string;
    expect(published).toContain(CHART_LIBRARY_SRC);
    expect(published).toContain("echarts.init");
    expect(document.querySelector("iframe")?.getAttribute("sandbox")).toBe("allow-scripts");
    expect((document.querySelector("iframe") as HTMLIFrameElement).style.height).toBe("560px");
    expect(screen.getByRole("region", { name: "Weekly revenue" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Source" }));
    expect(screen.getByText(source, { normalizer: s => s })).toBeTruthy();
  });

  it("shows an invalid spec as a placeholder and never publishes it", () => {
    render(<AssistantMarkdown text={fence("{ \"series\": [ }")} />);
    expect(screen.getByRole("status").textContent).toMatch(/not valid JSON/);
    expect(invoke).not.toHaveBeenCalled();
  });
});
