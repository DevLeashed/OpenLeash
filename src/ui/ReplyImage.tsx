import { useState } from "react";
import { Thumb } from "./primitives";

/** Remote pictures remain ordinary images, never HTML in the app's origin. */
export function ReplyImage({ src, alt = "", title }: { src: string; alt?: string; title?: string }) {
  const [failed, setFailed] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const remote = /^https:\/\//i.test(src);
  if (!src) return <span className="reply-image-error">{alt || "Image unavailable"}</span>;
  return <span className="reply-image">
    {failed ? <span className="reply-image-error"><span role="status">Image unavailable{alt ? `: ${alt}` : ""}</span> <button type="button" onClick={() => { setLoaded(false); setFailed(false); }}>Retry image</button></span>
      : <>
        {!loaded && <span role="status" className="reply-image-loading">Loading image…</span>}
        <Thumb src={src} alt={alt} loading="lazy" onLoad={() => setLoaded(true)} onError={() => setFailed(true)} style={{ maxWidth: "100%", maxHeight: 560, objectFit: "contain" }} />
      </>}
    {(title || remote) && <span className="reply-image-caption">{title && <span>{title} </span>}{remote && <a href={src} target="_blank" rel="noreferrer">Image source</a>}</span>}
  </span>;
}
