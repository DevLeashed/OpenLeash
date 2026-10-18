import type { ReactNode } from "react";
import { ChevronRight, type LucideIcon } from "lucide-react";
import type { ConnectorState } from "../connectors";
import { Switch } from "./primitives";

export type PillTone = "ok" | "busy" | "off" | "bad";

/**
 * The one-word state a collapsed connector shows. Derived from the backend
 * status alone, so a collapsed row can't claim "Connected" for a bot that has
 * stopped: an error or an unfinished setup is still visible (in red) without
 * opening anything.
 */
export function connectorPill(s: { enabled: boolean; state: ConnectorState } | null, configured: boolean): { label: string; tone: PillTone } {
  if (!s) return { label: "Checking", tone: "off" };
  if (!s.enabled) return { label: configured ? "Off" : "Not set up", tone: "off" };
  switch (s.state) {
    case "connected": return { label: "Connected", tone: "ok" };
    case "connecting": return { label: "Connecting", tone: "busy" };
    case "reconnecting": return { label: "Reconnecting", tone: "busy" };
    case "incomplete": return { label: "Setup needed", tone: "bad" };
    case "error": return { label: "Error", tone: "bad" };
    default: return { label: "Off", tone: "off" };
  }
}

const TONE: Record<PillTone, { color: string; background: string }> = {
  ok: { color: "var(--green)", background: "rgba(146,170,153,0.16)" },
  busy: { color: "var(--fg2)", background: "var(--ov-40)" },
  off: { color: "var(--mut)", background: "var(--ov-25)" },
  bad: { color: "var(--red)", background: "rgba(233,133,133,0.16)" },
};

/**
 * Settings → Connectors → Chat with your agent: the always-visible row of one
 * platform. The name and pill are a native button, so Enter and Space toggle
 * it and screen readers hear the expanded state and the pill; the switch sits outside it so
 * turning the bot on never also opens or closes the panel.
 */
export function ChatConnectorHeader({ icon: Icon, name, pill, summary, open, onToggle, controls, enabled, onEnable }: {
  icon: LucideIcon;
  name: string;
  pill: { label: string; tone: PillTone };
  /** One line with the detail behind the pill (bot name, the error text). */
  summary: ReactNode;
  open: boolean;
  onToggle: () => void;
  /** Id of the details element this row expands. */
  controls: string;
  enabled: boolean;
  onEnable: (enabled: boolean) => void;
}) {
  const tone = TONE[pill.tone];
  const pillId = `${controls}-pill`;
  return <div className="srowx" style={{ gap: 8 }}>
    <button type="button" aria-label={`${name} details`} aria-describedby={pillId} aria-expanded={open} aria-controls={controls} onClick={onToggle}
      style={{ display: "flex", alignItems: "center", gap: 8, flex: "none", background: "none", border: 0, padding: 0, color: "inherit", font: "inherit", textAlign: "left", cursor: "pointer" }}>
      <ChevronRight aria-hidden="true" size={14} style={{ flex: "none", color: "var(--mut)", transform: open ? "rotate(90deg)" : undefined, transition: "transform .15s" }} />
      <Icon aria-hidden="true" size={16} style={{ flex: "none" }} />
      <span style={{ fontWeight: 500 }}>{name}</span>
      <span id={pillId} data-tone={pill.tone} style={{ fontSize: 11, fontWeight: 500, lineHeight: "18px", padding: "0 7px", borderRadius: 9, whiteSpace: "nowrap", ...tone }}>{pill.label}</span>
    </button>
    <span className="desc" role="status" title={typeof summary === "string" ? summary : undefined} style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", ...(pill.tone === "bad" ? { color: "var(--red)" } : {}) }}>{summary === pill.label ? "" : summary}</span>
    <Switch label={`${name} bot`} checked={enabled} onChange={onEnable} />
  </div>;
}
