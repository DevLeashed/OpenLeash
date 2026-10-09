/** Click any image to see it full size: one shared overlay for the whole app,
 *  with arrow keys / Esc to page through the set the image belongs to. */
import { createContext, useCallback, useContext, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronLeft, ChevronRight, X } from "lucide-react";

/** Everything the overlay needs about one image: its src and the set it came from. */
export interface LightboxItem { src: string; alt?: string }

type Open = (items: LightboxItem[], index?: number) => void;
const Ctx = createContext<Open>(() => {});
export const useLightbox = () => useContext(Ctx);

/** Step `dir` through `n` images, wrapping at both ends.
 *
 *  Caller must check `n > 0` first: `n === 0` divides by zero and yields NaN,
 *  which then indexes as a property, not a position. Both call sites here are
 *  behind a length check, and the type-checker cannot see that from here. */
export const cycle = (i: number, dir: number, n: number) => (i + dir + n) % n;

/** An image that opens the lightbox on click (Enter/Space work too). `items` pages
 *  through a whole set — pass the same list plus this image's index to every thumb. */
export function Thumb({ src, alt = "", items, index = 0, style }: { src: string; alt?: string; items?: LightboxItem[]; index?: number; style?: React.CSSProperties }) {
  const open = useLightbox();
  const show = () => open(items?.length ? items : [{ src, alt }], items?.length ? index : 0);
  return (
    <img
      src={src}
      alt={alt}
      className="zoomable"
      style={style}
      role="button"
      tabIndex={0}
      onClick={(e) => { e.stopPropagation(); show(); }}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); e.stopPropagation(); show(); } }}
    />
  );
}

export function LightboxProvider({ children }: { children: React.ReactNode }) {
  const [items, setItems] = useState<LightboxItem[]>([]);
  const [i, setI] = useState(0);
  const open: Open = useCallback((list, index = 0) => { setItems(list); setI(index); }, []);

  useEffect(() => {
    if (!items.length) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setItems([]); return; }
      if (e.key === "ArrowRight") { e.preventDefault(); e.stopPropagation(); setI((n) => cycle(n, 1, items.length)); }
      if (e.key === "ArrowLeft") { e.preventDefault(); e.stopPropagation(); setI((n) => cycle(n, -1, items.length)); }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [items.length]);

  const item = items[i];
  return (
    <Ctx.Provider value={open}>
      {children}
      {!!item && createPortal(
        <div className="lightbox" role="dialog" aria-modal="true" aria-label={item.alt || "Image"} onMouseDown={(e) => { if (e.target === e.currentTarget) setItems([]); }}>
          <button type="button" className="lightbox-x" aria-label="Close" onClick={() => setItems([])}><X size={16} strokeWidth={1.8} /></button>
          {items.length > 1 && <button type="button" className="lightbox-nav prev" aria-label="Previous image" onClick={() => setI((n) => cycle(n, -1, items.length))}><ChevronLeft size={20} strokeWidth={1.8} /></button>}
          <img className="lightbox-img" src={item.src} alt={item.alt || ""} draggable={false} />
          {items.length > 1 && <button type="button" className="lightbox-nav next" aria-label="Next image" onClick={() => setI((n) => cycle(n, 1, items.length))}><ChevronRight size={20} strokeWidth={1.8} /></button>}
          {items.length > 1 && <div className="lightbox-foot"><span className="lightbox-n">{i + 1} / {items.length}</span></div>}
        </div>,
        document.body,
      )}
    </Ctx.Provider>
  );
}
