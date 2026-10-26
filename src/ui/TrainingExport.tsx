import { useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { api } from "../api";
import { flash, set, useStore } from "../store";
import { Button, Modal } from "./primitives";
import { I } from "./icons";

/** The dataset shapes the two targets ask for. */
export type TrainingFormat = "openrouter" | "claude";

const FORMATS: { value: TrainingFormat; label: string; hint: string }[] = [
  { value: "openrouter", label: "OpenRouter / OpenAI-style", hint: "System prompt as the first message inside \"messages\"." },
  { value: "claude", label: "Claude (Anthropic)", hint: "System prompt in the top-level \"system\" field; user and assistant turns alternate." },
];

/**
 * Write this chat as a fine-tuning dataset.
 *
 * The ordinary export keeps everything the app knows about a chat, which is
 * what a backup wants and the opposite of what a trainer wants: a trainer
 * reads the conversation and nothing else. So this keeps the visible text of
 * the user and assistant turns, drops tool traffic and hidden reasoning, and
 * carries the project instructions the agent worked under as the system prompt.
 */
export async function exportTraining(taskId: string, format: TrainingFormat) {
  const path = await save({
    defaultPath: `openleash-training-${new Date().toISOString().slice(0, 10)}.jsonl`,
    filters: [{ name: "JSON Lines", extensions: ["jsonl"] }],
  }).catch((e) => { flash(String(e)); return null; });
  if (!path) return;
  try {
    await api.exportTraining(taskId, path, format);
    flash("Training data exported");
  } catch (e) {
    flash(String(e));
  }
}

/** Mounted once beside the other overlays; shows while `trainingExport` names a chat. */
export function TrainingExport() {
  const id = useStore((s) => s.trainingExport);
  if (!id) return null;
  return <TrainingFormatDialog key={id} taskId={id} onClose={() => set({ trainingExport: null })} />;
}

/** The picker for the two dataset shapes. */
export function TrainingFormatDialog({ taskId, onClose }: { taskId: string; onClose: () => void }) {
  const [format, setFormat] = useState<TrainingFormat>("openrouter");
  const chosen = FORMATS.find((f) => f.value === format)!;
  return (
    <Modal onClose={onClose} ariaLabel="Export training data" style={{ width: "min(520px, calc(100vw - 24px))" }}>
      <div className="mh">
        <span style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>Export training data</span>
        <Button variant="ghost" onClick={onClose}>Close</Button>
      </div>
      <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        <div className="desc" style={{ lineHeight: 1.5 }}>
          One JSONL file, one conversation per line: the visible text of every user and assistant turn, plus the project instructions as the system prompt. Tool calls and hidden reasoning are left out — a text-only trainer cannot represent them.
        </div>
        <div role="radiogroup" aria-label="Dataset format" style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {FORMATS.map((f) => (
            <label key={f.value} style={{ display: "flex", gap: 8, alignItems: "flex-start", padding: 9, borderRadius: 8, cursor: "pointer", background: format === f.value ? "rgba(255,255,255,0.06)" : "transparent" }}>
              <input type="radio" name="training-format" value={f.value} checked={format === f.value}
                onChange={() => setFormat(f.value)}
                style={{ marginTop: 2, accentColor: "var(--accent)" }} />
              <span style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                <span style={{ fontWeight: 500 }}>{f.label}</span>
                <span className="desc" style={{ lineHeight: 1.45 }}>{f.hint}</span>
              </span>
            </label>
          ))}
        </div>
        <div className="desc" style={{ fontSize: 11.5 }}>Saved as <span className="mono">.jsonl</span> — one JSON object per line, ready for the training script.</div>
      </div>
      <div className="mf">
        <Button variant="ghost" onClick={onClose}>Cancel</Button>
        <Button variant="primary" onClick={() => { onClose(); void exportTraining(taskId, format); }}>
          {I.download(12)} Export {chosen.label.split(" ")[0]}
        </Button>
      </div>
    </Modal>
  );
}
