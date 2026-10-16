/** Which of the two palettes to paint.
 *
 *  `system` follows the OS rather than snapshotting it once, and `prefersLight`
 *  is a parameter rather than a `matchMedia` call so the choice is testable
 *  without a browser. Anything unknown — including a settings file written
 *  before light mode existed, which has no `mode` key at all — is dark, matching
 *  the backend default. */
export function resolveMode(mode: string | undefined, prefersLight: boolean): "light" | "dark" {
  if (mode === "light") return "light";
  if (mode === "system") return prefersLight ? "light" : "dark";
  return "dark";
}

/** Paint the resolved palette onto the document root.
 *
 *  While the setting means "follow the OS", keep following it: a desktop that
 *  switches at sunset should take the app with it, which is what `system` means
 *  to a user. Returns a teardown for the caller's effect.
 *
 *  `doc`/`win` are injectable so a test can drive it without a real window. */
export function applyMode(
  mode: string | undefined,
  doc: Document = document,
  win: Window = window,
): () => void {
  // `matchMedia` is missing in some test environments; the palette then just
  // falls back to dark rather than throwing on boot.
  const mq = win.matchMedia?.("(prefers-color-scheme: light)") ?? null;
  const paint = () => {
    doc.documentElement.dataset.mode = resolveMode(mode, !!mq?.matches);
  };
  paint();
  if (mode !== "system" || !mq) return () => {};
  mq.addEventListener("change", paint);
  return () => mq.removeEventListener("change", paint);
}
