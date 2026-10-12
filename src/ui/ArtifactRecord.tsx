import { useState } from "react";
import type { VisualizationFeedbackDraft } from "./visualizationFeedback";
import { ImageExplanation, parseImageSpec } from "./ImageExplanation";

type Ref = { path: string; line?: number };
type Evidence = { command: string; result: string; scope?: string; path?: string };
type Node = { id: string; title: string; description: string; refs: Ref[] };
export type CollaborationRecord = {
  schema: "openleash.collaboration.v1"; title: string; purpose: string;
  stage: "concept" | "prototype" | "implemented" | "verified";
  nodes: Node[]; edges: { from: string; to: string; reason: string }[];
  evidence: Evidence[]; limitations: string[]; image?: string;
};
const text = (v: unknown, max = 2000): v is string => typeof v === "string" && v.length <= max;
export function parseCollaborationRecord(content: string): CollaborationRecord | null {
  try {
    const v = JSON.parse(content);
    if (!v || v.schema !== "openleash.collaboration.v1" || !text(v.title, 160) || !text(v.purpose) || !["concept", "prototype", "implemented", "verified"].includes(v.stage)) return null;
    if (!Array.isArray(v.nodes) || v.nodes.length > 100 || !Array.isArray(v.edges) || v.edges.length > 200 || !Array.isArray(v.evidence) || v.evidence.length > 50 || !Array.isArray(v.limitations) || v.limitations.length > 50 || !v.limitations.every((x: unknown) => text(x))) return null;
    const ids = new Set<string>();
    for (const node of v.nodes) {
      if (!node || !text(node.id, 100) || ids.has(node.id) || !text(node.title, 160) || !text(node.description) || !Array.isArray(node.refs) || node.refs.length > 20) return null;
      ids.add(node.id);
      for (const ref of node.refs) if (!ref || !text(ref.path, 300) || ref.path.startsWith("/") || /(^|[\\/])\.\.([\\/]|$)|^[a-z]:|^[a-z]+:\/\//i.test(ref.path) || (ref.line !== undefined && (!Number.isInteger(ref.line) || ref.line < 1))) return null;
    }
    for (const edge of v.edges) if (!edge || !ids.has(edge.from) || !ids.has(edge.to) || !text(edge.reason)) return null;
    for (const e of v.evidence) if (!e || !text(e.command) || !text(e.result) || (e.scope !== undefined && !text(e.scope)) || (e.path !== undefined && !text(e.path, 300))) return null;
    if (v.image !== undefined && (!text(v.image, 240000) || !parseImageSpec(v.image))) return null;
    return v as CollaborationRecord;
  } catch { return null; }
}
export function ArtifactRecord({ record, onDraft }: { record: CollaborationRecord; onDraft?: (draft: VisualizationFeedbackDraft) => void }) {
  const [selected, setSelected] = useState(record.nodes[0]?.id ?? "");
  const node = record.nodes.find(n => n.id === selected);
  const target = (label: string) => onDraft?.({ target: { label: label.slice(0, 300) } });
  return <section aria-label="Collaboration record" style={{ border: "1px solid var(--line)", borderRadius: 10, padding: 12 }}>
    <h3>{record.title}</h3><p>{record.purpose}</p>
    <p><b>Recorded stage: {record.stage}</b> · supplied by the author, not independently certified by OpenLeash.</p>
    {record.image && <ImageExplanation source={record.image} onFeedbackDraft={onDraft} />}
    {!!record.nodes.length && <><label>Explore component <select aria-label="Architecture component" value={selected} onChange={e => setSelected(e.target.value)}>{record.nodes.map(n => <option key={n.id} value={n.id}>{n.title}</option>)}</select></label>
    {node && <div><h4>{node.title}</h4><p>{node.description}</p><ul>{node.refs.map((r, i) => <li key={i}><code>{r.path}{r.line ? `:${r.line}` : ""}</code></li>)}</ul><button type="button" disabled={!onDraft} onClick={() => target(`Component ${node.id}: ${node.title}`)}>Discuss this component</button></div>}
    <ul>{record.edges.filter(e => e.from === selected || e.to === selected).map((e, i) => <li key={i}><button type="button" disabled={!onDraft} onClick={() => target(`Dependency ${e.from} → ${e.to}: ${e.reason}`)}>{e.from} → {e.to}</button> — {e.reason}</li>)}</ul></>}
    <h4>Implementation and verification record</h4>{!record.evidence.length && <p>No verification evidence recorded.</p>}
    {record.evidence.map((e, i) => <details key={i}><summary>{e.result}</summary><pre style={{ whiteSpace: "pre-wrap" }}>{e.command}</pre><p>Scope: {e.scope || "Not recorded"}</p>{e.path && <code>{e.path}</code>}</details>)}
    <h4>Remaining limitations</h4><ul>{record.limitations.map((l, i) => <li key={i}>{l}</li>)}</ul>
  </section>;
}
