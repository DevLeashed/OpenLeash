import { RotateCcw, Sparkles } from "lucide-react";
import { changedTweaks, hexColor, type Tweak, type TweakValue } from "./artifictTweaks";
import { Button, Input } from "./primitives";

/** The design's own knobs, as the agent declared them. Moving one changes the
 *  live preview only; "Apply" turns the moved values into a request so the
 *  agent can make them the design's defaults. */
export function ArtifictTweaksPanel({ tweaks, values, onChange, onReset, onApply, busy }: {
  tweaks: Tweak[];
  values: Record<string, TweakValue>;
  onChange: (name: string, value: TweakValue) => void;
  onReset: () => void;
  /** Absent when nothing is listening to send the values to (a studio borrowed onto the new-task screen). */
  onApply?: () => void;
  busy?: boolean;
}) {
  const changed = changedTweaks(tweaks, values);
  if (!tweaks.length) return <div className="studio-empty-note">
    <Sparkles size={16} aria-hidden="true" />
    <p>This design has no tweaks yet. Ask the agent for a few — accent colour, corner radius, density — and they appear here as live controls.</p>
  </div>;
  return <div className="studio-tweaks" aria-label="Tweaks">
    {tweaks.map((tweak) => {
      const value = tweak.name in values ? values[tweak.name]! : tweak.value;
      const moved = value !== tweak.value;
      return <div key={tweak.name} className="studio-tweak" data-moved={moved ? "true" : undefined}>
        <div className="studio-tweak-head">
          <span className="studio-tweak-label">{tweak.label}</span>
          <span className="studio-tweak-value">{tweak.type === "range" ? `${Number(value)}${tweak.unit}` : tweak.type === "toggle" ? (value ? "On" : "Off") : tweak.type === "color" ? String(value) : ""}</span>
        </div>
        <TweakControl tweak={tweak} value={value} onChange={(next) => onChange(tweak.name, next)} />
      </div>;
    })}
    <div className="studio-tweaks-actions">
      <Button variant="ghost" disabled={!changed.length} onClick={onReset}><RotateCcw size={12} />Reset</Button>
      {onApply && <Button variant="primary" disabled={busy || !changed.length} onClick={onApply}>Apply {changed.length || ""} to design</Button>}
    </div>
    <p className="studio-fine">{onApply ? "Tweaks change this preview only. Applying sends the values to the agent as a request to make them the design's defaults." : "Tweaks change this preview only."}</p>
  </div>;
}

function TweakControl({ tweak, value, onChange }: { tweak: Tweak; value: TweakValue; onChange: (value: TweakValue) => void }) {
  switch (tweak.type) {
    case "range":
      return <input type="range" className="studio-range" aria-label={tweak.label} min={tweak.min} max={tweak.max} step={tweak.step} value={Number(value)} onChange={(e) => onChange(Number(e.currentTarget.value))} />;
    case "color": {
      const hex = hexColor(String(value));
      return <div className="studio-color">
        {hex && <input type="color" aria-label={`${tweak.label} colour`} value={hex} onChange={(e) => onChange(e.currentTarget.value)} />}
        <Input aria-label={tweak.label} value={String(value)} maxLength={80} spellCheck={false} onChange={(e) => onChange(e.currentTarget.value)} />
      </div>;
    }
    case "select":
      return <div className="studio-choices" role="radiogroup" aria-label={tweak.label}>
        {tweak.options.map((option) => <button key={option.value} type="button" role="radio" aria-checked={value === option.value} className={value === option.value ? "on" : undefined} onClick={() => onChange(option.value)}>{option.label}</button>)}
      </div>;
    case "toggle":
      return <button type="button" role="switch" aria-label={tweak.label} aria-checked={!!value} className={`studio-switch${value ? " on" : ""}`} onClick={() => onChange(!value)}><span /></button>;
    case "text":
      return <Input aria-label={tweak.label} value={String(value)} maxLength={200} onChange={(e) => onChange(e.currentTarget.value)} />;
  }
}
