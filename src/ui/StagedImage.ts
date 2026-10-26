// A picture the transcript no longer holds, read back from OpenLeash's own
// staging folder.
//
// The transcript keeps a message's picture blobs on the item's `images` field
// and drops them from older messages to bound what a long chat holds in memory
// (`prune_item_images` in the backend). The picture is not lost: the backend
// staged a copy of it on disk when the message was sent, and that copy is
// still there. This reads it back, so a picture the user sent stays a picture
// in the transcript instead of turning into a paperclip chip with a size.
//
// The read is confined to OpenLeash's staging folder by the backend
// (`staged_image_url`), because the path comes from the transcript and the
// transcript is data the agent can put words into.
import { useEffect, useState } from "react";
import { api } from "../api";

/** How many staged pictures are read at once. A message can carry a handful,
 *  and each one is a whole image as base64. */
const CACHE_MAX = 24;

/** Read pictures back, and remember them: a transcript re-renders on every
 *  status, step and cost event, and re-reading the file each time would read
 *  the same image over and over. */
const cache = new Map<string, Promise<string>>();

function stagedImage(path: string): Promise<string> {
  let hit = cache.get(path);
  if (!hit) {
    const fresh = api.stagedImageUrl(path);
    // A failure is not remembered, so a retry really tries again — a copy that
    // is missing now may be there later, and the user should not be left with a
    // picture that can never come back. Only this request's own entry is
    // dropped, so a late failure cannot evict a newer retry.
    const forget = () => { if (cache.get(path) === fresh) cache.delete(path); };
    fresh.then((url) => { if (!url) forget(); }, forget);
    cache.set(path, fresh);
    if (cache.size > CACHE_MAX) cache.delete(cache.keys().next().value!);
    hit = fresh;
  }
  return hit;
}

/** What is known about one staged picture: the URL once it has been read, and
 *  whether the read has failed. A failure is final — the copy is gone from
 *  disk — so the caller can fall back to naming it instead of drawing it. */
export interface StagedPicture { url: string | null; failed: boolean }

/** How long to wait before trying a failed read again. The copy may not be
 *  there yet — a chat opened while the staging folder was still being written,
 *  or a read that failed for a moment — and a picture that can come back
 *  should not be written off on the first refusal. */
const RETRY_MS = 1500;

/** The staged pictures at `paths`, read back in the same order.
 *
 *  `paths` are the staged copies of the pictures whose blob the transcript no
 *  longer holds. Each one comes back as a URL, or as a failure once the read
 *  has settled; while it is in flight the picture is simply not drawn yet.
 *
 *  A read that failed is tried again once: the copy may still turn up, and a
 *  picture the user sent is worth one more attempt. */
export function useStagedImages(paths: string[]): StagedPicture[] {
  const key = paths.join(" ");
  const [pictures, setPictures] = useState<StagedPicture[]>(() => paths.map(() => ({ url: null, failed: false })));
  useEffect(() => {
    let live = true;
    let retry: ReturnType<typeof setTimeout> | undefined;
    // A different list of paths means different pictures, so the previous
    // answers are dropped rather than shown against the wrong picture.
    setPictures(paths.map(() => ({ url: null, failed: false })));
    if (!paths.length) return () => { live = false; };
    const read = (attempt: number) => {
      void Promise.all(paths.map(async (p) => {
        try {
          const url = await stagedImage(p);
          return typeof url === "string" && url ? { url, failed: false } : { url: null, failed: true };
        } catch {
          return { url: null, failed: true };
        }
      })).then((out) => {
        if (!live) return;
        setPictures(out);
        // Only a picture that is still missing is worth another try, and only
        // once: a copy that is truly gone stays gone.
        if (attempt === 0 && out.some((p) => p.failed)) {
          retry = setTimeout(() => read(1), RETRY_MS);
        }
      });
    };
    read(0);
    return () => { live = false; if (retry) clearTimeout(retry); };
    // `key` is the list's identity: the paths joined, so a re-render with the
    // same paths does not read them again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return pictures;
}
