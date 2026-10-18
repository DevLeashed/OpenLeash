/** `openleash-chart` blocks: a declarative Apache ECharts option, as JSON,
 *  rendered by the host-bundled engine inside the same isolated preview frame
 *  an `openleash-viz` document runs in.
 *
 *  The spec never becomes markup. It is parsed here, re-serialized, and carried
 *  into the frame as an inert JSON script element, so a string inside it (a
 *  label, a tooltip template) cannot close a tag or run as code. The engine
 *  itself is served by the native viz protocol at `lib/echarts.js` (see
 *  `src-tauri/src/visualization.rs`); it is far too large to inline under the
 *  per-document limit. */

/** Where a preview document loads the engine from, relative to its own URL so
 *  the same markup works under the native scheme and remote access. Must match
 *  `LIBRARY_PATH` in `visualization.rs`. */
export const CHART_LIBRARY_SRC = "lib/echarts.js";

/** Frame height bounds for the host-only `openleash.height` hint. */
export const CHART_MIN_HEIGHT = 200;
export const CHART_MAX_HEIGHT = 1200;
export const CHART_DEFAULT_HEIGHT = 420;

export type ChartSpec = {
  /** The ECharts option, without the host-only `openleash` key. */
  option: Record<string, unknown>;
  title: string;
  height: number;
  renderer: "canvas" | "svg";
};

/** Parse a chart block. Returns a reason instead of throwing: a broken spec is
 *  shown to the reader as a placeholder with its source, not as a crash. */
export function parseChartSpec(source: string): ChartSpec | { error: string } {
  let value: unknown;
  try { value = JSON.parse(source); } catch (e) {
    return { error: `This chart spec is not valid JSON: ${e instanceof Error ? e.message : String(e)}` };
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return { error: "This chart spec must be a JSON object (an ECharts option)." };
  const { openleash: hints, ...option } = value as Record<string, unknown>;
  const host = hints && typeof hints === "object" && !Array.isArray(hints) ? hints as Record<string, unknown> : {};
  const rawHeight = typeof host.height === "number" && Number.isFinite(host.height) ? host.height : CHART_DEFAULT_HEIGHT;
  const height = Math.round(Math.min(CHART_MAX_HEIGHT, Math.max(CHART_MIN_HEIGHT, rawHeight)));
  const renderer = host.renderer === "svg" ? "svg" : "canvas";
  return { option, title: chartTitle(option, host), height, renderer };
}

/** The reader-facing name: an explicit host title, else the option's own first
 *  title text, else a generic label. */
function chartTitle(option: Record<string, unknown>, host: Record<string, unknown>): string {
  const pick = (v: unknown) => typeof v === "string" ? v.replace(/\s+/g, " ").trim().slice(0, 160) : "";
  const first = Array.isArray(option.title) ? option.title[0] : option.title;
  const own = first && typeof first === "object" ? pick((first as Record<string, unknown>).text) : "";
  return pick(host.title) || own || "Chart";
}

/** JSON that is safe inside a `<script type="application/json">` element: no
 *  `<` can start `</script>` or `<!--`, and the line separators JavaScript
 *  once rejected are escaped too. */
export function inertJson(value: unknown): string {
  return JSON.stringify(value)
    .replace(/</g, "\\u003c")
    .replace(/>/g, "\\u003e")
    .replace(/&/g, "\\u0026")
    .replace(/\u2028/g, "\\u2028")
    .replace(/\u2029/g, "\\u2029");
}

/** The body of the preview document for one chart; `buildVisualizationDocument`
 *  wraps it with the theme tokens and the host bridge. */
export function buildChartBody(spec: ChartSpec): string {
  const label = spec.title.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;" })[c]!);
  return `<style>html,body{height:100%}body{padding:0;overflow:hidden}#openleash-chart{position:absolute;inset:8px}#openleash-chart-error{position:absolute;inset:16px;color:var(--muted);font:13px/1.5 var(--font-sans);white-space:pre-wrap}</style>
<div id="openleash-chart" role="img" aria-label="${label}"></div>
<script type="application/json" id="openleash-chart-spec">${inertJson({ option: spec.option, renderer: spec.renderer })}</script>
<script src="${CHART_LIBRARY_SRC}"></script>
<script>${CHART_BOOT}</script>`;
}

/** Runs in the frame after the engine loads. Accessibility descriptions are on
 *  unless the spec decides otherwise; the chart follows the frame's size; and a
 *  failure is shown in place rather than leaving an empty box. */
const CHART_BOOT = String.raw`(function () {
  var el = document.getElementById("openleash-chart");
  var fail = function (message) {
    el.style.display = "none";
    var box = document.createElement("div");
    box.id = "openleash-chart-error";
    box.setAttribute("role", "alert");
    box.textContent = message;
    document.body.appendChild(box);
  };
  if (!window.echarts) { fail("The chart engine could not be loaded."); return; }
  try {
    var spec = JSON.parse(document.getElementById("openleash-chart-spec").textContent);
    var option = spec.option;
    if (option.aria === undefined) option.aria = { enabled: true };
    var chart = echarts.init(el, undefined, { renderer: spec.renderer });
    chart.setOption(option);
    window.openleashChart = chart;
    if (window.ResizeObserver) new ResizeObserver(function () { chart.resize(); }).observe(el);
    else window.addEventListener("resize", function () { chart.resize(); });
  } catch (e) {
    fail("This chart could not be drawn: " + (e && e.message ? e.message : String(e)));
  }
})();`;
