// Folder trust is a one-time exact-folder decision, with discovery evidence first.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { api, baseName, TrustManifest, TrustState } from "../api";
import { flash, set, useStore } from "../store";
import { Button, IconButton, Modal } from "./primitives";
import { AlertTriangle, Bot, FileText, FolderCheck, Globe, Info, Plug, Server, Shield, Sparkles, Terminal, X } from "lucide-react";

const li = (C: typeof Shield, size = 14) => <C aria-hidden="true" size={size} strokeWidth={1.7} />;
function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} kB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function useManifest(path: string) {
  const [result, setResult] = useState<{ path: string; manifest: TrustManifest | null; error: string }>({ path: "", manifest: null, error: "" });
  useEffect(() => {
    if (!path) return;
    let live = true;
    api.trustManifest(path)
      .then((manifest) => { if (live) setResult({ path, manifest, error: "" }); })
      .catch((e) => { if (live) setResult({ path, manifest: null, error: String(e) }); });
    return () => { live = false; };
  }, [path]);
  return result.path === path ? result : { manifest: null, error: "" };
}

function Section({ icon, title, count, hint, children }: { icon: ReactNode; title: string; count: number; hint?: string; children: ReactNode }) {
  if (!count) return null;
  return <div className="sgroup" style={{ borderRadius: 10, overflow: "hidden" }}>
    <div style={{ padding: "8px 12px 4px", display: "flex", alignItems: "center", gap: 8, color: "var(--mut2)", fontSize: 11, fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.04em" }}>
      <span style={{ display: "flex", flex: "none" }}>{icon}</span>
      <span style={{ flex: 1 }}>{title}{count > 1 ? ` · ${count}` : ""}</span>
      {hint && <span style={{ textTransform: "none", letterSpacing: 0, fontWeight: 400, color: "var(--dim)" }}>{hint}</span>}
    </div>{children}
  </div>;
}
function Row({ children }: { children: ReactNode }) {
  return <div className="srowx" style={{ minHeight: 38, alignItems: "flex-start" }}>{children}</div>;
}
const WARN_COLOR: Record<string, string> = { danger: "var(--red)", warn: "var(--amber, #fbbf24)", info: "var(--mut2)" };
function Warnings({ m }: { m: TrustManifest }) {
  const shown = new Set<string>();
  const uniq = m.warnings.filter((w) => !shown.has(w.text) && (shown.add(w.text), true));
  if (!uniq.length) return null;
  return <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>{uniq.map((w, i) => (
    <div key={i} role={w.level === "danger" ? "alert" : undefined} style={{ display: "flex", gap: 8, alignItems: "flex-start", fontSize: 11.5, lineHeight: 1.45, color: w.level === "info" ? "var(--hint)" : "var(--fg2)" }}>
      <span style={{ flex: "none", marginTop: 1, color: WARN_COLOR[w.level] ?? "var(--mut2)" }}>{w.level === "danger" ? li(AlertTriangle, 13) : w.level === "warn" ? li(Terminal, 13) : li(Info, 13)}</span><span>{w.text}</span>
    </div>
  ))}</div>;
}

/** Discovery evidence shown before deciding, including instruction previews. */
export function ManifestView({ m, compact = false }: { m: TrustManifest; compact?: boolean }) {
  const named = m.instructions.filter((f) => !f.pointer_only);
  const empty = !named.length && !m.skills.length && !m.agents.length && !m.hooks.length && !m.mcp.length && !m.overrides.length;
  return <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
    <Warnings m={m} />
    {empty && <div className="desc" style={{ fontSize: 11.5, color: "var(--hint)" }}>This folder has nothing project-local to inject — no instructions, skills, agents, hooks or MCP config.</div>}
    <Section icon={li(FileText, 13)} title="Instructions" count={named.length} hint={compact ? undefined : "injected into every agent's system prompt"}>
      {named.map((f) => <Row key={f.path}><div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontWeight: 500, display: "flex", alignItems: "center", gap: 6 }}><span className="mono" style={{ fontSize: 12 }}>{f.name}</span>{f.global && <span className="chip" style={{ height: 18, padding: "0 6px", fontSize: 10, color: "var(--mut2)" }}>{li(Globe, 10)} your file</span>}{f.truncated && <span style={{ fontSize: 10, color: "var(--amber, #fbbf24)" }}>truncated</span>}</div>
        <div className="desc mono" style={{ fontSize: 10.5, wordBreak: "break-all" }}>{f.path}</div>
        {!compact && f.preview.trim() && <div className="mono" style={{ fontSize: 10.5, color: "var(--mut2)", marginTop: 4, maxHeight: 60, overflow: "hidden", whiteSpace: "pre-wrap", borderLeft: "2px solid var(--ov-90)", paddingLeft: 8 }}>{f.preview.slice(0, 240)}</div>}
      </div><span style={{ fontSize: 11, color: "var(--dim)", whiteSpace: "nowrap" }}>{fmtBytes(f.bytes)} · {f.chars.toLocaleString()} chars</span></Row>)}
    </Section>
    <Section icon={li(Sparkles, 13)} title="Skills" count={m.skills.length} hint="name + description reach the prompt">{m.skills.map((s) => <Row key={s.path}><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }} className="mono">{s.name}</div><div className="desc" style={{ fontSize: 11 }}>{s.description}</div></div>{s.files > 0 && <span style={{ fontSize: 11, color: "var(--dim)" }}>{s.files} file{s.files === 1 ? "" : "s"}</span>}</Row>)}</Section>
    <Section icon={li(Bot, 13)} title="Sub-agents" count={m.agents.length}>{m.agents.map((a) => <Row key={a.path}><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>{a.name} <span className="mono" style={{ color: "var(--dim)", fontSize: 11 }}>{a.id}</span></div><div className="desc" style={{ fontSize: 11 }}>{a.description}</div></div><span className="chip" style={{ height: 18, padding: "0 6px", fontSize: 10, color: "var(--mut2)" }}>{a.tools === "read_only" ? "read-only" : a.tools === "no_shell" ? "no shell" : "all tools"}</span></Row>)}</Section>
    <Section icon={li(Terminal, 13)} title="Hooks" count={m.hooks.length} hint="shell commands">{m.hooks.map((h, i) => <Row key={i}><div style={{ flex: 1, minWidth: 0 }}><div className="mono" style={{ fontSize: 11.5, wordBreak: "break-all" }}>{h.command}</div><div className="desc" style={{ fontSize: 10.5 }}>{h.source} · {h.event || "tool"}{h.matcher ? ` · /${h.matcher}/` : ""}</div></div></Row>)}</Section>
    <Section icon={li(Server, 13)} title="MCP servers" count={m.mcp.length} hint="programs that would start">{m.mcp.map((s, i) => <Row key={i}><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>{s.name} <span className="desc" style={{ fontSize: 10.5 }}>{s.transport}</span></div><div className="desc mono" style={{ fontSize: 10.5, wordBreak: "break-all" }}>{s.command}</div></div></Row>)}</Section>
    <Section icon={li(Plug, 13)} title="Setting overrides" count={m.overrides.length}>{m.overrides.map((o, i) => <Row key={i}><div style={{ flex: 1, minWidth: 0 }}><div className="mono" style={{ fontSize: 11.5 }}>{o.key}: {o.value}</div><div className="desc" style={{ fontSize: 10.5 }}>{o.file}</div></div></Row>)}</Section>
  </div>;
}

export function TrustDialog() {
  const open = useStore((s) => s.trustPrompt);
  const project = useStore((s) => s.settings?.project ?? "");
  const { manifest: m, error } = useManifest(open ? project : "");
  const writing = useRef(false);
  const [saving, setSaving] = useState(false);
  if (!open) return null;

  const decide = async (decision: TrustState) => {
    if ((!m && decision === "trusted") || writing.current) return;
    // A ref guards repeated clicks/dismissals before React renders disabled buttons.
    writing.current = true;
    setSaving(true);
    try {
      const settings = await api.trustSet(project, "folder", decision);
      set({ settings, trustPrompt: false });
      flash(decision === "trusted" ? `Trusted ${baseName(project)}` : `Not trusted · ${baseName(project)}`);
    } catch (e) {
      flash(String(e));
    } finally {
      writing.current = false;
      setSaving(false);
    }
  };
  // Refusal is safe even while discovery is loading; granting trust needs evidence.
  const close = () => { void decide("untrusted"); };
  return <Modal onClose={close} style={{ width: "min(620px, 100%)" }}>
    <div className="mh">{li(FolderCheck, 15)}<span style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>Trust <span className="mono">{baseName(project) || project}</span>?</span><IconButton label="Close" disabled={saving} onClick={close}>{li(X, 16)}</IconButton></div>
    <div className="mb">
      <div className="desc" style={{ fontSize: 11.5, lineHeight: 1.5, color: "var(--hint)" }}>Trust only folders you wrote or reviewed. Trusting loads the instructions, skills, agents, hooks and MCP config below; otherwise project-local content stays blocked.</div>
      {error && <div className="settings-note" role="alert">{error}</div>}
      {!m && !error && <div className="desc" style={{ fontSize: 11.5 }}>Looking at what this folder would inject…</div>}
      {m && <><div className="settings-section-label" style={{ marginTop: 0 }}>What trusting it would load</div><ManifestView m={m} /></>}
    </div>
    <div className="mf"><span style={{ fontSize: 11, color: "var(--dim)" }}>Remembered once for this folder only.</span><div style={{ flex: 1 }} /><Button variant="ghost" disabled={!m || saving} onClick={() => void decide("untrusted")}>Don't trust</Button><Button variant="primary" disabled={!m || saving} onClick={() => void decide("trusted")}>Trust folder</Button></div>
  </Modal>;
}
