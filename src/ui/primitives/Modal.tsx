import { CSSProperties, ReactNode, useEffect, useRef } from "react";
import { createPortal } from "react-dom";

/** Anything the keyboard can land on, in the order Tab would visit it. */
const FOCUSABLE = "a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex=\"-1\"])";
const stops = (root: HTMLElement) => [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) => el.getClientRects().length > 0);

export function Modal({ children, onClose, style, className, ariaLabel, ariaLabelledBy }: { children: ReactNode; onClose: () => void; style?: CSSProperties; className?: string; ariaLabel?: string; ariaLabelledBy?: string }) {
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (document.querySelector(".ddpop")) return;
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  // `aria-modal` only tells a screen reader the rest of the window is inert — it does
  // nothing for the keyboard, which would happily walk out behind the scrim. So: land
  // focus inside on open, keep Tab on it, and hand focus back to whatever opened us.
  // Mount/unmount only: onClose is a fresh arrow on every render, so depending on it
  // would re-trap (and steal focus back) in the middle of the conversation.
  useEffect(() => {
    const was = document.activeElement as HTMLElement | null;
    // A frame later, so autoFocus fields that mount with the portal get first refusal.
    const id = window.setTimeout(() => {
      const root = panel.current;
      if (!root) return;
      if (root.contains(document.activeElement) && document.activeElement !== document.body) return;
      (stops(root)[0] ?? root).focus();
    }, 0);
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      // A dropdown owns the keyboard while it is open; it is portalled outside the dialog.
      if (document.querySelector(".ddpop")) return;
      const root = panel.current;
      if (!root) return;
      const items = stops(root);
      if (!items.length) { e.preventDefault(); root.focus(); return; }
      const first = items[0]!, last = items[items.length - 1]!, active = document.activeElement;
      if (!root.contains(active)) { e.preventDefault(); (e.shiftKey ? last : first).focus(); return; }
      if (e.shiftKey ? active === first : active === last) { e.preventDefault(); (e.shiftKey ? last : first).focus(); }
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.clearTimeout(id);
      window.removeEventListener("keydown", onKey, true);
      was?.focus?.();
    };
  }, []);

  return createPortal(
    <div className="modal-scrim" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div ref={panel} className={className ? `modal ${className}` : "modal"} role="dialog" aria-modal="true" aria-label={ariaLabel} aria-labelledby={ariaLabelledBy} tabIndex={-1} style={style}>
        {children}
      </div>
    </div>,
    document.body,
  );
}
