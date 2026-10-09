import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";

/**
 * The accent grid. One palette, one "pick something else" control, one custom
 * colour input — previously written out twice: once for sub-agents in the agent
 * dialog, and once more inline as a bare `<input type="color">` over a 9px dot
 * in the model picker, which is why picking a model colour felt like a different,
 * much smaller feature than picking a subagent colour.
 *
 * `value` is whatever is currently in effect, including a family default, so the
 * grid can show the default as selected without the caller having to know what
 * the default is. `reset` is only rendered when there is something to reset to.
 */
export const ACCENTS = ["#33d6ff", "#60a5fa", "#818cf8", "#a78bfa", "#c084fc", "#e879f9", "#f472b6", "#fb7185", "#f87171", "#fb923c", "#fbbf24", "#a3e635", "#4ade80", "#34d399", "#2dd4bf", "#e8e8ea"];

/** `#rrggbb` only: anything else can't go in a colour input's `value`. */
export const asHex = (color: string): string => (/^#[0-9a-fA-F]{6}$/.test(color) ? color : "#a78bfa");

/**
 * A swatch for something being added, so two things added in a row never open on
 * the same colour.
 *
 * Untouched, a new model's accent was its *provider's*, so every model added
 * under one provider came in the same hue — a wall of identical dots in the
 * picker, which is the one list where telling models apart at a glance is the
 * whole job (the provider is already a column right there). Picking from the
 * grid rather than generating a hue keeps the palette the user is offered and
 * the colour they actually get the same set: a random HSL value the picker
 * couldn't re-show as selected would mean the dot in the list and the swatch in
 * the dialog disagreed about what colour this model is.
 *
 * `own` is the colour this would otherwise have fallen back to — passed in rather
 * than looked up, so this stays a pure function of the palette and can be
 * tested without a store behind it. Skipping it matters when it happens to be
 * one of ours: handing a model `#34d399` to sit next to a `codex` `#10a37f` is
 * the same wall of dots in a narrower form. `taken` is the colours already in
 * use, so a run of adds spreads instead of repeating.
 *
 * Walks the grid from a random offset rather than sampling it, because a plain
 * random pick that lands on a taken colour would have to retry from the top
 * anyway, and walking means the search for a free one never dead-ends.
 */
export function randomAccent(own?: string, taken: readonly string[] = []): string {
  const start = Math.floor(Math.random() * ACCENTS.length);
  for (let i = 0; i < ACCENTS.length; i++) {
    const c = ACCENTS[(start + i) % ACCENTS.length]!;
    if (c !== own && !taken.includes(c)) return c;
  }
  // Everything is taken, which needs 16 colours handed out and only 16 exist.
  // Repeat rather than return nothing: no accent is the provider's colour, which
  // is the bug this whole thing exists to fix.
  return ACCENTS[start]!;
}

/**
 * The "anything else" control: the live colour, not a fixed rainbow.
 *
 * It used to paint a conic gradient at every size, which means the one control
 * that exists to show you *what colour you just picked* never showed a colour at
 * all — it showed a wheel, permanently.
 *
 * The paint is live and the write is debounced, and the reason they are two
 * things is React: `onChange` on an `<input type="color">` *is* the native
 * `input` event, which Chromium fires continuously while you drag the hue strip.
 * Handling `onInput` for the paint and `onChange` for the commit therefore looks
 * right and separates nothing — both fire on every step, so a drag was a
 * settings write, and a full settings round-trip through `ColorDot`, per pixel
 * of hue. The dot lagged a step behind the cursor the whole way, which is what
 * "doesn't react well on input" meant.
 *
 * 150ms of quiet is long enough to collapse a drag into one write and short
 * enough that releasing the mouse and reaching for the model dot has landed.
 */
function CustomSwatch({ value, onChange }: { value: string; onChange: (color: string) => void }) {
  const hex = asHex(value);
  const [live, setLive] = useState(hex);
  // Anything the parent handed us wins over what we were dragging towards: the
  // commit came back from settings, and re-showing the pre-commit colour would
  // undo it under the cursor.
  useEffect(() => setLive(hex), [hex]);

  // The pending commit, in a ref rather than state: the flush below has to run on
  // unmount without a re-render, and it has to call the *current* `onChange` — a
  // caller's closure captured when the drag started can be a whole settings
  // object out of date by the time the picker closes.
  const pending = useRef<string | null>(null);
  const commit = useRef(onChange);
  commit.current = onChange;
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const flush = () => {
    clearTimeout(timer.current);
    timer.current = undefined;
    const v = pending.current;
    if (v === null) return;
    pending.current = null;
    commit.current(v);
  };
  const step = (v: string) => {
    setLive(v);
    pending.current = v;
    clearTimeout(timer.current);
    timer.current = setTimeout(flush, 150);
  };
  // A drag that ends with the panel closed, the dialog cancelled or the app
  // shutting down must still land: the debounce would otherwise eat the last
  // colour the user actually chose.
  useEffect(() => () => flush(), []);

  return (
    <label className="swatch custom" title="Custom color">
      <span className="cswatch-fill" aria-hidden="true" style={{ background: live }} />
      {/* Both handlers are the same function on purpose: `onChange` already
          covers Chromium's continuous `input` stream, and a platform that only
          fires `change` when the OS dialog closes is covered by the second
          binding. Running it twice for one event is harmless — it repaints the
          same value and restarts the same timer. */}
      <input
        type="color"
        value={hex}
        onInput={(e) => step(e.currentTarget.value)}
        onChange={(e) => step(e.currentTarget.value)}
        aria-label="Custom color"
      />
    </label>
  );
}

export function ColorPicker({ value, onChange, reset, style }: { value: string; onChange: (color: string) => void; reset?: () => void; style?: CSSProperties }) {
  return (
    <div className="cpicker" style={style}>
      {ACCENTS.map((c) => <button key={c} type="button" aria-label={c} title={c} aria-pressed={value === c} className={"swatch" + (value === c ? " on" : "")} style={{ background: c }} onClick={() => onChange(c)} />)}
      {/* A colour the grid doesn't carry still has to be visible as the current
          one, or saving it looks like it did nothing. */}
      {!ACCENTS.includes(value) && <button type="button" aria-label={value} title={value} aria-pressed className="swatch on" style={{ background: value }} onClick={() => onChange(value)} />}
      <CustomSwatch value={value} onChange={onChange} />
      {reset && <button type="button" className="cpicker-reset" title="Use the default again" onClick={reset}>Reset</button>}
    </div>
  );
}
