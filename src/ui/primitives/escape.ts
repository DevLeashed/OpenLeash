/** Escape closes this thing, and must beat the global key handler.
 *
 * The app binds Escape on `window` (see `onKey` in `src/App.tsx`) to pause a
 * running chat, then stop it if already paused. Every popup here also closes on
 * Escape, so its listener has to run first *and* stop the event there — a
 * capture-phase listener that forgot `stopPropagation` would let the same press
 * both dismiss a dropdown and freeze the agent behind it.
 *
 * Centralised because the two halves are easy to get wrong independently: the
 * capture flag (without it the global handler, bound at mount, sees the event
 * first) and the propagation stop (without it both fire).
 */
import { useEffect } from "react";

/** `open` gates the listener, so a closed popup costs nothing. */
export function useEscapeClose(open: boolean, close: () => void) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      close();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, close]);
}
