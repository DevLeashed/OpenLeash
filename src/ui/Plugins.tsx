import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, Plugins, PluginsStatus } from "../api";
import { Dropdown, Switch, Button } from "./primitives";
import { flash, saveSettings, useStore } from "../store";
import { AppConnectors } from "./Connectors";
import { McpServers } from "./McpServers";

const OFF: Plugins = { github: { enabled: false }, computer: { enabled: false, settle_ms: 600 }, browser: { enabled: false, width: 1280, height: 800 } };

function Details({ name, children }: { name: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  return <><div className="srowx" style={{ minHeight: 34 }}><Button aria-label={`${name} Details`} aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}>Details {open ? "▾" : "▸"}</Button></div><div id={id} hidden={!open}>{open && children}</div></>;
}

function usePlugins() {
  const p = useStore((s) => s.settings?.plugins) ?? OFF;
  const [st, setSt] = useState<PluginsStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  const pending = useRef(false);
  const check = () => { setBusy(true); api.pluginsStatus().then(setSt).catch((e) => flash(String(e))).finally(() => setBusy(false)); };
  useEffect(check, []);
  const save = async (next: Plugins) => {
    if (pending.current) return false;
    pending.current = true; setSaving(true);
    try { return await saveSettings({ plugins: next }); }
    catch (e) { flash(String(e)); return false; }
    finally { pending.current = false; setSaving(false); }
  };
  return { p, st, busy, saving, check, save };
}

export function PluginsTab() {
  const { p, st, busy, saving, check, save } = usePlugins();
  const pc = st?.computer;
  const primary = pc?.monitors.find((m) => m.primary) ?? pc?.monitors[0];
  const br = st?.browser;
  // Match the backend's unset-dimension defaults, and retain custom saved sizes.
  const width = p.browser.width > 0 ? p.browser.width : 1280;
  const height = p.browser.height > 0 ? p.browser.height : 800;
  const sizes = [[1024, 768], [1280, 800], [1440, 900], [1920, 1080]];
  if (!sizes.some(([w, h]) => w === width && h === height)) sizes.push([width, height]);
  const settle = p.computer.settle_ms || 600;
  return <>
    <div className="settings-head"><div className="settings-head-main settings-title">Plugins</div><span className="exptag" title="Each plugin gives the agent real capabilities on this machine">Experimental</span><Button disabled={busy} onClick={check}>{busy ? "Checking…" : "Check again"}</Button></div>
    <div className="desc">Built-in tool sets for agents. Turning one on applies to new chats (and existing ones after <span className="mono">/compact</span>), so running chats keep their prompt cache.</div>
    <div className="desc">Still finding its shape, and each one hands the agent real capabilities on this machine — every plugin below is off until you switch it on yourself. Availability below checks readiness, not whether a plugin is enabled.</div>
    <fieldset disabled={saving} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }} aria-busy={saving}>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div>Computer use</div><div className="desc">{!pc ? "Checking…" : pc.ok && primary ? `Screen available · ${primary.name || "primary"} · ${primary.w}×${primary.h}${pc.monitors.length > 1 ? ` · ${pc.monitors.length - 1} other monitors ignored` : ""}` : `Can't capture the screen: ${pc.error ?? "no monitors"}`}</div></div><Switch label="Computer use" checked={p.computer.enabled} onChange={(enabled) => void save({ ...p, computer: { ...p.computer, enabled } })} /></div>
        <Details name="Computer use"><div className="srowx desc">Agents see your primary screen and drive the mouse and keyboard. Every action returns a fresh screenshot. Clicks and typing ask first unless you always-allow them or use Full access.</div><div className="srowx"><div style={{ flex: 1 }}>Settle time<div className="desc">Wait after an action before the screenshot. Raise it for slow apps.</div></div><Dropdown search={false} style={{ width: 120 }} value={String(settle)} onChange={(v) => void save({ ...p, computer: { ...p.computer, settle_ms: Number(v) } })} options={Array.from(new Set([300, 600, 1000, 1500, 2500, settle])).map((ms) => ({ value: String(ms), label: `${ms} ms` }))} /></div></Details>
      </div>
      <div className="sgroup" style={{ marginTop: 16 }}>
        <div className="srowx"><div style={{ flex: 1 }}>Browser<div className="desc">{!br ? "Checking…" : br.ok ? `Browser available · ${br.browser} · headless` : `Can't start a browser: ${br.error ?? "no browser found"}. Install Chrome or Edge.`}</div></div><Switch label="Browser plugin" checked={p.browser.enabled} onChange={(enabled) => void save({ ...p, browser: { ...p.browser, enabled } })} /></div>
        <Details name="Browser"><div className="srowx desc">Agents drive a real browser: open pages, follow links, fill in forms, read rendered pages and take screenshots. One browser per task, with a throwaway profile. It never touches your screen, mouse or keyboard. Reading needs no approval; clicking and typing do, since they can submit something.</div><div className="srowx"><div style={{ flex: 1 }}>Viewport<div className="desc">Page size. Wider layouts use more screenshot tokens.</div></div><Dropdown search={false} style={{ width: 150 }} value={`${width}x${height}`} onChange={(v) => { const [w, h] = v.split("x").map(Number); void save({ ...p, browser: { ...p.browser, width: w!, height: h! } }); }} options={sizes.map(([w, h]) => ({ value: `${w}x${h}`, label: `${w} × ${h}` }))} /></div></Details>
      </div>
    </fieldset>{saving && <div role="status">Saving…</div>}
    <McpServers custom />
  </>;
}

export function ConnectorsTab() {
  const { p, st, busy, saving, check, save } = usePlugins();
  const [token, setToken] = useState("");
  const [cid, setCid] = useState(p.github.client_id ?? "");
  const [adv, setAdv] = useState(false);
  const [login, setLogin] = useState<{ code: string; url: string } | null>(null);
  const [authBusy, setAuthBusy] = useState(false);
  const authPending = useRef(false);
  const signIn = async () => {
    if (authPending.current) return;
    authPending.current = true; setAuthBusy(true);
    try {
      const d = await api.githubLoginStart();
      setLogin({ code: d.user_code, url: d.verification_uri });
      navigator.clipboard?.writeText(d.user_code).catch(() => {});
      void openUrl(d.verification_uri).catch((e) => flash(String(e)));
      const who = await api.githubLoginWait(d.device_code, d.interval, d.expires_in);
      flash(`Signed in to GitHub as ${who}`); check();
    } catch (e) { if (String(e) !== "cancelled") flash(String(e)); }
    finally { setLogin(null); authPending.current = false; setAuthBusy(false); }
  };
  const saveToken = async (t: string) => {
    if (authPending.current) return;
    authPending.current = true; setAuthBusy(true);
    try { await api.githubToken(t); setToken(""); flash(t ? "Token saved" : "Token removed"); check(); }
    catch (e) { flash(String(e)); }
    finally { authPending.current = false; setAuthBusy(false); }
  };
  const gh = st?.github;
  return <>
    <div className="settings-head"><div className="settings-head-main settings-title">Connectors</div><Button disabled={busy} onClick={check}>{busy ? "Checking…" : "Check again"}</Button></div>
    <div className="desc">Connect accounts and apps. Connecting does not pre-approve tools. GitHub tools follow the switch below; GitHub device sign-in also enables it. Other app connections enable their MCP server. Changes to GitHub tools apply to new chats or after /compact.</div>
    <div className="sgroup">
      <fieldset disabled={saving} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }} aria-busy={saving}>
        <div className="srowx"><div style={{ flex: 1 }}>GitHub<div className="desc">{!gh ? "Checking…" : gh.ok ? `Account available · ${gh.login} · token from ${gh.source}` : gh.source ? `Token from ${gh.source} doesn't work: ${gh.error}` : "No token · not connected"}</div></div><Switch label="GitHub plugin" checked={p.github.enabled} onChange={(enabled) => void save({ ...p, github: { ...p.github, enabled } })} /></div>
        <Details name="GitHub">
          <div className="srowx desc">Issues, pull requests, reviews, merges, Actions runs and logs, code search and files — through the API as you. Changes ask first unless you're in Full access.</div>
          {login ? <div className="srowx" style={{ flexWrap: "wrap" }}><span>Enter this code on GitHub, then Authorize: <b className="mono sel">{login.code}</b></span><Button onClick={() => { void navigator.clipboard?.writeText(login.code).catch((e) => flash(String(e))); }}>Copy</Button><Button onClick={() => void openUrl(login.url).catch((e) => flash(String(e)))}>Open GitHub</Button><Button onClick={() => void api.githubLoginCancel().catch((e) => flash(String(e)))}>Cancel</Button></div> : <div className="srowx"><div className="desc" style={{ flex: 1 }}>Sign in through your browser, use GH_TOKEN, or run gh auth login.</div><Button disabled={authBusy} onClick={() => st && !st.can_login ? setAdv(true) : void signIn()}>{authBusy ? "Waiting…" : "Sign in with GitHub"}</Button></div>}
          <div className="srowx"><input aria-label="GitHub token" className="input mono" type="password" style={{ flex: 1, minWidth: 0 }} value={token} placeholder={st?.has_token ? "Saved · paste to replace" : "ghp_… or github_pat_… (repo, workflow scopes)"} onChange={(e) => setToken(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter" && token.trim()) void saveToken(token.trim()); }} /><Button disabled={authBusy || !token.trim()} onClick={() => void saveToken(token.trim())}>Save token</Button>{st?.has_token && <Button disabled={authBusy} onClick={() => void saveToken("")}>Sign out</Button>}</div>
          <div className="srowx"><Button aria-expanded={adv} onClick={() => setAdv(!adv)}>Advanced · OAuth app</Button></div>
          {adv && <><div className="srowx desc">Sign-in uses a GitHub OAuth App with Device Flow enabled. Create one in GitHub Settings → Developer settings → OAuth Apps, enable Device Flow, and paste its Client ID here. Any homepage/callback URL works; no secret needed.</div><div className="srowx"><input aria-label="GitHub OAuth Client ID" className="input mono" style={{ flex: 1 }} value={cid} onChange={(e) => setCid(e.currentTarget.value)} placeholder={st?.can_login ? "using the built-in app" : "Ov23li…"} /><Button onClick={() => void save({ ...p, github: { ...p.github, client_id: cid.trim() } }).then((saved) => { if (saved) { flash("Saved"); check(); } })}>Save client ID</Button></div></>}
        </Details>
      </fieldset>{saving && <div role="status">Saving…</div>}
    </div>
    <AppConnectors />
  </>;
}
