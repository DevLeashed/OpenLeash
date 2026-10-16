import { fmtK, type ContextParts } from "../../api";

const RADIUS = 25;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

/** The categories the backend can attribute, in the order we draw them.
 *
 *  A fixed order matters: the panel is read at a glance while a turn runs, and
 *  ordering by size would make the rows reshuffle every time a number moved. */
const PART_ROWS: { key: keyof ContextParts; label: string; color: string }[] = [
  { key: "system", label: "System prompt", color: "var(--violet)" },
  { key: "instructions", label: "Project instructions", color: "var(--cyan)" },
  { key: "skills", label: "Skills & memory", color: "var(--green)" },
  { key: "tools", label: "Tool schemas", color: "var(--hl-attr)" },
  { key: "attachments", label: "Attachments", color: "var(--hl-literal)" },
  { key: "history", label: "Conversation", color: "var(--hl-title)" },
  { key: "summary", label: "Summary", color: "var(--dim)" },
];

export interface ContextPartRow {
  key: keyof ContextParts;
  label: string;
  color: string;
  value: number;
  /** This category's share of the measured request, 0–1. */
  share: number;
}

/** The rows worth drawing. A category the request did not use is noise rather
 *  than information — "Skills & memory 0" tells nobody anything, and the summary
 *  row only ever exists after a compaction. Returns nothing at all when every
 *  bucket is zero, so a caller can treat an empty list as "no breakdown". */
export function contextPartRows(parts: ContextParts | undefined): ContextPartRow[] {
  if (!parts) return [];
  const value = (k: keyof ContextParts) => {
    const v = parts[k];
    return Number.isFinite(v) ? Math.max(0, v) : 0;
  };
  const total = PART_ROWS.reduce((n, r) => n + value(r.key), 0);
  if (total <= 0) return [];
  return PART_ROWS.filter((r) => value(r.key) > 0).map((r) => ({
    ...r,
    value: value(r.key),
    share: value(r.key) / total,
  }));
}

export interface ContextRingData {
  used: number;
  limit: number;
  remaining: number;
  /** May exceed 100 when provider-reported usage is over the configured limit. */
  percentage: number | null;
  /** The drawn arc is capped at one full ring. */
  fill: number;
}

/** The saved usage is an aggregate from the latest request, not a category trace.
 *
 *  The ring itself is provider truth; the category split lives beside it in
 *  `contextPartRows`, which is an estimate by construction. */
export function contextRingData(used: number, limit: number): ContextRingData {
  const safeUsed = Number.isFinite(used) ? Math.max(0, used) : 0;
  const safeLimit = Number.isFinite(limit) ? Math.max(0, limit) : 0;
  const ratio = safeLimit > 0 ? safeUsed / safeLimit : 0;
  return {
    used: safeUsed,
    limit: safeLimit,
    remaining: Math.max(0, safeLimit - safeUsed),
    percentage: safeLimit > 0 ? Math.round(ratio * 100) : null,
    fill: Math.min(1, ratio),
  };
}

/** A compact, measured used/limit view.
 *
 *  The ring is provider truth — the exact prompt size the provider counted. The
 *  breakdown under it is the *shape* of that number in categories, which no
 *  provider reports: it is measured from the request the harness assembled and
 *  rescaled so the parts sum to the ring. Both are worth showing together —
 *  "84% full" is a deadline, "84% full and 60% of it is tool schemas" is
 *  something the user can act on — but the split is the half that is an
 *  estimate, so it is drawn only for the user who asks for it: `breakdown`
 *  comes from Settings → General → Context breakdown and is off by default. */
export function ContextRing({ used, limit, parts, breakdown = false }: { used: number; limit: number; parts?: ContextParts; breakdown?: boolean }) {
  const data = contextRingData(used, limit);
  const percent = data.percentage;
  const label = percent == null
    ? `Context usage ${fmtK(data.used)} tokens; context limit unavailable`
    : `${fmtK(data.used)} of ${fmtK(data.limit)} context tokens; ${percent}% used`;
  const color = percent != null && percent > 75 ? "var(--red)" : "var(--violet)";
  // Off, `parts` is never consulted: the measured pair is the whole meter.
  const rows = breakdown ? contextPartRows(parts) : [];

  return (
    <div className="ctx-usage" aria-label="Context window usage">
      <div className="ctx-ring" role="img" aria-label={label}>
        <svg viewBox="0 0 64 64" aria-hidden="true">
          <circle className="ctx-ring-track" cx="32" cy="32" r={RADIUS} />
          {percent != null && <circle
            className="ctx-ring-used"
            cx="32" cy="32" r={RADIUS}
            stroke={color}
            strokeDasharray={CIRCUMFERENCE}
            strokeDashoffset={CIRCUMFERENCE * (1 - data.fill)}
            transform="rotate(-90 32 32)"
          />}
        </svg>
        <span>{percent == null ? "—" : `${percent}%`}</span>
      </div>
      <div className="ctx-legend">
        <div className="ctx-legend-row"><i className="ctx-swatch used" /><span>Used</span><b>{fmtK(data.used)}</b></div>
        <div className="ctx-legend-row"><i className="ctx-swatch remaining" /><span>Remaining</span><b>{data.limit ? fmtK(data.remaining) : "—"}</b></div>
        <div className="ctx-limit">{data.limit ? `${fmtK(data.limit)} token limit` : "Context limit unavailable"}</div>
      </div>
      {rows.length > 0 && (
        <div className="ctx-breakdown">
          {rows.map((r) => (
            <div className="ctx-breakdown-row" key={r.key}>
              <i className="ctx-swatch" style={{ background: r.color }} />
              <span className="ctx-breakdown-label">{r.label}</span>
              <b>{fmtK(r.value)}</b>
              <span className="ctx-breakdown-bar" aria-hidden="true">
                <i style={{ width: `${Math.round(r.share * 100)}%`, background: r.color }} />
              </span>
            </div>
          ))}
          <div className="ctx-note">Estimated split, scaled to the provider's prompt count.</div>
        </div>
      )}
    </div>
  );
}
