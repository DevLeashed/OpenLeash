import { useEffect, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type { ArtifictFormat } from "./artifictApi";
import { svgImageUrl } from "./hostTheme";
import { useImageUrl } from "./ReplyImage";
import { buildVisualizationDocument, captureTheme } from "./vizDocument";

/** A picture drawn by the host: the same source rules the picture itself uses,
 *  so a card can never show something the artifact would refuse. The card is
 *  named by its title, so the picture here is decorative. */
function ImageThumb({ src }: { src: string }) {
  const { url } = useImageUrl(src.trim());
  if (!url) return null;
  return <img className="artifict-thumb-media" alt="" src={url} referrerPolicy="no-referrer" />;
}

/** What a card shows when it is not the record you are on: the document's own
 *  first words, clamped — a document rather than a grey box. */
function DocumentCard({ excerpt }: { excerpt?: string }) {
  const text = excerpt?.trim();
  return text ? <span className="artifict-doc-text">{text.slice(0, 280)}</span> : null;
}

/** The artifact's own isolated runtime, rendered at a desktop size and scaled
 *  into the card — a miniature of the thing, not a screenshot of it.
 *
 *  Only the card you are on gets one. Running every card would publish a
 *  document per artifact — each one running that artifact's scripts — against a
 *  bounded registry (64 documents / 8 MiB / 30 min) that a large project would
 *  exhaust. One live miniature says what the record is; the rest are its own
 *  words and a picture. */
function LiveFrame({ source, title }: { source: string; title: string }) {
  const [id, setId] = useState<string | null>(null);
  const current = useRef<string | null>(null);
  useEffect(() => {
    let alive = true;
    const release = (token: string) => { void invoke("visualization_release", { id: token }).catch(() => {}); };
    invoke<string>("visualization_publish", { source: buildVisualizationDocument(source, captureTheme()) })
      .then((token) => {
        // Published after the card went away: release it here, or it holds a
        // slot in a bounded registry nothing will ever free.
        if (!alive) { release(token); return; }
        current.current = token;
        setId(token);
      })
      .catch(() => { /* A card that cannot publish shows its document text instead. */ });
    return () => {
      alive = false;
      if (current.current) release(current.current);
      current.current = null;
    };
  }, [source]);
  if (!id) return <DocumentCard />;
  return <iframe
    className="artifict-thumb-frame"
    title={`${title} preview`}
    aria-hidden="true"
    tabIndex={-1}
    sandbox="allow-scripts"
    referrerPolicy="no-referrer"
    src={convertFileSrc(id, "openleash-viz")}
  />;
}

/** One gallery card's picture area. `live` is true for the record the workspace
 *  is on and nothing else. The card itself carries the kind chip, so nothing
 *  here needs to name the format. */
export function ArtifictThumb({ format, content, title, live }: { format: ArtifictFormat; content: string; title: string; live: boolean }) {
  if (format === "html") return live ? <LiveFrame source={content} title={title} /> : <DocumentCard />;
  if (format === "svg") return <img className="artifict-thumb-media" alt="" src={svgImageUrl(content)} />;
  if (format === "image") return <ImageThumb src={content} />;
  return <DocumentCard excerpt={content} />;
}
