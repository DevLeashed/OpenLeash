import { useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, type McpServerCfg, type McpStatus } from "../api";
import { flash, saveSettings, set, useStore } from "../store";
import { Button, Input } from "./primitives";

// Match verified official endpoints; OAuth never pre-approves MCP tools.
interface ConnectorPreset { name: string; docs: string; note: string; url?: string; client?: boolean; redirectPort?: number; local?: boolean; token?: string; scopes?: string[]; workiq?: boolean }
const APPS: ConnectorPreset[] = [
  { name: "Slack", docs: "https://docs.slack.dev/ai/slack-mcp-server/", url: "https://mcp.slack.com/mcp", client: true, redirectPort: 42817, scopes: ["search:read.public", "channels:history", "channels:read", "users:read"], note: "Requires your registered internal or Marketplace Slack app with public PKCE enabled. Enter its client ID; register http://127.0.0.1:42817/callback as its redirect URI. Requests public-channel search/history, channel listings and basic user profiles only; no private-channel, direct-message or write access. Workspace admin approval may be required." },
  { name: "Notion", docs: "https://developers.notion.com/docs/get-started-with-mcp", url: "https://mcp.notion.com/mcp", local: true, scopes: ["default"], token: "Notion integration token", note: "Sign in to the official hosted Notion server. The legacy local token integration remains available and requires Node.js/npm." },
  { name: "Linear", docs: "https://linear.app/docs/mcp", url: "https://mcp.linear.app/mcp", token: "Linear API key", scopes: ["read"], note: "Official hosted MCP supports a Linear API key as a bearer token. Prefer a restricted key; browser OAuth requests read-only access." },
  { name: "Google Workspace", docs: "https://developers.google.com/workspace/guides/configure-mcp-servers", url: "https://gmailmcp.googleapis.com/mcp/v1", client: true, scopes: ["https://www.googleapis.com/auth/gmail.readonly"], note: "Workspace MCP (Developer Preview membership required). Requires a Google Cloud project, the selected service enabled and your public/native OAuth client ID. Requests only the selected service's read-only scopes, not every advertised scope." },
  { name: "Microsoft 365", docs: "https://github.com/microsoft/work-iq", workiq: true, note: "Official Microsoft Work IQ. Connecting downloads and runs @microsoft/workiq using Node.js/npm. Requires a Microsoft 365 Copilot license and tenant/admin consent. The CLI handles Microsoft sign-in on first access; review Microsoft's license/EULA in the official docs. Tools still require your normal permissions." },
  { name: "Atlassian", docs: "https://github.com/atlassian/atlassian-mcp-server", url: "https://mcp.atlassian.com/v2/mcp", token: "Atlassian service account API key", scopes: ["read:me", "read:account", "offline_access", "read:jira:agent-interface", "search:jira:agent-interface", "read:confluence:agent-interface", "search:confluence:agent-interface"], note: "Official hosted MCP supports service account bearer API keys when your organization admin enables API-token authentication. Personal API tokens require Basic auth in custom MCP settings. Browser OAuth requests read-only Jira/Confluence access; token authentication remains optional." },
];

const GOOGLE_SERVICES = [
  { name: "Gmail", host: "gmailmcp", scopes: ["gmail.readonly"] },
  { name: "Drive", host: "drivemcp", scopes: ["drive.readonly"] },
  { name: "Docs", host: "docsmcp", scopes: ["drive.readonly", "documents.readonly"] },
  { name: "Sheets", host: "sheetsmcp", scopes: ["drive.readonly", "spreadsheets.readonly"] },
  { name: "Slides", host: "slidesmcp", scopes: ["drive.readonly", "presentations.readonly"] },
  { name: "Calendar", host: "calendarmcp", scopes: ["calendar.calendarlist.readonly", "calendar.events.freebusy", "calendar.events.readonly"] },
  { name: "Chat", host: "chatmcp", scopes: ["chat.spaces.readonly", "chat.memberships.readonly", "chat.messages.readonly"] },
  { name: "People", host: "people", scopes: ["directory.readonly", "userinfo.profile", "contacts.readonly"] },
].map((service) => ({ ...service, url: `https://${service.host}.googleapis.com/mcp/v1`, scopes: service.scopes.map((scope) => `https://www.googleapis.com/auth/${scope}`) }));

export function AppConnectors() {
  const settings = useStore((s) => s.settings);
  const [status, setStatus] = useState<McpStatus[]>([]);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [clients, setClients] = useState<Record<string, string>>({});
  const [googleService, setGoogleService] = useState("Gmail");
  const [busy, setBusy] = useState(false);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [checkError, setCheckError] = useState("");
  const [authMessage, setAuthMessage] = useState("");
  const [pending, setPending] = useState("");
  const flow = useRef<string | null>(null);
  const alive = useRef(true);
  const generation = useRef(0);
  useEffect(() => { alive.current = true; return () => { alive.current = false; if (flow.current) void api.mcpOauthCancel(flow.current).catch(() => {}); }; }, []);
  const refresh = async () => {
    // Read backend state after OAuth; never save the pre-login settings snapshot.
    const fresh = await api.boot();
    set({ settings: fresh.settings });
    if (alive.current) setStatus(await api.mcpStatus());
  };
  const cancel = async () => {
    generation.current++;
    const id = flow.current;
    flow.current = null;
    if (id) await api.mcpOauthCancel(id).catch(() => flash("Unable to cancel sign-in"));
    if (alive.current) { setPending(""); setBusy(false); setAuthMessage("Sign-in cancelled. You can retry."); }
  };
  const login = async (app: typeof APPS[number], cfg?: McpServerCfg) => {
    const run = ++generation.current;
    let ownFlow: string | null = null;
    setBusy(true); setPending(app.name); setAuthMessage("");
    try {
      let name = cfg?.name ?? app.name.toLowerCase().replace(/\s+/g, "-");
      const servers = settings?.mcp ?? [];
      if (!cfg) {
        const base = name;
        for (let suffix = 2; servers.some((m) => m.name === name); suffix++) name = `${base}-${suffix}`;
        if (!await saveSettings({ mcp: [...servers, { name, command: "", args: [], env: {}, enabled: false, transport: "http", url: app.url, oauth: true, auto_approve: [] }] })) throw new Error("Unable to save connector settings");
      } else if (cfg.enabled) {
        if (!await saveSettings({ mcp: servers.map((m) => m === cfg ? { ...m, enabled: false } : m) })) throw new Error("Unable to disable connector before sign-in");
      }
      if (!alive.current || run !== generation.current) return;
      const started = await api.mcpOauthStart(name, clients[app.name]?.trim() || undefined, app.scopes, app.redirectPort);
      if (!alive.current || run !== generation.current) { await api.mcpOauthCancel(started.flow_id); return; }
      ownFlow = started.flow_id;
      flow.current = started.flow_id;
      await openUrl(started.authorization_url);
      await api.mcpOauthWait(started.flow_id);
      if (alive.current && run === generation.current) { await refresh(); setAuthMessage(`${app.name} authorized. MCP connection status appears above.`); }
    } catch (e) {
      if (ownFlow) await api.mcpOauthCancel(ownFlow).catch(() => {});
      if (alive.current && run === generation.current) setAuthMessage(`Sign-in failed: ${String(e)}`);
    } finally {
      if (run === generation.current) { flow.current = null; if (alive.current) { setBusy(false); setPending(""); } }
    }
  };
  const disconnect = async (cfg: McpServerCfg, servers: McpServerCfg[], app: string) => {
    if (!cfg.oauth) { await change(servers.filter((m) => m !== cfg), app); return; }
    setBusy(true);
    try { await api.mcpOauthDisconnect(cfg.name); await refresh(); }
    catch (e) { setAuthMessage(`Disconnect failed: ${String(e)}`); }
    finally { if (alive.current) setBusy(false); }
  };
  useEffect(() => {
    let active = true;
    const check = () => api.mcpStatus().then((next) => { if (active) { setStatus(next); setCheckError(""); } }).catch(() => { if (active) setCheckError("Unable to check MCP status"); });
    void check();
    const timer = setInterval(() => void check(), 4000);
    return () => { active = false; clearInterval(timer); };
  }, []);
  const custom = () => set({ settingsTab: "mcp" });
  const change = async (mcp: McpServerCfg[], name: string) => {
    setBusy(true);
    try {
      if (await saveSettings({ mcp })) {
        setDrafts((prev) => ({ ...prev, [name]: "" }));
        await api.mcpReconnect();
        setStatus(await api.mcpStatus());
      }
    } catch { flash("MCP reconnect failed; check MCP server settings for details."); }
    finally { setBusy(false); }
  };
  return <>
    <div className="settings-title">App connectors</div>
    <div className="desc">Connect only accounts and servers you trust. Tokens and OAuth credentials are stored locally, not encrypted credential storage. Connecting does not pre-approve tools; your existing permission mode still applies. Disconnecting removes token-based servers; OAuth disconnect disables the saved server and deletes its local tokens. It does not revoke provider access.</div>
    {checkError && <div role="status" className="desc">{checkError}</div>}
    {authMessage && <div role="status" className="desc">{authMessage}</div>}
    {pending && <div className="srowx" role="status">Waiting for {pending} sign-in in your browser…<Button onClick={() => void cancel()}>Cancel sign-in</Button></div>}
    {APPS.map((preset) => {
      const service = GOOGLE_SERVICES.find((s) => s.name === googleService)!;
      const app = preset.name === "Google Workspace" ? { ...preset, url: service.url, scopes: service.scopes } : preset;
      const servers = settings?.mcp ?? [];
      // Match the official endpoint, not a generic app name that could belong
      // to an unrelated custom server. Manage those only in MCP settings.
      const configured = servers.filter((m) => (app.url && m.transport === "http" && (m.url === app.url || (app.name === "Google Workspace" && GOOGLE_SERVICES.some((s) => s.url === m.url)))) || (app.local && (m.transport ?? "stdio") === "stdio" && m.command === "npx" && m.args.length === 2 && m.args[0] === "-y" && m.args[1] === "@notionhq/notion-mcp-server") || (app.workiq && (m.transport ?? "stdio") === "stdio" && m.command === "npx" && m.args.join(" ") === "-y @microsoft/workiq mcp"));
      const token = drafts[app.name] ?? "";
      const detailsId = `connector-${app.name.toLowerCase().replace(/\s/g, "-")}-details`;
      const summary = configured.length ? configured.map((cfg) => {
        const st = status.find((s) => s.name === cfg.name);
        return `${cfg.name} · ${!cfg.enabled ? "Disabled" : st?.status === "connected" ? `Connected · ${st.tools} tools` : st?.status ?? "Waiting for MCP status"}`;
      }).join("; ") : app.url || app.local || app.workiq ? "Not connected" : "Custom setup required";
      return <div className="sgroup" key={app.name}>
        <div className="srowx">
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>{app.name}</div><div className="desc" role="status">{summary}</div></div>
          <Button variant="ghost" aria-label={`${app.name} details`} aria-expanded={!!expanded[app.name]} aria-controls={detailsId} onClick={() => setExpanded((prev) => ({ ...prev, [app.name]: !prev[app.name] }))}>Details</Button>
        </div>
        {expanded[app.name] && <div id={detailsId}>
        <div className="srowx"><div className="desc" style={{ flex: 1 }}>{app.note}</div><Button variant="ghost" onClick={() => void openUrl(app.docs).catch(() => flash("Couldn't open connector documentation"))}>Official docs</Button></div>
        {configured.map((cfg) => {
          const st = status.find((s) => s.name === cfg.name);
          return <div className="srowx" key={cfg.name}>
            <div style={{ flex: 1 }}>{cfg.name}{st?.status === "error" ? " · Check MCP settings for error details" : ""}</div>
            {cfg.oauth && <Button disabled={busy || (app.client && !clients[app.name]?.trim())} onClick={() => {
              const savedService = GOOGLE_SERVICES.find((s) => s.url === cfg.url);
              void login(savedService ? { ...app, url: savedService.url, scopes: savedService.scopes } : app, cfg);
            }}>Reconnect {app.name}</Button>}
            <Button disabled={busy} onClick={() => void disconnect(cfg, servers, app.name)}>Disconnect {app.name}</Button>
          </div>;
        })}
        {app.name === "Google Workspace" && <div className="srowx"><label htmlFor="google-mcp-service">Workspace service</label><select id="google-mcp-service" className="input" value={googleService} disabled={busy} onChange={(e) => setGoogleService(e.currentTarget.value)}>{GOOGLE_SERVICES.map((s) => <option key={s.name}>{s.name}</option>)}</select></div>}
        {app.client && <div className="srowx"><Input aria-label={`${app.name} OAuth client ID`} placeholder="Your registered public OAuth client ID" value={clients[app.name] ?? ""} onChange={(e) => { const value = e.currentTarget.value; setClients((prev) => ({ ...prev, [app.name]: value })); }} /></div>}
        <div className="srowx">
          {app.workiq && !configured.length && <Button disabled={busy || !settings} onClick={() => {
            let name = "microsoft-365";
            for (let suffix = 2; servers.some((m) => m.name === name); suffix++) name = `microsoft-365-${suffix}`;
            void change([...servers, { name, command: "npx", args: ["-y", "@microsoft/workiq", "mcp"], env: {}, enabled: true, transport: "stdio", auto_approve: [] }], app.name);
          }}>Install and connect Microsoft 365</Button>}
          {app.url && !configured.some((cfg) => cfg.oauth && cfg.url === app.url) && <Button disabled={busy || !settings || (app.client && !clients[app.name]?.trim())} onClick={() => void login(app)}>Sign in to {app.name}</Button>}
          {(app.token || app.local) && !configured.length && <>
            <Input className="input" type="password" autoComplete="off" aria-label={app.token} placeholder={app.token} value={token} onChange={(e) => { const value = e.currentTarget.value; setDrafts((prev) => ({ ...prev, [app.name]: value })); }} />
            <Button disabled={busy || !settings || !token.trim()} onClick={() => {
              let name = app.name.toLowerCase();
              for (let suffix = 2; servers.some((m) => m.name === name); suffix++) name = `${app.name.toLowerCase()}-${suffix}`;
              const cfg: McpServerCfg = app.local
                ? { name, command: "npx", args: ["-y", "@notionhq/notion-mcp-server"], env: { NOTION_TOKEN: token.trim() }, enabled: true, transport: "stdio", auto_approve: [] }
                : { name, command: "", args: [], env: {}, enabled: true, transport: "http", url: app.url, headers: { Authorization: `Bearer ${token.trim()}` }, oauth: false, auto_approve: [] };
              void change([...servers, cfg], app.name);
            }}>Connect {app.name}</Button>
          </>}
          <Button variant="ghost" onClick={custom}>Custom MCP settings</Button>
        </div>
        </div>}
      </div>;
    })}
  </>;
}
