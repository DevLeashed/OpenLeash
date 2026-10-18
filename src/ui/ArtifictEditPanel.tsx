import { MousePointerClick, RotateCcw } from "lucide-react";
import { colorToHex, cssValue, EDIT_PROPS, editChanges, pxNumber, type EditProp, type EditSet, type ElementEdit, type InspectPick } from "./artifictEdits";
import { Button, Dropdown, Input, TextArea } from "./primitives";

/** Edit mode's card: the picked element's text and a few styles, changed live
 *  in the preview, then saved as the user's own revision. Nothing here reaches
 *  the agent; a saved edit is a version like any other, marked as yours. */
export function ArtifictEditPanel({ picked, edits, onTip, nextNumber, busy, onText, onStyle, onResetElement, onDiscard, onSave, onGoLatest }: {
  picked: InspectPick | null;
  edits: EditSet;
  onTip: boolean;
  nextNumber: number;
  busy?: boolean;
  onText: (pick: InspectPick, text: string) => void;
  onStyle: (pick: InspectPick, prop: EditProp, value: string | null) => void;
  onResetElement: (selector: string) => void;
  onDiscard: () => void;
  onSave: () => void;
  onGoLatest: () => void;
}) {
  if (!onTip) return <div className="studio-empty-note">
    <p>Edits build on the latest version, so they are made there.</p>
    <Button variant="ghost" onClick={onGoLatest}>Go to latest</Button>
  </div>;
  const changed = Object.values(edits).filter(editChanges);
  const edit: ElementEdit | undefined = picked ? edits[picked.selector] : undefined;
  return <div className="studio-tweaks studio-edit" aria-label="Edit">
    {!picked ? <div className="studio-empty-note">
      <MousePointerClick size={16} aria-hidden="true" />
      <p>Click any element in the design to change its text or style. Changes show live; Save keeps them as a new version.</p>
    </div> : <>
      <div className="studio-comment-target" title={picked.selector}>{picked.label}</div>
      {picked.text !== null ? <div className="studio-tweak">
        <div className="studio-tweak-head"><span className="studio-tweak-label">Text</span></div>
        <TextArea aria-label="Element text" rows={3} maxLength={2000} value={edit?.text ?? picked.text.trim()} onChange={(e) => onText(picked, e.currentTarget.value)} />
      </div> : <p className="studio-fine">This element holds other elements, so only its style can be edited here. Pick the text itself to change words.</p>}
      {EDIT_PROPS.map((def) => {
        const own = edit?.styles[def.prop];
        const current = own ?? picked.styles[def.prop];
        return <div key={def.prop} className="studio-tweak" data-moved={own !== undefined ? "true" : undefined}>
          <div className="studio-tweak-head">
            <span className="studio-tweak-label">{def.label}</span>
            {own !== undefined && <button type="button" className="studio-link" aria-label={`Undo ${def.label}`} onClick={() => onStyle(picked, def.prop, null)}>Undo</button>}
          </div>
          {def.kind === "color" ? <div className="studio-color">
            <input type="color" aria-label={`${def.label} colour`} value={colorToHex(current) ?? "#ffffff"} onChange={(e) => onStyle(picked, def.prop, e.currentTarget.value)} />
            <Input aria-label={def.label} value={own ?? colorToHex(current) ?? current ?? ""} maxLength={40} spellCheck={false} onChange={(e) => { const v = cssValue(def.prop, e.currentTarget.value); if (v) onStyle(picked, def.prop, v); }} />
          </div> : def.kind === "px" ? <Input type="number" aria-label={`${def.label} (px)`} min={def.min} max={def.max} value={pxNumber(current) ?? ""} onChange={(e) => { const v = cssValue(def.prop, e.currentTarget.value); if (v) onStyle(picked, def.prop, v); }} />
          : <Dropdown placeholder={def.label} search={false} value={def.options!.includes(current ?? "") ? current! : ""} onChange={(v) => { if (v) onStyle(picked, def.prop, v); }} style={{ width: "100%", height: 28, padding: "0 8px", fontSize: 12 }}
              options={[...(def.options!.includes(current ?? "") ? [] : [{ value: "", label: current || "—" }]), ...def.options!.map((o) => ({ value: o, label: o }))]} />}
        </div>;
      })}
      {edit && editChanges(edit) && <Button variant="ghost" onClick={() => onResetElement(picked.selector)}><RotateCcw size={12} />Reset this element</Button>}
    </>}
    <div className="studio-tweaks-actions">
      <Button variant="ghost" disabled={!changed.length} onClick={onDiscard}>Discard</Button>
      <Button variant="primary" disabled={busy || !changed.length} onClick={onSave}>Save as v{nextNumber}{changed.length ? ` · ${changed.length}` : ""}</Button>
    </div>
    <p className="studio-fine">Saved edits are your revision: text is replaced where it appears once in the source, styles go into one marked block the agent can fold into the design.</p>
  </div>;
}
