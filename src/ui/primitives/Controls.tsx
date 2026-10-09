import { forwardRef, useEffect, useState } from "react";
import type { CSSProperties, ComponentProps, KeyboardEvent, MouseEvent, ReactNode } from "react";
import { X } from "lucide-react";
import { chord } from "../../keys";
import { zoom } from "../../store";
import { Tooltip } from "./Tooltip";

export interface Choice<T extends string | number> { value: T; label: string; hint?: string; disabled?: boolean }

/**
 * A `url(...)` token for a CSS property, from a data URL the backend produced.
 * The quotes matter: a bare interpolation stops at the first `)` or newline, and
 * a data URL can carry both. JSON string syntax is a subset of CSS string syntax,
 * so quoting this way also escapes the `"`/`\` a filename could contain.
 */
export const cssUrl = (src: string | undefined) => (src ? `url(${JSON.stringify(src)})` : "none");

type ActionProps = Omit<ComponentProps<"button">, "children" | "onClick" | "type"> & { children: ReactNode; onClick: (event: MouseEvent<HTMLButtonElement>) => void };

/**
 * A keyboard hint. Write the chord in words ("Ctrl K", "Shift Tab"); it renders
 * in the host's dialect, so a Mac build shows ⌘K instead of a key that is not
 * on the keyboard. Non-chord text ("/compact", "200k ctx") passes through.
 */
export function Kbd({ children, className = "", style }: { children: string; className?: string; style?: CSSProperties }) {
  return <span className={"kbd" + (className ? ` ${className}` : "")} style={style}>{chord(children)}</span>;
}

export const Input = forwardRef<HTMLInputElement, ComponentProps<"input">>(function Input({ className = "input", ...props }, ref) {
  return <input {...props} ref={ref} className={className} />;
});

export const TextArea = forwardRef<HTMLTextAreaElement, ComponentProps<"textarea">>(function TextArea({ className = "input", ...props }, ref) {
  return <textarea {...props} ref={ref} className={className} />;
});

export function Button({ children, onClick, variant = "default", disabled = false, style, className = "", ...rest }: ActionProps & { variant?: "default" | "primary" | "ghost" }) {
  return <button {...rest} type="button" className={`btn${variant === "default" ? "" : ` ${variant}`}${className ? ` ${className}` : ""}`} disabled={disabled} style={style} onClick={onClick}>{children}</button>;
}

export function ChipButton({ children, onClick, style, className = "", ...rest }: ActionProps) {
  return <button {...rest} type="button" className={`chip${className ? ` ${className}` : ""}`} style={style} onClick={onClick}>{children}</button>;
}

export function TextButton({ children, onClick, style, className = "", link = false, ...rest }: ActionProps & { link?: boolean }) {
  return <button {...rest} type="button" className={`text-button${link ? " link" : ""}${className ? ` ${className}` : ""}`} style={style} onClick={onClick}>{children}</button>;
}

export function MenuButton({ children, onClick, style, className = "", ...rest }: ActionProps) {
  return <button {...rest} type="button" className={`cbtn${className ? ` ${className}` : ""}`} style={style} onClick={onClick}>{children}</button>;
}

export function MenuRow({ children, onClick, style, className = "", ...rest }: ActionProps) {
  return <button {...rest} type="button" className={`mrow${className ? ` ${className}` : ""}`} style={style} onClick={onClick}>{children}</button>;
}

// `disabled` is dropped from `ActionProps` too: nothing ever disables a send
// button, and leaving it accepted would mean a `disabled` that silently does nothing.
export function RoundButton({ children, label, onClick, className = "" }: Omit<ActionProps, "disabled"> & { label: string }) {
  return <Tooltip content={label}><button type="button" aria-label={label} className={`send${className ? ` ${className}` : ""}`} onClick={onClick}>{children}</button></Tooltip>;
}

export function WindowButton({ children, label, onClick, close = false }: ActionProps & { label: string; close?: boolean }) {
  return <button type="button" aria-label={label} className={`wc${close ? " close" : ""}`} onClick={onClick}>{children}</button>;
}

export function Pressable({ children, onClick, onKeyDown, tabIndex = 0, ...rest }: Omit<ComponentProps<"div">, "onClick"> & { onClick: (event: MouseEvent<HTMLDivElement>) => void }) {
  return <div {...rest} role="button" tabIndex={tabIndex} onClick={onClick} onKeyDown={(event) => { onKeyDown?.(event); if (event.defaultPrevented || event.target !== event.currentTarget) return; if (event.key === "Enter" || event.key === " ") { event.preventDefault(); event.currentTarget.click(); } }}>{children}</div>;
}

/**
 * A transparent click-catcher behind a menu. Menus are portalled out of the
 * zoomed app root, so the scrim has to carry the same zoom — otherwise it
 * covers a different part of the screen than the menu, and the menu renders at
 * the wrong size. `layer` keeps the whole pair above the popovers it was opened
 * from: the titlebar's project list sits at 60, and a context menu opened
 * inside one has to paint over it, not behind it.
 */
export function MenuScrim({ onClick, onContextMenu, layer, style }: { onClick: (event: MouseEvent<HTMLDivElement>) => void; onContextMenu?: (event: MouseEvent<HTMLDivElement>) => void; layer?: number; style?: CSSProperties }) {
  // The zoom lives in CSS, so it has to be read rather than tracked by hand.
  const [z, setZ] = useState(1);
  useEffect(() => setZ(zoom()), []);
  return <div className="scrim" role="presentation" style={{ zoom: z, zIndex: layer, ...style }} onClick={onClick} onContextMenu={onContextMenu} />;
}

/** The app root is zoomed, so a menu portalled to the body has to be too. */
export const menuLayer = (zoom: number): CSSProperties => ({ position: "fixed", inset: 0, zoom, zIndex: 90 });

/**
 * Where a context menu goes: at the cursor, turned back onto the screen when it
 * would hang off an edge. `h` is the menu's height — a guess until it has been
 * measured, which is enough to decide whether it opens upwards.
 *
 * When the menu is taller than the window there is no edge to turn onto, so the
 * cap has to be a real scroll rather than a clip: the menu would otherwise just
 * end, with every row past the fold rendered but unreachable. `maxHeight` alone
 * did nothing here, because `.pop` sets `overflow: visible` and wins over the
 * inline style. Callers that can carry long content (the account menu, whose
 * email row is user data) pair this with `overflowY: auto`, and the `up`
 * calculation uses the capped height so an oversized menu opens from the top
 * instead of being pushed off it.
 */
export function ctxPlace(x: number, y: number, w: number, h: number, z: number): CSSProperties {
  const vw = window.innerWidth / z, vh = window.innerHeight / z;
  const height = Math.min(h, Math.max(120, vh - 16));
  const up = y / z + height + 8 > vh;
  return {
    left: Math.max(8, Math.min(x / z, vw - w - 8)),
    top: up ? "auto" : y / z,
    bottom: up ? (window.innerHeight - y) / z + 6 : "auto",
    width: w,
    maxHeight: Math.max(120, vh - 16),
    overflowY: "auto",
  };
}

export function IconButton({ children, label, onClick, disabled = false, style, className = "", tooltipSide = "top" }: { children: ReactNode; label: string; onClick: (event: MouseEvent<HTMLButtonElement>) => void; disabled?: boolean; style?: CSSProperties; className?: string; tooltipSide?: "top" | "bottom" | "left" | "right" }) {
  return <Tooltip content={label} side={tooltipSide}><button type="button" className={`ico${className ? ` ${className}` : ""}`} aria-label={label} disabled={disabled} style={style} onClick={onClick}>{children}</button></Tooltip>;
}

export function NavTab({ selected, onClick, children, style }: { selected: boolean; onClick: () => void; children: ReactNode; style?: CSSProperties }) {
  return <button type="button" role="tab" style={style} aria-selected={selected} className={"stab" + (selected ? " on" : "")} onClick={onClick}>{children}</button>;
}

export function Segmented<T extends string | number>({ options, value, onChange, label, style, quiet = false }: { options: readonly Choice<T>[]; value: T; onChange: (value: T) => void; label: string; style?: CSSProperties; quiet?: boolean }) {
  const index = Math.max(0, options.findIndex((option) => option.value === value));
  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, current: number) => {
    const direction = event.key === "ArrowRight" || event.key === "ArrowDown" ? 1 : event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 0;
    if (!direction && event.key !== "Home" && event.key !== "End") return;
    event.preventDefault();
    const enabled = options.map((option, i) => !option.disabled ? i : -1).filter((i) => i >= 0);
    if (!enabled.length) return;
    const at = enabled.indexOf(current);
    // Every branch lands on an index `enabled` holds, and we returned above when
    // it holds none: Home and End take the ends, and the modulo is in range.
    const next = event.key === "Home" ? enabled[0]! : event.key === "End" ? enabled[enabled.length - 1]! : enabled[(at + direction + enabled.length) % enabled.length]!;
    onChange(options[next]!.value);
    (event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("button")[next])?.focus();
  };
  return (
    <div className={"seg" + (quiet ? " quiet" : "")} style={{ width: Math.max(210, options.length * 100), ...style }} role="radiogroup" aria-label={label}>
      <div className="knob" aria-hidden="true" style={{ width: `calc((100% - 4px) / ${options.length})`, transform: `translateX(${index * 100}%)` }} />
      {options.map((option, i) => (
        <Tooltip key={option.value} content={option.hint}>
          <button type="button" role="radio" aria-label={option.label} aria-checked={option.value === value} disabled={option.disabled} tabIndex={option.value === value ? 0 : -1} className={"o" + (option.value === value ? " on" : "")} onClick={() => onChange(option.value)} onKeyDown={(event) => onKeyDown(event, i)}>{option.label}</button>
        </Tooltip>
      ))}
    </div>
  );
}

export function Switch({ checked, onChange, label, hint, small = false }: { checked: boolean; onChange?: (checked: boolean, event: MouseEvent<HTMLButtonElement>) => void; label: string; hint?: string; small?: boolean }) {
  const className = "toggle" + (small ? " sm" : "") + (checked ? " on" : "");
  return onChange
    ? <Tooltip content={hint}><button type="button" role="switch" aria-label={label} aria-checked={checked} className={className} onClick={(event) => onChange(!checked, event)}><span /></button></Tooltip>
    : <span aria-hidden="true" className={className}><span /></span>;
}

export function ChoiceMark({ variant, style }: { variant: "radio" | "check"; style?: CSSProperties }) {
  return <span aria-hidden="true" className={variant} style={style}><span /></span>;
}

export function ChoiceChip({ selected, onClick, children, hint, locked = false, checkbox = false }: { selected: boolean; onClick: () => void; children: ReactNode; hint?: string; locked?: boolean; checkbox?: boolean }) {
  return (
    <Tooltip content={hint}>
      <button type="button" aria-pressed={selected} aria-disabled={locked} className={"chipbox" + (selected ? " on" : "") + (locked ? " locked" : "")} onClick={() => { if (!locked) onClick(); }}>
        {checkbox && <ChoiceMark variant="check" />}{children}
      </button>
    </Tooltip>
  );
}

export function ChoiceRow({ selected, onClick, variant, children, className = "opt", style }: { selected: boolean; onClick: () => void; variant: "radio" | "check"; children: ReactNode; className?: string; style?: CSSProperties }) {
  return <div role={variant === "radio" ? "radio" : "checkbox"} aria-checked={selected} tabIndex={0} className={className + (selected ? " on" : "")} style={style} onClick={onClick} onKeyDown={(e: KeyboardEvent<HTMLDivElement>) => { if (e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); onClick(); } }}><ChoiceMark variant={variant} />{children}</div>;
}

export function RemovableChip({ children, onRemove, label }: { children: ReactNode; onRemove: () => void; label: string }) {
  return <span className="chipbox sm on head">{children}<button type="button" className="x" aria-label={`Remove ${label}`} onClick={onRemove}><X size={10} strokeWidth={2} /></button></span>;
}
