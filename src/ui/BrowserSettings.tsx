import { useId, useState, type ReactNode } from "react";
import type { AllowRule, BrowserPluginCfg, Plugins, PluginsStatus } from "../api";
import { saveSettings, useStore } from "../store";
import { Button, Dropdown, IconButton, Input, Segmented, Switch } from "./primitives";
import { X } from "lucide-react";

/** Per-site browser exceptions are ordinary permission rules in `settings.allow`,
 *  so the Permissions page and this card edit one list. */
export type SiteMode = "allow" | "ask" | "block";
const SITE_PREFIX: Record<SiteMode, string> = { allow: "browser:site ", ask: "browser:ask ", block: "!browser:site " };
const LAN_PREFIX = "browser:lan ";

/** The rule pattern for a site exception. */
export const siteRule = (mode: SiteMode, host: string) => SITE_PREFIX[mode] + host;
export const lanRule = (host: string) => LAN_PREFIX + host;

/** A site rule's mode and host, or `null` for any other rule. */
export function parseSiteRule(pattern: string): { mode: SiteMode; host: string } | null {
  for (const mode of ["block", "allow", "ask"] as const) {
    if (pattern.startsWith(SITE_PREFIX[mode])) return { mode, host: pattern.slice(SITE_PREFIX[mode].length).trim() };
  }
  return null;
}

const LABEL = /^(?!-)[a-z0-9-]{1,63}(?<!-)$/;

/** A normalised host pattern (`example.com`, `*.example.com`), or `null`. A
 *  wildcard needs at least two labels after it, so `*.com` can't allow a TLD. */
export function normalizeHostPattern(raw: string, wildcard = true): string | null {
  let h = raw.trim().toLowerCase();
  // Pasted URLs are common; keep just the host.
  const m = h.match(/^[a-z][a-z0-9+.-]*:\/\/([^/?#]+)/);
  if (m) h = m[1]!;
  h = h.replace(/:\d+$/, "").replace(/\.$/, "");
  if (!h || h.length > 253) return null;
  const star = h.startsWith("*.");
  if (star && !wildcard) return null;
  const labels = (star ? h.slice(2) : h).split(".");
  if (!labels.every((l) => LABEL.test(l))) return null;
  if (star && labels.length < 2) return null;
  return h;
}

const MODES: { value: SiteMode; label: string; hint: string }[] = [
  { value: "allow", label: "Allow", hint: "Interactions on this site never ask — including sensitive ones like submit, pay or delete" },
  { value: "ask", label: "Ask", hint: "Every interaction on this site asks first" },
  { value: "block", label: "Block", hint: "The agent can't interact with this site" },
];

function Row({ title, desc, children }: { title: string; desc?: string; children?: ReactNode }) {
  return <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}>{title}{desc && <div className="desc">{desc}</div>}</div>{children}</div>;
}

/** Settings → Plugins → Browser: the switch, its readiness, and every browser
 *  setting, including the per-site permission rules. */
export function BrowserSettings({ p, status, save }: { p: Plugins; status: PluginsStatus["browser"] | undefined; save: (next: Plugins) => Promise<unknown> }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const b = p.browser;
  const set = (patch: Partial<BrowserPluginCfg>) => void save({ ...p, browser: { ...b, ...patch } });
  // Match the backend's unset-dimension defaults, and retain custom saved sizes.
  const width = b.width > 0 ? b.width : 1280;
  const height = b.height > 0 ? b.height : 800;
  const sizes = [[1024, 768], [1280, 800], [1440, 900], [1920, 1080]];
  if (!sizes.some(([w, h]) => w === width && h === height)) sizes.push([width, height]);
  const mode = b.headed ? "window" : "headless";
  return <>
    <div className="srowx"><div style={{ flex: 1 }}>Browser<div className="desc">{!status ? "Checking…" : status.ok ? `Browser available · ${status.browser} · ${mode}` : `Can't start a browser: ${status.error ?? "no browser found"}. Install Chrome or Edge.`}</div></div><Switch label="Browser plugin" checked={b.enabled} onChange={(enabled) => set({ enabled })} /></div>
    <div className="srowx" style={{ minHeight: 34 }}><Button aria-label="Browser Details" aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}>Details {open ? "▾" : "▸"}</Button></div>
    <div id={id} hidden={!open}>{open && <BrowserDetails b={b} set={set} width={width} height={height} sizes={sizes} />}</div>
  </>;
}

function BrowserDetails({ b, set, width, height, sizes }: { b: BrowserPluginCfg; set: (patch: Partial<BrowserPluginCfg>) => void; width: number; height: number; sizes: number[][] }) {
  const [downloads, setDownloads] = useState(b.download_dir);
  const saveDownloads = () => { if (downloads.trim() !== b.download_dir) set({ download_dir: downloads.trim() }); };
  return <>
    <div className="srowx desc">Agents drive a real browser: open pages, follow links, fill in forms, read rendered pages and take screenshots. It runs in its own separate profile — never your everyday Chrome or Edge profile — and never touches your screen, mouse or keyboard. Reading needs no approval. You can watch and take over in the chat's Browser view.</div>
    <Row title="Viewport" desc="Page size. Wider layouts use more screenshot tokens."><Dropdown search={false} style={{ width: 150 }} value={`${width}x${height}`} onChange={(v) => { const [w, h] = v.split("x").map(Number); set({ width: w!, height: h! }); }} options={sizes.map(([w, h]) => ({ value: `${w}x${h}`, label: `${w} × ${h}` }))} /></Row>
    <Row title="Page interactions" desc="What happens when an agent clicks, types or submits. Sensitive actions such as submitting a form, paying or deleting still ask unless the site is set to Allow below.">
      <Segmented label="Page interactions" value={b.interactions} onChange={(interactions) => set({ interactions })} options={[{ value: "ask", label: "Ask" }, { value: "allow", label: "Allow" }, { value: "block", label: "Block" }] as const} />
    </Row>
    <Row title="Keep sign-ins between chats" desc="One persistent browser profile shared by every chat, so a site you sign into stays signed in. The agent can then act as you on those sites. It is still a separate profile, never your real Chrome profile, and interactions still ask as set above. Off: a fresh, throwaway profile per chat.">
      <Switch label="Keep sign-ins between chats" checked={b.persistent_profile} onChange={(persistent_profile) => set({ persistent_profile })} />
    </Row>
    <Row title="Show browser window" desc="Open a visible browser window instead of running headless. Useful for signing in or watching closely; it can take focus.">
      <Switch label="Show browser window" checked={b.headed} onChange={(headed) => set({ headed })} />
    </Row>
    <Row title="Downloads folder" desc="Where files the page downloads are saved. Blank: the project's .openleash/browser-downloads folder.">
      <Input aria-label="Downloads folder" className="input mono" style={{ width: 230, fontSize: 12 }} value={downloads} placeholder=".openleash/browser-downloads" onChange={(e) => setDownloads(e.currentTarget.value)} onBlur={saveDownloads} onKeyDown={(e) => { if (e.key === "Enter") saveDownloads(); }} />
    </Row>
    <Row title="Developer mode" desc="Offer agents the raw Chrome DevTools Protocol. Powerful and unfiltered, so every raw call asks, whatever else is allowed.">
      <Switch label="Developer mode (raw CDP — every call asks)" checked={b.developer_mode} onChange={(developer_mode) => set({ developer_mode })} />
    </Row>
    <SiteRules />
    <LanHosts />
  </>;
}

function useAllow(): AllowRule[] {
  return useStore((s) => s.settings?.allow) ?? [];
}

function SiteRules() {
  const allow = useAllow();
  const [host, setHost] = useState("");
  const [mode, setMode] = useState<SiteMode>("allow");
  const [err, setErr] = useState("");
  const rows = allow.map((r, i) => ({ r, i, site: parseSiteRule(r.pattern) })).filter((x) => x.site);
  const add = () => {
    const h = normalizeHostPattern(host);
    if (!h) { setErr("Use a host like example.com or *.example.com."); return; }
    // One exception per host: adding again replaces the old mode.
    const rest = allow.filter((r) => { const s = parseSiteRule(r.pattern); return !(s && s.host === h && !r.project); });
    void saveSettings({ allow: [...rest, { pattern: siteRule(mode, h), project: "" }] });
    setHost(""); setErr("");
  };
  return <div className="srowx" style={{ flexDirection: "column", alignItems: "stretch", gap: 6 }}>
    <div>Site exceptions<div className="desc">Override page interactions for one site. Allow also covers sensitive actions; *.example.com covers its subdomains and example.com itself. Stored as permission rules, so they also show under Permissions.</div></div>
    {rows.length === 0 ? <div className="desc">No site exceptions.</div> : rows.map(({ r, i, site }) => <div key={`${r.pattern}:${r.project}:${i}`} className="srowx" style={{ padding: 0, gap: 8 }}>
      <span className="mono" style={{ flex: 1, minWidth: 0, fontSize: 12, overflow: "hidden", textOverflow: "ellipsis" }}>{site!.host}{r.project && <span className="desc"> · {r.project}</span>}</span>
      <Segmented quiet label={`Rule for ${site!.host}`} style={{ width: 210 }} value={site!.mode} options={MODES} onChange={(m) => void saveSettings({ allow: allow.map((x, j) => (j === i ? { ...x, pattern: siteRule(m, site!.host) } : x)) })} />
      <IconButton label={`Remove ${site!.host}`} onClick={() => void saveSettings({ allow: allow.filter((_, j) => j !== i) })}><X size={12} /></IconButton>
    </div>)}
    <div className="srowx" style={{ padding: 0, gap: 8 }}>
      <Input aria-label="Site host" className="input mono" style={{ flex: 1, minWidth: 0, fontSize: 12 }} value={host} placeholder="example.com or *.example.com" onChange={(e) => { setHost(e.currentTarget.value); setErr(""); }} onKeyDown={(e) => { if (e.key === "Enter" && host.trim()) add(); }} />
      <Segmented quiet label="New site rule" style={{ width: 210 }} value={mode} options={MODES} onChange={setMode} />
      <Button disabled={!host.trim()} onClick={add}>Add site</Button>
    </div>
    {err && <div role="alert" className="desc" style={{ color: "var(--red2)" }}>{err}</div>}
  </div>;
}

function LanHosts() {
  const allow = useAllow();
  const [host, setHost] = useState("");
  const [err, setErr] = useState("");
  const rows = allow.map((r, i) => ({ r, i })).filter(({ r }) => r.pattern.startsWith(LAN_PREFIX));
  const add = () => {
    const h = normalizeHostPattern(host, false);
    if (!h) { setErr("Use a host or address like nas.local or 192.168.1.20."); return; }
    if (!rows.some(({ r }) => r.pattern === lanRule(h) && !r.project)) void saveSettings({ allow: [...allow, { pattern: lanRule(h), project: "" }] });
    setHost(""); setErr("");
  };
  return <div className="srowx" style={{ flexDirection: "column", alignItems: "stretch", gap: 6 }}>
    <div>Allowed local-network hosts<div className="desc">Pages can reach localhost freely, but other machines on your network (routers, NAS, intranet) only when listed here, or after you open one yourself in the Browser view. Cloud metadata addresses are never reachable.</div></div>
    {rows.length === 0 ? <div className="desc">No local-network hosts allowed.</div> : rows.map(({ r, i }) => <div key={`${r.pattern}:${r.project}:${i}`} className="srowx" style={{ padding: 0, gap: 8 }}>
      <span className="mono" style={{ flex: 1, minWidth: 0, fontSize: 12 }}>{r.pattern.slice(LAN_PREFIX.length)}{r.project && <span className="desc"> · {r.project}</span>}</span>
      <IconButton label={`Remove ${r.pattern.slice(LAN_PREFIX.length)}`} onClick={() => void saveSettings({ allow: allow.filter((_, j) => j !== i) })}><X size={12} /></IconButton>
    </div>)}
    <div className="srowx" style={{ padding: 0, gap: 8 }}>
      <Input aria-label="Local-network host" className="input mono" style={{ flex: 1, minWidth: 0, fontSize: 12 }} value={host} placeholder="nas.local or 192.168.1.20" onChange={(e) => { setHost(e.currentTarget.value); setErr(""); }} onKeyDown={(e) => { if (e.key === "Enter" && host.trim()) add(); }} />
      <Button disabled={!host.trim()} onClick={add}>Add host</Button>
    </div>
    {err && <div role="alert" className="desc" style={{ color: "var(--red2)" }}>{err}</div>}
  </div>;
}
