/**
 * Keyboard hints, in whatever dialect the host OS actually uses.
 *
 * Shortcuts are written once as `Ctrl K` / `Shift Tab` / `Ctrl Enter`, and
 * rendered as `⌘K` on macOS and `Ctrl K` everywhere else, so a Mac build never
 * asks the user to press a key their keyboard does not have.
 */

/** True on macOS. Resolved once at module load: the platform cannot change. */
export const isMac = typeof navigator !== "undefined" && /mac/i.test(navigator.platform || navigator.userAgent);

const MAC_MODS: Record<string, string> = { ctrl: "⌘", shift: "⇧", alt: "⌥", cmd: "⌘" };

/**
 * `Ctrl K` becomes `⌘K` on macOS, `Ctrl K` elsewhere. Unrecognised text
 * ("/compact", "200k ctx") is returned untouched, so this is safe to call on
 * any hint the UI happens to be holding.
 */
export function chord(text: string | undefined): string | undefined {
  if (!text) return text;
  if (!isMac) return text;
  // A slash command or a bare number is not a chord; splitting on the space
  // would mangle "/compact in src" into two mangled halves.
  if (/^[/0-9]/.test(text)) return text;
  return text.replace(/\b(ctrl|shift|alt|cmd)\b\s*/gi, (word) => MAC_MODS[word.toLowerCase()] ?? word);
}

/**
 * True when a key event came from somewhere the user is typing.
 *
 * Every container that answers bare keys — 1-9 to pick, S to skip, arrows to
 * page — has to ask this first, because those keys are ordinary letters and
 * digits to a text field. The check that was on the non-blocking card instead
 * excluded only the *non*-text inputs (`INPUT && type !== "text"`), which let
 * every plain text box straight through: a note field ate digits as option
 * picks, and typing any word with an "s" in it skipped the question and jumped
 * to the next page — the form appearing to move on by itself mid-sentence.
 *
 * There is no input type for which those keys belong to the form around the
 * field rather than to the field, so the tag is the whole test. It lives here,
 * beside the rest of the keyboard vocabulary, because getting it subtly wrong
 * is silent: nothing throws, and it only shows up as "the card skipped my
 * question".
 */
export function inTextField(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.tagName !== "string") return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable === true;
}
