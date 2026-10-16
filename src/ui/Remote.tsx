import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Laptop, LogOut, Plug, PlugZap, QrCode, RefreshCw, Server, Smartphone, Trash2, BookOpen, ChevronLeft, ChevronRight } from "lucide-react";
import { flash, set } from "../store";
import { isRemoteClient, remoteApi, sshApi, type RemoteStatus, type SshHostView } from "../remote";
import { copyText } from "./clipboard";
import { Button, IconButton, Input, MorphText, Segmented, Switch, Modal } from "./primitives";

const when = (unix: number) => {
  const s = Date.now() / 1000 - unix;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return new Date(unix * 1000).toLocaleDateString();
};

/** Live remote status, refreshed whenever a device connects or disconnects. */
export function useRemoteStatus(): [RemoteStatus | null, (s: RemoteStatus) => void, () => void] {
  const [st, setSt] = useState<RemoteStatus | null>(null);
  const refresh = useCallback(() => { void remoteApi.status().then(setSt).catch(() => {}); }, []);
  useEffect(() => {
    refresh();
    let un: (() => void) | undefined;
    let dead = false;
    void listen("ol://remote", refresh).then((u) => { if (dead) u(); else un = u; });
    return () => { dead = true; un?.(); };
  }, [refresh]);
  return [st, setSt, refresh];
}

function PairPanel({ st, onStatus }: { st: RemoteStatus; onStatus: (s: RemoteStatus) => void }) {
  const [showLink, setShowLink] = useState(false);
  const [copied, setCopied] = useState(false);
  const [left, setLeft] = useState(st.code?.expires_in ?? 0);
  useEffect(() => {
    setLeft(st.code?.expires_in ?? 0);
    if (!st.code) return;
    const t = window.setInterval(() => setLeft((n) => Math.max(0, n - 1)), 1000);
    return () => window.clearInterval(t);
  }, [st.code]);
  const qr = st.qr_svg ? "data:image/svg+xml;charset=utf-8," + encodeURIComponent(st.qr_svg) : "";
  return (
    <div className="sgroup">
      <div className="srowx remote-pair" style={{ alignItems: "flex-start", gap: 20, flexWrap: "wrap" }}>
        {qr && <img src={qr} alt="QR code that pairs a device with OpenLeash" width={168} height={168} style={{ borderRadius: 8, background: "#fff", flexShrink: 0 }} />}
        <div style={{ flex: 1, minWidth: 220, display: "flex", flexDirection: "column", gap: 10 }}>
          <div style={{ fontWeight: 500 }}>Scan with your phone's camera</div>
          <div className="desc">Or open <span className="mono sel">{st.urls[0]}</span> on the device and enter a pairing code.</div>
          {st.code && left > 0
            ? <div><div className="mono sel" style={{ fontSize: 28, fontWeight: 600, letterSpacing: "0.3em" }}>{st.code.code}</div><div className="desc">Valid for {Math.floor(left / 60)}:{String(left % 60).padStart(2, "0")} · single use</div></div>
            : <div><Button onClick={() => void remoteApi.newCode().then(onStatus).catch((e) => flash(String(e)))}>Create pairing code</Button></div>}
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <Button variant="ghost" onClick={() => setShowLink((v) => !v)}>{showLink ? "Hide pairing link" : "Show pairing link"}</Button>
            {showLink && <Button variant="ghost" onClick={() => copyText(st.pair_url, { onResult: (ok) => { if (!ok) return; setCopied(true); window.setTimeout(() => setCopied(false), 1200); } })}><MorphText>{copied ? "Copied" : "Copy link"}</MorphText></Button>}
          </div>
          {showLink && <div className="mono sel" style={{ fontSize: 11, wordBreak: "break-all" }}>{st.pair_url}</div>}
        </div>
      </div>
    </div>
  );
}

export function RemoteTab() {
  const [st, setSt] = useRemoteStatus();
  const [port, setPort] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
  const [pairing, setPairing] = useState(false);
  const [tutorialStep, setTutorialStep] = useState<number | null>(null);
  const [copiedCommand, setCopiedCommand] = useState(false);
  useEffect(() => { if (st) setPort(String(st.port)); }, [st?.port]); // eslint-disable-line react-hooks/exhaustive-deps
  if (!st) return null;
  const remote = isRemoteClient();
  const copyCommand = () => {
    void (async () => {
      try {
        if (!copyText(`tailscale serve --bg http://127.0.0.1:${st.port}`)) throw new Error("Clipboard access is unavailable on this device.");
        setCopiedCommand(true);
        window.setTimeout(() => setCopiedCommand(false), 1400);
      } catch {
        flash("Clipboard access is unavailable. Copy the command manually.");
      }
    })();
  };
  const apply = (enabled: boolean, lan: boolean, p = Number(port) || st.port) => {
    setBusy(true);
    void remoteApi.configure(enabled, lan, p)
      .then((next) => { setSt(next); if (next.error) flash(next.error); })
      .catch((e) => flash(String(e)))
      .finally(() => setBusy(false));
  };
  return (
    <>
      <div className="settings-head"><div className="settings-head-main"><div className="settings-title">Remote</div><div className="settings-lead">Use OpenLeash from your phone or another computer's browser. Agents keep running here; the other device is a window onto this one.</div></div></div>
      {remote && <div className="settings-note">You are using OpenLeash remotely from <span className="mono">{window.__OPENLEASH_REMOTE__?.host}</span>. Changing these settings can disconnect this device.</div>}
      <div className="settings-note"><b>Recommended for sharing outside your local network: Tailscale.</b> It keeps access on your private tailnet, so you don't need to expose OpenLeash to the public internet. Leave Reachable from set to <b>This computer</b>, then use the tutorial to connect with Tailscale Serve. <Button variant="ghost" style={{ height: 24, marginLeft: 6 }} onClick={() => setTutorialStep(0)}><BookOpen size={13} /> Tailscale tutorial</Button></div>
      <div className="settings-note"><b>A paired device can do everything you can do here</b>: approve tool calls, run commands through agents, change settings and read your files. Only pair devices you own, and revoke any you lose.</div>
      {tutorialStep !== null && <TailscaleTutorial step={tutorialStep} port={st.port} onStep={setTutorialStep} onClose={() => setTutorialStep(null)} copied={copiedCommand} onCopy={copyCommand} />}
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Allow remote control</div><div className="desc">{st.running ? <>Listening on port {st.port}{st.lan ? " on your network" : " on this computer only"}.</> : st.error || "Off."}</div></div>
          <Switch label="Allow remote control" checked={st.enabled} onChange={busy ? undefined : (checked) => apply(checked, st.lan)} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Reachable from</div><div className="desc">{st.lan
          ? "Any device on your network that can reach this computer. Traffic is not encrypted on the network; on untrusted Wi-Fi use Tailscale or an SSH tunnel instead."
          : "This computer only. Reach it from elsewhere through an SSH tunnel or a private network such as Tailscale."}</div></div>
          <Segmented label="Reachable from" options={[{ value: "local", label: "This computer" }, { value: "lan", label: "Network" }]} value={st.lan ? "lan" : "local"} onChange={(v) => apply(st.enabled, v === "lan")} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Port</div></div>
          <Input aria-label="Port" className="input mono" style={{ width: 96 }} inputMode="numeric" value={port} onChange={(e) => setPort(e.currentTarget.value.replace(/\D/g, "").slice(0, 5))} onKeyDown={(e) => { if (e.key === "Enter") apply(st.enabled, st.lan); }} onBlur={() => { if (Number(port) !== st.port && port) apply(st.enabled, st.lan); }} /></div>
        {st.running && <div className="srowx" style={{ alignItems: "flex-start" }}><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Addresses</div>{st.urls.map((u) => <div key={u} className="mono sel desc">{u}</div>)}</div></div>}
      </div>
      {st.running && <>
        <div className="settings-section-label">Pair a device</div>
        {pairing ? <PairPanel st={st} onStatus={setSt} /> : <div className="sgroup"><div className="srowx"><div style={{ flex: 1 }} className="desc">Shows a QR code and pairing link. Both are secrets while they are on screen.</div><Button onClick={() => setPairing(true)}><QrCode size={13} /> Pair a device</Button></div></div>}
      </>}
      <div className="settings-section-label">Paired devices</div>
      <div className="sgroup">
        {st.sessions.length === 0 && <div className="srowx desc">No devices are paired.</div>}
        {[...st.sessions].sort((a, b) => b.last_seen - a.last_seen).map((s) => {
          const Icon = /iPhone|iPad|Android/.test(s.label) ? Smartphone : Laptop;
          return (
            <div key={s.id} className="srowx">
              <Icon size={15} strokeWidth={1.7} aria-hidden="true" />
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontWeight: 500 }}>{s.label}{s.connected > 0 && <span className="remote-live"> · connected</span>}</div>
                <div className="desc">Paired {when(s.created)} · last used {when(s.last_seen)}</div>
              </div>
              <IconButton label={`Revoke ${s.label}`} onClick={() => void remoteApi.revoke(s.id).then(setSt).catch((e) => flash(String(e)))}><Trash2 size={13} /></IconButton>
            </div>
          );
        })}
        <div className="srowx">
          <div style={{ flex: 1 }} className="desc">Signs out every device and invalidates the current QR code and link.</div>
          {confirmReset
            ? <><Button variant="ghost" onClick={() => setConfirmReset(false)}>Cancel</Button><Button onClick={() => { setConfirmReset(false); void remoteApi.reset().then(setSt).catch((e) => flash(String(e))); }}>Sign out all</Button></>
            : <Button variant="ghost" onClick={() => setConfirmReset(true)}><RefreshCw size={13} /> Sign out all devices</Button>}
        </div>
        {remote && <div className="srowx"><div style={{ flex: 1 }} className="desc">Sign this browser out of OpenLeash.</div><Button variant="ghost" onClick={() => void window.__OPENLEASH_REMOTE__?.logout?.()}><LogOut size={13} /> Sign out this device</Button></div>}
      </div>
      <SshHosts />
    </>
  );
}

const TAILSCALE_STEPS = [
  { title: "Connect both devices to Tailscale", body: <>Install Tailscale and sign in to the same tailnet on this computer and the device you'll use remotely. Tailscale creates a private network between your devices.</> },
  { title: "Keep OpenLeash on this computer only", body: <>Turn on <b>Allow remote control</b> above, and leave <b>Reachable from → This computer</b> selected. This keeps the server bound to localhost; Tailscale Serve will provide private tailnet access without opening it to your LAN.</> },
  { title: "Share it with Tailscale Serve", body: <>On this computer, run this command in a terminal. It securely shares the local OpenLeash page with your tailnet and runs in the background:</> },
  { title: "Open it on your other device", body: <>On the host computer, run <span className="mono">tailscale serve status</span> and open the listed HTTPS address on your other Tailscale-connected device. Then pair it with OpenLeash using the QR code or pairing code in the <b>Pair a device</b> section. Pairing is still required even on your private network.</> },
];

function TailscaleTutorial({ step, port, onStep, onClose, copied, onCopy }: { step: number; port: number; onStep: (step: number) => void; onClose: () => void; copied: boolean; onCopy: () => void }) {
  const current = Math.max(0, Math.min(step, TAILSCALE_STEPS.length - 1));
  return <Modal onClose={onClose} ariaLabelledBy="tailscale-tutorial-title" className="tutorial-modal">
    <div className="mh"><div><div className="settings-title" id="tailscale-tutorial-title">Tailscale tutorial</div><div className="desc">Step {current + 1} of {TAILSCALE_STEPS.length}</div></div><Button variant="ghost" onClick={onClose}>Close</Button></div>
    <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 14, minHeight: 130 }}>
      <h3 style={{ margin: 0, fontSize: 16 }}>{TAILSCALE_STEPS[current]!.title}</h3>
      <div className="desc" style={{ lineHeight: 1.6 }}>{TAILSCALE_STEPS[current]!.body}</div>
      {current === 2 && <div className="srowx" style={{ gap: 8 }}><code className="mono sel" style={{ flex: 1, padding: 10, borderRadius: 6, background: "var(--panel)" }}>tailscale serve --bg http://127.0.0.1:{port}</code><Button onClick={onCopy}><MorphText>{copied ? "Copied" : "Copy command"}</MorphText></Button></div>}
      {current === 0 && <a href="https://tailscale.com/download" target="_blank" rel="noreferrer">Download Tailscale</a>}
      {current === 3 && <a href="https://tailscale.com/kb/1242/tailscale-serve" target="_blank" rel="noreferrer">Tailscale Serve documentation</a>}
    </div>
    <div className="mf" style={{ justifyContent: "space-between" }}>
      <Button variant="ghost" disabled={current === 0} onClick={() => onStep(current - 1)}><ChevronLeft size={14} /> Back</Button>
      {current === TAILSCALE_STEPS.length - 1
        ? <Button onClick={onClose}>Done</Button>
        : <Button onClick={() => onStep(current + 1)}>Next <ChevronRight size={14} /></Button>}
    </div>
  </Modal>;
}

/**
 * Titlebar chip. On the desktop it appears while any device is connected,
 * so remote control is never silently in use; in a remote browser it says
 * which computer this window is driving.
 */
export function RemoteIndicator({ onOpen }: { onOpen: () => void }) {
  const remote = isRemoteClient();
  const [st] = useRemoteStatus();
  const n = st?.clients.length ?? 0;
  if (!remote && n === 0) return null;
  const label = remote
    ? `Remote · ${window.__OPENLEASH_REMOTE__?.host ?? ""}`
    : `${n} remote device${n === 1 ? "" : "s"} connected`;
  return (
    <button type="button" className="remote-chip" title={remote ? "Using OpenLeash remotely" : st!.clients.map((c) => `${c.label} (${c.ip})`).join("\n")} aria-label={label} onClick={onOpen}>
      <Smartphone size={12} strokeWidth={1.8} aria-hidden="true" />
      <span>{remote ? "Remote" : n}</span>
    </button>
  );
}

/** Keep the app-wide copy current too: SSH projects are named by host name. */
const publishHosts = (h: SshHostView[]) => { set({ sshHosts: h }); return h; };

/**
 * Other machines running OpenLeash, opened over SSH. The agent keeps running
 * on that machine; this tunnels to its remote-control page and opens it in
 * the browser, already paired. Authentication is the system ssh client's.
 */
function SshHosts() {
  const [hosts, setHosts] = useState<SshHostView[]>([]);
  const [name, setName] = useState("");
  const [target, setTarget] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  useEffect(() => {
    void sshApi.list().then(publishHosts).then(setHosts).catch(() => {});
    const t = window.setInterval(() => { void sshApi.list().then(publishHosts).then(setHosts).catch(() => {}); }, 5000);
    return () => window.clearInterval(t);
  }, []);
  const run = (key: string, f: () => Promise<SshHostView[]>) => {
    setBusy(key);
    void f().then(publishHosts).then(setHosts).catch((e) => { flash(String(e)); void sshApi.list().then(publishHosts).then(setHosts).catch(() => {}); }).finally(() => setBusy(null));
  };
  return <>
    <div className="settings-section-label">Other machines over SSH</div>
    <div className="settings-note">Run agents on a dev box, a server or WSL-less Linux machine, and drive them from here. That machine needs OpenLeash with remote control on ("This computer" is enough). Uses your <span className="mono">ssh</span> client, config and keys; password prompts aren't supported, so set up key or agent login first. For a project inside WSL on this PC, just open its <span className="mono">\\wsl.localhost\…</span> folder: commands and git run in the distro automatically.</div>
    <div className="sgroup">
      {hosts.length === 0 && <div className="srowx desc">No machines added.</div>}
      {hosts.map((h) => (
        <div key={h.id} className="srowx">
          <Server size={15} strokeWidth={1.7} aria-hidden="true" />
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontWeight: 500 }}>{h.name}{h.connected && <span className="remote-live"> · connected</span>}</div>
            <div className="desc mono" style={{ fontSize: 11 }}>ssh {h.target}{h.local_url ? ` → ${h.local_url}` : ""}</div>
            {h.error && !h.connected && <div className="desc" style={{ color: "#ff8a8a" }}>{h.error}</div>}
          </div>
          {h.connected
            ? <><Button disabled={busy === h.id} onClick={() => run(h.id, () => sshApi.connect(h.id))}><PlugZap size={13} /> Open</Button><Button variant="ghost" onClick={() => run(h.id, () => sshApi.disconnect(h.id))}>Disconnect</Button></>
            : <Button disabled={busy === h.id} onClick={() => run(h.id, () => sshApi.connect(h.id))}><Plug size={13} /> {busy === h.id ? "Connecting…" : "Connect"}</Button>}
          <IconButton label={`Remove ${h.name}`} onClick={() => run(h.id, () => sshApi.remove(h.id))}><Trash2 size={13} /></IconButton>
        </div>
      ))}
      <div className="srowx" style={{ gap: 8, flexWrap: "wrap" }}>
        <Input aria-label="Name" className="input" style={{ width: 140 }} value={name} placeholder="Dev box" onChange={(e) => setName(e.currentTarget.value)} />
        <Input aria-label="SSH destination" className="input mono" style={{ flex: 1, minWidth: 160 }} value={target} placeholder="me@devbox or an ssh alias" onChange={(e) => setTarget(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter" && target.trim()) { run("add", () => sshApi.save(null, name, target)); setName(""); setTarget(""); } }} />
        <Button disabled={!target.trim() || busy === "add"} onClick={() => { run("add", () => sshApi.save(null, name, target)); setName(""); setTarget(""); }}>Add</Button>
      </div>
    </div>
  </>;
}
