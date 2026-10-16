import { useEffect, useState } from "react";
import { api } from "../api";
import { get } from "../store";
import { Thumb } from "./primitives";

/** Inline pictures a reply may carry as `data:` URLs. Raster only: an SVG data
 *  URL is inert in an `<img>`, but agents have `openleash-svg` for that, which
 *  also gives the user its source. The cap is the backend's 8 MiB image limit
 *  as base64, so a pasted blob can't be bigger than a file would be allowed. */
const DATA_IMAGE = /^data:image\/(?:png|jpeg|gif|webp);base64,[A-Za-z0-9+/=\s]+$/i;
const MAX_DATA_URL = 12 * 1024 * 1024;

export type ImageSource = { kind: "remote" | "data"; url: string } | { kind: "local"; path: string } | { kind: "blocked" };

/**
 * What a reply image's `src` is. Anything that isn't https, an inline raster
 * `data:` URL or a path is refused outright (`http:` would be blocked by the
 * CSP anyway, and `javascript:` and friends are never images). A path is
 * resolved by the backend against the chat's folder and confined to it, so
 * an agent can show the chart it just rendered without the reply becoming a
 * way to read files from anywhere.
 */
export function imageSource(src: string): ImageSource {
  const s = src.trim();
  if (/^https:\/\//i.test(s)) return { kind: "remote", url: s };
  if (/^data:/i.test(s)) return s.length <= MAX_DATA_URL && DATA_IMAGE.test(s) ? { kind: "data", url: s } : { kind: "blocked" };
  const local = /^file:\/\//i.test(s) || /^[A-Za-z]:[\\/]/.test(s) || !/^[A-Za-z][\w+.-]*:/.test(s);
  if (!local || !s) return { kind: "blocked" };
  // Markdown percent-encodes a path written as `<out/my chart.png>`.
  let path = s;
  try { path = decodeURI(s); } catch { /* keep it as written */ }
  // Same rule as the backend, checked before anything is asked: a path that
  // doesn't even name an image (`file:///etc/passwd`) never leaves the page.
  if (!/\.(?:png|jpe?g|gif|webp)$/i.test(path)) return { kind: "blocked" };
  return { kind: "local", path };
}

/** Resolved local pictures, so a re-render (a streaming reply re-renders every
 *  tick) doesn't re-read the file. Small and evicted oldest-first, because each
 *  entry is a whole image as base64. */
const cache = new Map<string, Promise<string>>();
const CACHE_MAX = 24;
function localImage(cwd: string, path: string): Promise<string> {
  const key = `${cwd}\u0000${path}`;
  let hit = cache.get(key);
  if (!hit) {
    const fresh = api.replyImage(cwd, path);
    // A failure is not remembered: Retry has to really try again. Only drop
    // this request's own entry: a late failure must not evict a newer retry.
    fresh.catch(() => { if (cache.get(key) === fresh) cache.delete(key); });
    cache.set(key, fresh);
    if (cache.size > CACHE_MAX) cache.delete(cache.keys().next().value!);
    hit = fresh;
  }
  return hit;
}

/** Remote pictures remain ordinary images, never HTML in the app's origin. */
/** The URL to put in an `<img>` for any reply-image `src`: https and inline
 *  data as they are, a path once the backend has read it. `error` is the
 *  backend's reason a path couldn't be shown; `retry` re-reads it. Shared by
 *  Markdown pictures and `openleash-image` fences so a path works in both. */
export function useImageUrl(src: string): { source: ImageSource; url: string | null; error: string | null; retry: () => void } {
  const source = imageSource(src);
  const [resolved, setResolved] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const localPath = source.kind === "local" ? source.path : null;
  useEffect(() => {
    if (localPath == null) return;
    let live = true;
    setResolved(null);
    setError(null);
    const s = get();
    const cwd = (s.task ? s.tasks[s.task]?.cwd : undefined) ?? s.settings?.project ?? "";
    localImage(cwd, localPath).then((url) => { if (live) setResolved(url); }).catch((e) => { if (live) setError(String(e)); });
    return () => { live = false; };
  }, [localPath, attempt]);
  const url = source.kind === "local" ? resolved : source.kind === "blocked" ? null : source.url;
  return { source, url, error: source.kind === "local" ? error : null, retry: () => setAttempt((n) => n + 1) };
}

export function ReplyImage({ src, alt = "", title }: { src: string; alt?: string; title?: string }) {
  const { source, url, error, retry: reread } = useImageUrl(src);
  const [broken, setBroken] = useState(false);
  const [loaded, setLoaded] = useState(false);
  if (!src || source.kind === "blocked") return <span className="reply-image-error">{alt || "Image unavailable"}</span>;
  const failed = error ?? (broken ? "" : null);
  const remote = source.kind === "remote";
  const retry = () => { setLoaded(false); setBroken(false); reread(); };
  return <span className="reply-image">
    {failed != null ? <span className="reply-image-error"><span role="status">Image unavailable{alt ? `: ${alt}` : ""}{failed ? ` (${failed})` : ""}</span> <button type="button" onClick={retry}>Retry image</button></span>
      : <>
        {!loaded && <span role="status" className="reply-image-loading">Loading image…</span>}
        {url && <Thumb src={url} alt={alt} loading="lazy" onLoad={() => setLoaded(true)} onError={() => setBroken(true)} style={{ maxWidth: "100%", maxHeight: 560, objectFit: "contain" }} />}
      </>}
    {(title || remote || source.kind === "local") && <span className="reply-image-caption">{title && <span>{title} </span>}{remote && <a href={source.url} target="_blank" rel="noreferrer">Image source</a>}{source.kind === "local" && !title && <span className="mono">{source.path}</span>}</span>}
  </span>;
}
