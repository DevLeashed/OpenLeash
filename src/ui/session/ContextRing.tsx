import { fmtK } from "../../api";

const RADIUS = 25;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

export interface ContextRingData {
  used: number;
  limit: number;
  remaining: number;
  /** May exceed 100 when provider-reported usage is over the configured limit. */
  percentage: number | null;
  /** The drawn arc is capped at one full ring. */
  fill: number;
}

/** The saved usage is an aggregate from the latest request, not a category trace. */
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

/** A compact, measured used/limit view. It intentionally does not split tokens
 *  into prompt/tool/conversation categories: the usage persisted by the task is
 *  only a total, and the serialized request is not retained for attribution. */
export function ContextRing({ used, limit }: { used: number; limit: number }) {
  const data = contextRingData(used, limit);
  const percent = data.percentage;
  const label = percent == null
    ? `Context usage ${fmtK(data.used)} tokens; context limit unavailable`
    : `${fmtK(data.used)} of ${fmtK(data.limit)} context tokens; ${percent}% used`;
  const color = percent != null && percent > 75 ? "var(--red)" : "var(--violet)";

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
    </div>
  );
}
