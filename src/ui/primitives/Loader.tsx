import { createContext, useContext } from "react";
import type { CSSProperties } from "react";

/** Color for "agent is working" spinners below this point: the model doing the work
 *  (a chat's model in its session, a subagent's in its panel/pill). */
export const AgentColor = createContext<string | undefined>(undefined);

type LoaderVariant = "general" | "agent";

export function Loader({ variant = "general", size = 14, label, className = "", color: explicit }: { variant?: LoaderVariant; size?: number; label?: string; className?: string; color?: string }) {
  const inherited = useContext(AgentColor);
  const color = explicit ?? inherited;
  // Keep dimensions and artwork local: injected library styles can arrive after
  // the SVG mounts, leaving an enormous, still glyph during conversation loading.
  const glyph = variant === "agent"
    ? <svg aria-hidden="true" width={size} height={size} viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="2.5" style={{ color: color ?? "var(--violet)" }}>
      <rect x="1.25" y="1.25" width="17.5" height="17.5" rx="4" opacity="0.2" />
      <rect className="ld-trace-spin" x="1.25" y="1.25" width="17.5" height="17.5" rx="4" strokeDasharray="16 47.1327" strokeLinecap="round" />
    </svg>
    : <svg aria-hidden="true" width={size} height={size} viewBox="0 0 24 24" fill="none" style={{ color: "var(--mut2)" }}>
      <circle className="loader-arc-spin" cx="12" cy="12" r="10" stroke="currentColor" strokeDasharray="18 44.8" strokeLinecap="round" strokeWidth="2.5" />
    </svg>;

  return (
    <span className={["loader", `loader-${variant}`, className].filter(Boolean).join(" ")} role={label ? "status" : undefined} aria-label={label} style={{ "--loader-size": `${size}px` } as CSSProperties}>
      {glyph}
      {label && <span className="loader-label">{label}</span>}
    </span>
  );
}
