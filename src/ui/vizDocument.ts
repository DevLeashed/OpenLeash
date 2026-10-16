/** The document the isolated preview frame runs, and the theme it is handed.
 *
 *  Split out of the viewer component so a bare frame can publish the same
 *  document — the gallery renders a miniature of each artifact inside a card,
 *  and that miniature is the very same isolated runtime, not a screenshot. */
import { captureTheme, themeCss } from "./hostTheme";

/** Re-exported for callers that treat the frame document as the home of the
 *  theme: it moved to `hostTheme`, the leaf module an SVG image document reads
 *  it from too. */
export { captureTheme };

/** The document the isolated preview frame runs, given the theme it was handed.
 *  The wrapper precedes any supplied head/body so tokens exist when scripts run. */
export function buildVisualizationDocument(source: string, theme: Record<string, string>): string {
  const css = themeCss(theme);
  // A click is turned into a percentage of the document so the host can pin the
  // comment where the user actually pointed, at any frame size. `data-openleash-target`
  // still supplies the optional label; a click anywhere else is a valid point too.
  const feedbackBridge = `<script>(()=>{const send=(draft)=>{try{parent.postMessage({type:"openleash:feedback-draft",version:1,...draft},"*")}catch{}};const pct=(v,total)=>{if(!total)return 0;const n=Math.round((v/total)*1000)/10;return n<0?0:n>100?100:n};window.openleashFeedbackDraft=send;document.addEventListener("click",e=>{const t=e.target;const el=t instanceof Element?t:null;const marked=el?el.closest("[data-openleash-target]"):null;const label=(marked&&marked.getAttribute("data-openleash-target")||"").slice(0,300);const box=document.documentElement;const x=pct(e.clientX,box.clientWidth);const y=pct(e.clientY,box.clientHeight);const target={...(label?{label}:{}),...(marked?{selector:"[data-openleash-target]"}:{}),x,y};send({target})},true)})();</script>`;
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>:root{${css}}body{margin:0;padding:16px;background:var(--bg);color:var(--fg);font-family:var(--font-sans)}*{box-sizing:border-box}</style>${feedbackBridge}</head><body>${source}</body></html>`;
}
