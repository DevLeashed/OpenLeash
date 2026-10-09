import { Fragment, useEffect, useState } from "react";
import { ChevronDown, ChevronRight, Plus, Trash2, X } from "lucide-react";
import { api, type McpServerCfg, type McpStatus } from "../api";
import { saveSettings, useStore } from "../store";
import { Button, ChoiceChip, IconButton, Input, Loader, Modal, MorphText, RemovableChip, Segmented, Switch, Tooltip } from "./primitives";

function AddMcpDialog({ existing, onClose }: { existing: McpServerCfg[]; onClose: () => void }) {
  const [name, setName] = useState("");
  const [transport, setTransport] = useState<"stdio" | "http">("stdio");
  const [commandLine, setCommandLine] = useState("");
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const duplicate = existing.some((m) => m.name.toLowerCase() === name.trim().toLowerCase());
  // Underscores collide with the MCP tool-name delimiter.
  const badName = /[^A-Za-z0-9-]/.test(name.trim()) ? "Names can use letters, digits and dashes only — an underscore collides with MCP tool names." : "";
  const ready = !!name.trim() && !badName && !duplicate && (transport === "http" ? /^https?:\/\/\S+$/i.test(url.trim()) : !!commandLine.trim());
  const add = async () => {
    if (!ready || saving) return;
    setSaving(true); setError("");
    try {
      const defaults = { name: name.trim(), env: {}, enabled: true, oauth: false, auto_approve: [] };
      const parts = commandLine.trim().match(/(?:[^\s"]+|"[^"]*")+/g) ?? [];
      const cfg: McpServerCfg = transport === "http"
        ? { ...defaults, command: "", args: [], transport, url: url.trim(), headers: token.trim() ? { Authorization: token.trim().startsWith("Bearer ") ? token.trim() : `Bearer ${token.trim()}` } : {} }
        : { ...defaults, command: (parts[0] ?? "").replace(/^"|"$/g, ""), args: parts.slice(1).map((a) => a.replace(/^"|"$/g, "")), transport, url: "", headers: {} };
      if (!cfg.command && transport === "stdio") throw new Error("Enter a command to run.");
      if (await saveSettings({ mcp: [...existing, cfg] })) onClose();
      else setError("Couldn't save the server. Please try again.");
    } catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  };
  return <Modal onClose={onClose} style={{ width: "min(500px, 100%)" }}>
    <div className="mh"><span style={{ flex: 1 }}>Add MCP server</span><IconButton label="Close" onClick={onClose}><X size={16} /></IconButton></div>
    <div className="mb">
      <div className="field"><label>Name</label><Input aria-label="Server name" autoFocus value={name} placeholder="GitHub" onChange={(e) => setName(e.currentTarget.value)} /></div>
      <div className="field"><label>Transport</label><Segmented label="Transport" value={transport} onChange={(v) => setTransport(v as "stdio" | "http")} options={[{ value: "stdio", label: "Command" }, { value: "http", label: "URL" }]} /><div className="desc" style={{ marginTop: 6 }}>{transport === "http" ? "A remote server that speaks the streamable-HTTP transport — how hosted MCP servers are published." : "A program on this machine, spoken to over stdio."}</div></div>
      {transport === "http" ? <>
        <div className="field"><label>Server URL</label><Input aria-label="Server URL" className="input mono" value={url} placeholder="https://mcp.context7.com/mcp" onChange={(e) => setUrl(e.currentTarget.value)} onKeyDown={(e) => e.key === "Enter" && void add()} /></div>
        <div className="field"><label>Access token</label><Input aria-label="Access token" className="input mono" type="password" value={token} placeholder="Optional — sent as an Authorization header" onChange={(e) => setToken(e.currentTarget.value)} /></div>
        <div className="settings-note">Leave the token blank for a public server. “Bearer ” is added for you if you leave it off.</div>
      </> : <div className="field"><label>Command and arguments</label><Input aria-label="Command and arguments" className="input mono" value={commandLine} placeholder="npx -y @modelcontextprotocol/server-github" onChange={(e) => setCommandLine(e.currentTarget.value)} onKeyDown={(e) => e.key === "Enter" && void add()} /></div>}
      <div className="settings-note">Adding enables this server. Only add servers you trust: local commands run on this machine; remote servers receive tool inputs. Tools still require the existing permissions.</div>
      {duplicate && <div className="settings-note">A server with that name already exists.</div>}
      {badName && <div className="settings-note" role="alert">{badName}</div>}
      {error && <div className="settings-note" role="alert">{error}</div>}
    </div>
    <div className="mf"><div style={{ flex: 1 }} /><Button variant="ghost" onClick={onClose}>Cancel</Button><Button variant="primary" disabled={!ready || saving} onClick={add}>{saving ? "Adding…" : "Add server"}</Button></div>
  </Modal>;
}

const MCP_STATES: Record<string, { text: string; color: string; dot: string; busy?: boolean }> = {
  connected: { text: "tools", color: "var(--green2)", dot: "var(--green)" },
  starting: { text: "Starting", color: "var(--mut2)", dot: "var(--mut2)", busy: true },
  error: { text: "Error", color: "var(--red)", dot: "var(--red)" },
  off: { text: "Off", color: "var(--dim)", dot: "#55555c" },
};
function McpState({ st }: { st?: McpStatus }) {
  if (!st) return <span style={{ fontSize: 11, color: "var(--dim)", whiteSpace: "nowrap" }}>Waiting</span>;
  const state = MCP_STATES[st.status] ?? { text: st.status, color: "var(--mut3)", dot: "#55555c" };
  return <Tooltip content={st.error} side="left"><span style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 11, color: state.color, whiteSpace: "nowrap" }}>
    {state.busy ? <Loader size={11} /> : <span aria-hidden="true" style={{ width: 7, height: 7, borderRadius: "50%", background: state.dot, flex: "none" }} />}
    <MorphText>{st.status === "connected" ? `${st.tools} tool${st.tools === 1 ? "" : "s"}` : state.text}</MorphText>
  </span></Tooltip>;
}
function mcpToolError(t: string): string {
  if (!t) return "Enter a tool name.";
  if (t.includes("*")) return "A wildcard isn't allowed — pre-approval is one exact tool at a time.";
  if (/\s/.test(t)) return "A tool name can't contain spaces.";
  if (t.startsWith("mcp__")) return "Give the tool's own name (e.g. create_issue), not the full mcp__ name.";
  return "";
}
function McpAutoApprove({ cfg, status, onChange }: { cfg: McpServerCfg; status?: McpStatus; onChange: (next: string[], removed?: string) => void }) {
  const [draft, setDraft] = useState("");
  const [err, setErr] = useState("");
  const approved = cfg.auto_approve ?? [];
  const known = status?.tool_names ?? [];
  const add = (tool: string) => {
    const t = tool.trim(); const bad = mcpToolError(t);
    if (bad) { setErr(bad); return; }
    setErr(""); setDraft("");
    if (!approved.includes(t)) onChange([...approved, t]);
  };
  return <div style={{ padding: "10px 14px 12px", borderTop: "1px solid var(--ov-50)", background: "var(--ov-20)" }}>
    <div className="desc" style={{ marginBottom: 8 }}>Approve these tools without asking. Each applies to one exact tool on <span className="mono">{cfg.name}</span> — it never covers the rest of the server, and plan mode still blocks it. Removing one revokes it immediately.</div>
    {approved.length > 0 && <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginBottom: 8 }}>{approved.map((t) => <RemovableChip key={t} label={t} onRemove={() => onChange(approved.filter((x) => x !== t), t)}>{t}</RemovableChip>)}</div>}
    {known.filter((t) => !approved.includes(t)).length > 0 && <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginBottom: 8 }}>{known.filter((t) => !approved.includes(t)).map((t) => <ChoiceChip key={t} selected={false} onClick={() => add(t)} hint={`Pre-approve ${t}`}>{t}</ChoiceChip>)}</div>}
    <div style={{ display: "flex", gap: 8, alignItems: "center" }}><Input className="input mono" style={{ flex: 1, fontSize: 12 }} value={draft} placeholder="tool name, e.g. read_thing" onChange={(e) => { setDraft(e.currentTarget.value); setErr(""); }} onKeyDown={(e) => { if (e.key === "Enter") add(draft); }} /><Button variant="ghost" disabled={!draft.trim()} onClick={() => add(draft)}>Pre-approve</Button></div>
    {err && <div className="settings-note" role="alert" style={{ marginTop: 6 }}>{err}</div>}
  </div>;
}

export function McpServers({ custom = false }: { custom?: boolean }) {
  const s = useStore((st) => st.settings);
  const [status, setStatus] = useState<McpStatus[]>([]);
  const [adding, setAdding] = useState(false);
  const [open, setOpen] = useState<Record<string, boolean>>({});
  useEffect(() => {
    let prev = "";
    const load = () => api.mcpStatus().then((next) => {
      const key = JSON.stringify(next.map((m) => [m.name, m.status, m.tools, m.error, m.tool_names]));
      if (key !== prev) { prev = key; setStatus(next); }
    }).catch(() => {});
    void load(); const iv = setInterval(load, 3000);
    return () => clearInterval(iv);
  }, []);
  if (!s) return null;
  const save = (mcp: McpServerCfg[]) => saveSettings({ mcp });
  return <>
    <div className="settings-head"><div className="settings-head-main settings-title">{custom ? "Custom plugins" : "MCP servers"}</div><Button variant="ghost" onClick={() => void api.mcpReconnect()}>Reconnect</Button><Button onClick={() => setAdding(true)}><Plus size={12} />{custom ? "Add custom plugin" : "Add MCP"}</Button></div>
    {custom && <div className="desc">Add a local command or remote URL using MCP. These are the same servers managed in MCP settings; adding one does not pre-approve its tools.</div>}
    {s.mcp.length ? <div className="sgroup">{s.mcp.map((m, i) => {
      const st = status.find((x) => x.name === m.name);
      const approved = m.auto_approve ?? [];
      const expanded = !!open[m.name];
      const setCfg = (patch: Partial<McpServerCfg>) => save(s.mcp.map((x, j) => j === i ? { ...x, ...patch } : x));
      return <Fragment key={m.name}>
        <div className="srowx">
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>{m.name}{m.transport === "http" && <span className="desc" style={{ marginLeft: 6 }}>URL</span>}{approved.length > 0 && <span className="desc" style={{ marginLeft: 6 }}>{approved.length} pre-approved</span>}</div><Tooltip content={st?.error}><div className="desc mono" style={{ whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{st?.error ?? st?.desc ?? (m.transport === "http" ? m.url : `${m.command} ${m.args.join(" ")}`)}</div></Tooltip></div>
          <McpState st={st} />
          <IconButton label={`${expanded ? "Hide" : "Show"} tools to pre-approve for ${m.name}`} style={{ width: 24, height: 24 }} onClick={() => setOpen((o) => ({ ...o, [m.name]: !o[m.name] }))}>{expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}</IconButton>
          <Switch label={`${m.enabled ? "Disable" : "Enable"} ${m.name}`} checked={m.enabled} onChange={(enabled) => setCfg({ enabled })} />
          <IconButton label={`Remove ${m.name}`} style={{ width: 24, height: 24 }} onClick={() => save(s.mcp.filter((_, j) => j !== i))}><Trash2 size={12} /></IconButton>
        </div>
        {expanded && <McpAutoApprove cfg={m} status={st} onChange={(next, removed) => {
          const mcp = s.mcp.map((x, j) => j === i ? { ...x, auto_approve: next } : x);
          // Backend sync only adds grants: retract the exact allow rule in the same save.
          if (removed) void saveSettings({ mcp, allow: s.allow.filter((r) => r.pattern !== `mcp__${m.name}__${removed} *`) });
          else void save(mcp);
        }} />}
      </Fragment>;
    })}</div> : <div className="empty">{custom ? "No custom plugins yet." : "No MCP servers yet."}</div>}
    <div className="settings-note">Tools ask permission unless Full access, a pre-approved tool, or an allow rule applies. A URL server is reached over the streamable-HTTP transport; a command server runs as a local process.</div>
    {adding && <AddMcpDialog existing={s.mcp} onClose={() => setAdding(false)} />}
  </>;
}
