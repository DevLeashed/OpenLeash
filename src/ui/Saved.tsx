import { useState } from "react";
import { ago, baseName, effortLabel, oneLine, PERMS, SavedPrompt } from "../api";
import { dropPrompt, flash, get, go, loadPrompt, modelInfo, useStore } from "../store";
import { I } from "./icons";
import { Button, Pressable, Tooltip } from "./primitives";

const NO_SAVED: SavedPrompt[] = [];

/** Model + effort, the two options worth showing on a row. */
function ModelChip({ p }: { p: SavedPrompt }) {
  if (!p.model) return null;
  // Read against the model this prompt would run on: its own levels decide
  // which effort names mean anything.
  const effort = p.effort === null ? null : effortLabel(modelInfo(p.model), p.effort);
  return (
    <Tooltip content={`${p.model}${effort ? ` · ${effort} effort` : ""}`}>
      <span className="mono saved-model">{p.model.split("/").pop()}{effort ? ` · ${effort}` : ""}</span>
    </Tooltip>
  );
}

/** The options that don't fit on the row itself. */
function More({ p }: { p: SavedPrompt }) {
  const bits = [
    p.route ? `route: ${p.route}` : null,
    PERMS.find((x) => x.id === p.perm)?.name,
    p.assist,
    p.plan ? "plan" : null,
    p.ultra ? (p.ultra_wt ? "ultrathread · worktrees" : "ultrathread") : null,
    p.worktree ? "new worktree" : null,
    p.branch ? `branch: ${p.branch}` : null,
    (p.agents.length ? p.agents : []).length ? `${p.agents.length} subagents` : null,
    p.images.length ? `${p.images.length} image${p.images.length === 1 ? "" : "s"}` : null,
  ].filter(Boolean);
  return <Tooltip content={bits.join(" · ") || "Default options"}>{I.chev()}</Tooltip>;
}

export function Saved() {
  const list = useStore((s) => s.settings?.saved_prompts ?? NO_SAVED);
  const [arm, setArm] = useState<string | null>(null);
  const project = useStore((s) => s.settings?.project ?? "");
  // Loading a prompt parked in another folder opens that folder, so say so:
  // the composer appearing in a different project is otherwise unexplained.
  const load = (p: SavedPrompt) => {
    const before = get().settings?.project ?? "";
    void loadPrompt(p).then((ok) => {
      if (!ok) return;
      const after = get().settings?.project ?? "";
      flash(after && after !== before
        ? `Opened ${baseName(after)} · prompt loaded, Enter to start it`
        : "Loaded into the composer · Enter to start it");
    });
  };
  return (
    <div style={{ flex: 1, overflow: "auto", padding: "24px 28px" }}>
      <div style={{ maxWidth: 860, margin: "0 auto", display: "flex", flexDirection: "column", gap: 14 }}>
        <div style={{ display: "flex", alignItems: "flex-end", gap: 24, flexWrap: "wrap", animation: "olIn .28s cubic-bezier(.22,1,.36,1) both" }}>
          <div style={{ flex: 1, minWidth: 200 }}>
            <div style={{ fontSize: 20, fontWeight: 600, letterSpacing: "-0.025em" }}>Saved prompts</div>
            <div className="secondary-text" style={{ marginTop: 3 }}>
              {list.length ? `${list.length} waiting to start · click one to load it and its options into the composer` : "Prompts you park from the new-task screen land here"}
            </div>
          </div>
          <Button variant="primary" onClick={() => go("home")}>New task</Button>
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {list.map((p, i) => (
            <div key={p.id} className="savedrow" style={{ animationDelay: Math.min(i, 12) * 16 + "ms" }}
              onMouseLeave={() => setArm(null)}
              onClick={() => load(p)}
              onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); load(p); } }}
              role="button" tabIndex={0}
            >
              <span className="savedglyph">{I.bookmark(13)}</span>
              <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 3 }}>
                <span className="savedtext">{oneLine(p.text, 200)}</span>
                <span style={{ display: "flex", alignItems: "center", gap: 7, fontSize: 11, color: "var(--dim)" }}>
                  <ModelChip p={p} />
                  <More p={p} />
                  <span style={{ fontVariantNumeric: "tabular-nums" }}>{ago(p.created_at)}</span>
                </span>
              </div>
              {p.project && baseName(p.project) !== baseName(project) && (
                <Tooltip content={`Saved from ${p.project} · loading this switches to that folder`}><span className="savedtag">{baseName(p.project)}</span></Tooltip>
              )}
              {arm === p.id ? (
                <Pressable className="saveddel armed" onClick={(e) => { e.stopPropagation(); setArm(null); void dropPrompt(p).then(() => flash("Deleted")); }}>Delete</Pressable>
              ) : (
                <Pressable className="saveddel" onClick={(e) => { e.stopPropagation(); setArm(p.id); }} onFocus={() => setArm(p.id)} aria-label={`Delete saved prompt: ${oneLine(p.text, 40)}`}>{I.trash(12)}</Pressable>
              )}
            </div>
          ))}
        </div>
        {!list.length && (
          <div className="empty">
            Nothing saved yet. Write a prompt on the new-task screen and press the bookmark button next to Send to park it here.
          </div>
        )}
      </div>
    </div>
  );
}
