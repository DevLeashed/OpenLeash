import { flash } from "../store";

/**
 * Copy text to the clipboard on a machine whose browser may not have the async
 * clipboard at all.
 *
 * `navigator.clipboard` only exists in a *secure context*. The remote-control UI
 * is deliberately served over plain `http://` on the LAN (see
 * `src-tauri/src/remote/mod.rs`), so on a phone or another computer the property
 * is `undefined`. The tempting `navigator.clipboard?.writeText(t).then(...)`
 * does not degrade gracefully there: optional chaining stops at the property, so
 * the following `.then` runs on `undefined` and throws — the click does nothing
 * at all, with no success and no error, which is exactly how "no copy button
 * works on remote" presents. Every copy in the app goes through here so that
 * one behaviour, and one failure message, covers all of them.
 *
 * The fallback has to be synchronous inside the user's own gesture: iOS and
 * Safari refuse a programmatic copy from a deferred callback, which is why it is
 * not reached by awaiting the modern path first when the modern path is missing.
 */
function selectAndCopy(text: string): boolean {
  if (typeof document === "undefined") return false;
  const area = document.createElement("textarea");
  area.value = text;
  // Read-only keeps the on-screen keyboard away on touch devices, and the
  // offscreen 1px box keeps the page from scrolling to it. A visible selection
  // would flash the text across the UI for the duration of the copy.
  area.setAttribute("readonly", "");
  area.setAttribute("aria-hidden", "true");
  area.style.cssText = "position:fixed;top:0;left:0;width:1px;height:1px;padding:0;border:0;outline:0;opacity:0;pointer-events:none";
  const selection = document.getSelection();
  const previous = selection && selection.rangeCount > 0 ? selection.getRangeAt(0) : null;
  document.body.appendChild(area);
  try {
    area.select();
    // iOS ignores `select()` on a readonly field without an explicit range.
    area.setSelectionRange(0, text.length);
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    area.remove();
    // The user's own selection is theirs: copying a message must not silently
    // clear the text they had highlighted.
    if (previous && selection) {
      selection.removeAllRanges();
      selection.addRange(previous);
    }
  }
}

export interface CopyOptions {
  /** Message on success. */
  what?: string;
  /** Message when both routes fail. */
  failure?: string;
  /** Called with the outcome, for a button that shows its own "Copied" state. */
  onResult?: (ok: boolean) => void;
}

/** Copy `text`, falling back to a selection-based copy where the async
 *  clipboard is unavailable. Returns whether the text reached the clipboard. */
export function copyText(text: string, options: string | CopyOptions = {}): boolean {
  const { what = "Copied", failure = "Couldn't copy", onResult } = typeof options === "string" ? { what: options } as CopyOptions : options;
  const report = (ok: boolean) => {
    onResult?.(ok);
    flash(ok ? what : failure);
    return ok;
  };
  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
    // Fire and forget, but a rejection still falls back: some browsers expose the
    // API and then refuse without focus or permission.
    navigator.clipboard.writeText(text).then(() => report(true), () => report(selectAndCopy(text)));
    return true;
  }
  return report(selectAndCopy(text));
}
