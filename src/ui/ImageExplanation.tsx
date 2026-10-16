import { useState } from "react";
import { imageSource, ReplyImage, useImageUrl } from "./ReplyImage";
import type { VisualizationFeedbackDraft } from "./visualizationFeedback";

type Mark = { x: number; y: number; label: string };
type ImageSpec = { title: string; src: string; after?: string; marks: Mark[] };
/** https, an inline raster `data:` URL, or a path to an image on this machine,
 *  which the backend reads (see `reply_image`). A path is what agents should
 *  use: pasting a screenshot as base64 into the fence put hundreds of KB of
 *  text into the reply for nothing. */
const safeImage = (value: unknown): value is string => typeof value === "string" && value.length <= 200000 && imageSource(value).kind !== "blocked";
export function parseImageSpec(source: string): ImageSpec | null {
  try {
    const data = JSON.parse(source);
    if (!data || !safeImage(data.src) || (data.after !== undefined && !safeImage(data.after))) return null;
    const marks: Mark[] = [];
    if (data.marks !== undefined && (!Array.isArray(data.marks) || data.marks.length > 20)) return null;
    for (const mark of data.marks ?? []) {
      if (!mark || !Number.isFinite(mark.x) || !Number.isFinite(mark.y) || mark.x < 0 || mark.x > 100 || mark.y < 0 || mark.y > 100 || typeof mark.label !== "string" || mark.label.length > 200) return null;
      marks.push({ x: mark.x, y: mark.y, label: mark.label });
    }
    return { title: typeof data.title === "string" ? data.title.slice(0, 160) : "Image explanation", src: data.src, after: data.after, marks };
  } catch { return null; }
}
/** `bare` drops the section's own frame so the image explorer can sit inside a
 *  host frame (`ArtifictCanvas`) without a border in a border. */
export function ImageExplanation({ source, onFeedbackDraft, bare = false }: { source: string; onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void; bare?: boolean }) {
  const spec = parseImageSpec(source);
  const before = useImageUrl(spec?.src ?? "");
  const after = useImageUrl(spec?.after ?? "");
  const [split, setSplit] = useState(50);
  const [region, setRegion] = useState({ x: 25, y: 25, width: 50, height: 50 });
  const [targeting, setTargeting] = useState(false);
  if (!spec) return <p role="status">Invalid image explanation. Expected a src, optional after image, and bounded percentage marks.</p>;
  return <section className={`image-explanation${bare ? " bare" : ""}`} aria-label={spec.title} style={bare ? undefined : { border: "1px solid var(--line)", borderRadius: 10, padding: 12, margin: "12px 0" }}>
    <h3>{spec.title}</h3>
    <div style={{ position: "relative", overflow: "hidden" }}>
      {(before.error || after.error) && <p role="status">Image unavailable: {before.error ?? after.error}</p>}
      {before.url && <img src={before.url} alt={spec.after ? "Before" : spec.title} referrerPolicy="no-referrer" style={{ display: "block", width: "100%" }} />}
      {spec.after && after.url && <img src={after.url} alt="After" referrerPolicy="no-referrer" style={{ position: "absolute", inset: 0, width: "100%", height: "100%", objectFit: "contain", clipPath: `inset(0 0 0 ${split}%)` }} />}
      {targeting && <span aria-hidden="true" style={{ position: "absolute", left: `${region.x}%`, top: `${region.y}%`, width: `${Math.min(region.width, 100 - region.x)}%`, height: `${Math.min(region.height, 100 - region.y)}%`, border: "2px solid var(--accent)", pointerEvents: "none" }} />}
      {spec.marks.map((mark, i) => <span key={i} aria-hidden="true" style={{ position: "absolute", left: `${mark.x}%`, top: `${mark.y}%`, background: "var(--accent)", color: "var(--bg)", borderRadius: "50%", padding: "2px 7px", transform: "translate(-50%,-50%)" }}>{i + 1}</span>)}
    </div>
    {spec.after && <label>Before / after split: {split}% <input aria-label="Before after split" type="range" min={0} max={100} value={split} onChange={event => setSplit(Number(event.target.value))} style={{ width: "100%" }} /></label>}
    <ol>{spec.marks.map((mark, i) => <li key={i}>{mark.label}{onFeedbackDraft && <button type="button" onClick={() => onFeedbackDraft({ target: { label: `Image marker ${i + 1}: ${mark.label}` } })}>Discuss marker {i + 1}</button>}</li>)}</ol>
    {onFeedbackDraft && <><button type="button" aria-expanded={targeting} onClick={() => setTargeting(v => !v)}>Select screenshot region</button>{targeting && <fieldset><legend>Region in original image · percentages</legend>{(["x", "y", "width", "height"] as const).map(key => <label key={key}>{key} <input aria-label={`Region ${key}`} type="number" min={0} max={100} value={region[key]} onChange={e => setRegion(r => ({ ...r, [key]: Math.max(0, Math.min(100, Number(e.target.value))) }))} /></label>)}<p>Left {region.x}%, top {region.y}%, width {Math.min(region.width, 100 - region.x)}%, height {Math.min(region.height, 100 - region.y)}%</p><button type="button" onClick={() => onFeedbackDraft({ target: { label: `Screenshot region (${region.x}%, ${region.y}%, ${Math.min(region.width, 100 - region.x)}% × ${Math.min(region.height, 100 - region.y)}%)` }, state: { imageRegion: { ...region, width: Math.min(region.width, 100 - region.x), height: Math.min(region.height, 100 - region.y) } } })}>Stage region for feedback</button></fieldset>}</>}
    <details><summary>Original images and sources</summary><ReplyImage src={spec.src} alt="Original image" />{spec.after && <ReplyImage src={spec.after} alt="Comparison image" />}</details>
  </section>;
}
