import { open } from "@tauri-apps/plugin-dialog";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { FolderGit2, FolderOpen, Server, SquareTerminal } from "lucide-react";
import { api, baseName, type ClonePlan, type WslDistro } from "../api";
import { sshApi, type SshHostTest } from "../remote";
import { sshUri } from "../sshPath";
import { flash, get, go, openProject, set, useStore } from "../store";
import { pickFolder } from "./Home";
import { I } from "./icons";
import { Button, IconButton, Input, Loader, Modal, TextButton } from "./primitives";
import { loadSshHosts } from "./ProjectName";
import { PickerDialog } from "./RemotePicker";

/**
 * One way of opening a project, as the folder menus list them. Both menus (the
 * home screen's folder chip and the titlebar picker) render this same list, so
 * a new source is added here once and shows up in both.
 */
export interface OpenSource {
  id: "folder" | "wsl" | "clone" | "ssh";
  label: string;
  hint: string;
  icon: ReactNode;
  /** Callers close their own menu first; this only starts the flow. */
  run: () => void;
}

/** Ask the backend which distros are installed. Cheap: it doesn't boot one,
 *  and the backend remembers the answer for a minute. */
export function loadWsl() {
  return api.wslList()
    .then((wsl) => { set({ wsl }); return wsl; })
    .catch(() => get().wsl);
}

/**
 * The open-project sources, refreshed each time `active` turns true (a menu
 * opening), so a distro installed since launch appears without a restart.
 */
export function useOpenSources(active: boolean): OpenSource[] {
  const wsl = useStore((s) => s.wsl);
  useEffect(() => { if (active) void loadWsl(); }, [active]);
  const ico = (C: typeof FolderOpen) => <C size={13} strokeWidth={1.7} aria-hidden="true" />;
  return [
    { id: "folder", label: "Open folder…", hint: "Add another project · Ctrl O", icon: ico(FolderOpen), run: () => { void pickFolder().catch((e) => flash(String(e))); } },
    ...(wsl.length ? [{ id: "wsl" as const, label: "Open from WSL…", hint: wsl.length === 1 ? `A folder inside ${wsl[0]!.name}` : `A folder inside one of your ${wsl.length} distros`, icon: ico(SquareTerminal), run: openFromWsl }] : []),
    { id: "clone", label: "Clone repository…", hint: "From a GitHub or git URL into ~/openleash", icon: ico(FolderGit2), run: openClone },
    { id: "ssh", label: "Open over SSH…", hint: "A folder on another machine; the agent's tools run there", icon: ico(Server), run: openOverSsh },
  ];
}

export const openClone = () => set({ openFrom: "clone", menu: null, pal: false });
export const openOverSsh = () => set({ openFrom: "ssh", menu: null, pal: false });

/** One distro goes straight to its folder dialog; several ask which first. */
export function openFromWsl() {
  const wsl = get().wsl;
  set({ menu: null, pal: false });
  if (wsl.length === 1) void pickWslFolder(wsl[0]!);
  else if (wsl.length) set({ openFrom: "wsl" });
}

/**
 * The folder dialog, opened inside a distro. A remote browser gets the
 * in-app picker instead (the shim routes `open` there), and it honours
 * `defaultPath` the same way.
 */
export async function pickWslFolder(d: WslDistro) {
  // Finding the start folder reads the distro's /home, which boots it if it
  // isn't running — a few seconds the first time, so say what's happening.
  const slow = window.setTimeout(() => flash(`Starting ${d.name}…`), 600);
  const start = await api.wslStartDir(d.name).catch(() => d.root).finally(() => window.clearTimeout(slow));
  try {
    const dir = await open({ directory: true, multiple: false, title: `Open a folder in ${d.name}`, defaultPath: start });
    if (typeof dir === "string") await openProject(dir);
  } catch (e) {
    flash(String(e));
  }
}

/** Both dialogs, mounted once beside the other overlays. */
export function OpenProjectDialogs() {
  const which = useStore((s) => s.openFrom);
  if (which === "clone") return <CloneDialog />;
  if (which === "wsl") return <WslDialog />;
  if (which === "ssh") return <SshDialog />;
  return null;
}

const close = () => set({ openFrom: null });

function WslDialog() {
  const wsl = useStore((s) => s.wsl);
  return (
    <Modal onClose={close} ariaLabel="Open from WSL" style={{ width: "min(420px, 100%)" }}>
      <div className="mh">
        <SquareTerminal size={15} strokeWidth={1.7} aria-hidden="true" />
        <span style={{ flex: 1, minWidth: 0 }}>Open from WSL</span>
        <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
      </div>
      <div className="mb">
        <div className="desc" style={{ lineHeight: 1.5 }}>Pick a distro, then a folder inside it. Commands and git for that project run in the distro.</div>
        <div className="sgroup" style={{ borderRadius: 10, overflow: "hidden" }}>
          {wsl.map((d, i) => (
            <button key={d.name} type="button" className="srowx" onClick={() => { close(); void pickWslFolder(d); }}
              style={{ width: "100%", textAlign: "left", cursor: "pointer", background: "none", border: 0, borderTop: i ? "1px solid rgba(255,255,255,0.05)" : 0 }}>
              <span style={{ color: "var(--mut2)", display: "flex", flex: "none" }}><SquareTerminal size={14} strokeWidth={1.7} aria-hidden="true" /></span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontWeight: 500 }}>{d.name}{i === 0 && wsl.length > 1 && <span className="desc"> · default</span>}</div>
                <div className="desc mono" style={{ fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{d.root}</div>
              </div>
              <span style={{ color: "#45454b", flex: "none" }}>›</span>
            </button>
          ))}
          {!wsl.length && <div className="srowx desc">No WSL distros are installed.</div>}
        </div>
      </div>
    </Modal>
  );
}

function CloneDialog() {
  const [url, setUrl] = useState("");
  const [plan, setPlan] = useState<ClonePlan | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  // The clone outlives the dialog if it's closed mid-way; this is how the
  // result knows whether there is still a dialog to open the project from.
  const mounted = useRef(true);
  useEffect(() => () => { mounted.current = false; }, []);

  // The preview is the backend's own answer — same parser, same folder choice —
  // so what it shows is exactly what Clone will do.
  useEffect(() => {
    if (!url.trim()) { setPlan(null); setErr(""); return; }
    let stale = false;
    const t = window.setTimeout(() => {
      api.clonePreview(url)
        .then((p) => { if (!stale) { setPlan(p); setErr(""); } })
        .catch((e) => { if (!stale) { setPlan(null); setErr(String(e)); } });
    }, 180);
    return () => { stale = true; window.clearTimeout(t); };
  }, [url]);

  const clone = async () => {
    if (busy || !plan) return;
    setBusy(true);
    setErr("");
    try {
      const path = await api.cloneRepo(url);
      if (mounted.current && get().openFrom === "clone") {
        close();
        await openProject(path);
      } else {
        // Closed while cloning: don't switch projects under the user.
        flash(`Cloned ${baseName(path)} into ${path} · Clone the same link again to open it`);
      }
    } catch (e) {
      if (mounted.current) { setErr(String(e)); setBusy(false); }
      else flash(`Clone failed: ${String(e)}`);
    }
  };

  return (
    <Modal onClose={close} ariaLabel="Clone a repository" style={{ width: "min(480px, 100%)" }}>
      <div className="mh">
        <FolderGit2 size={15} strokeWidth={1.7} aria-hidden="true" />
        <span style={{ flex: 1, minWidth: 0 }}>Clone a repository</span>
        <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
      </div>
      <div className="mb">
        <Input aria-label="Repository URL" className="input mono" autoFocus spellCheck={false} autoCapitalize="off" autoCorrect="off"
          placeholder="https://github.com/owner/repo" value={url} disabled={busy}
          onChange={(e) => setUrl(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void clone(); } }} />
        <div className="desc" style={{ lineHeight: 1.5, minHeight: 34, overflowWrap: "anywhere" }}>
          {plan
            ? plan.existing
              ? <>Already cloned at <span className="mono">{plan.dest}</span> — this opens it.</>
              : <>Clones into <span className="mono">{plan.dest}</span></>
            : <>GitHub links, <span className="mono">git@host:owner/repo</span> and other git URLs work. Repositories go in the <span className="mono">openleash</span> folder in your home folder.</>}
        </div>
        {err && <div role="alert" style={{ color: "#ff8a8a", fontSize: 12, whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{err}</div>}
      </div>
      <div className="mf">
        <span className="desc" style={{ flex: 1, minWidth: 0, fontSize: 11 }}>{busy ? "Keeps going if you close this." : "Uses your git and its credentials."}</span>
        <Button variant="ghost" onClick={close}>{busy ? "Close" : "Cancel"}</Button>
        <Button variant="primary" disabled={!plan || busy} onClick={() => void clone()}>
          {busy ? <><Loader size={12} /> Cloning…</> : plan?.existing ? "Open" : "Clone"}
        </Button>
      </div>
    </Modal>
  );
}

type SshStep =
  | { k: "hosts" }
  | { k: "testing"; id: string }
  | { k: "ok"; id: string; test: SshHostTest }
  | { k: "failed"; id: string; error: string }
  | { k: "browse"; id: string; test: SshHostTest };

/**
 * Open over SSH: pick (or add) a saved host, check it end to end, browse its
 * folders, and open the chosen one as `ssh://<host-id>/<path>`. The project
 * check itself is `openProject`'s, so a host the backend can't use is
 * reported from there, not guessed at here.
 */
export function SshDialog() {
  const hosts = useStore((s) => s.sshHosts);
  const [step, setStep] = useState<SshStep>({ k: "hosts" });
  const [name, setName] = useState("");
  const [target, setTarget] = useState("");
  const [addErr, setAddErr] = useState("");
  const [adding, setAdding] = useState(false);
  const seq = useRef(0);
  useEffect(() => { void loadSshHosts(); }, []);
  useEffect(() => () => { seq.current++; }, []);
  const hostName = (id: string) => hosts?.find((h) => h.id === id)?.name ?? id;

  const test = (id: string) => {
    const n = ++seq.current;
    setStep({ k: "testing", id });
    sshApi.test(id)
      .then((t) => { if (n === seq.current) setStep({ k: "ok", id, test: t }); })
      .catch((e) => { if (n === seq.current) setStep({ k: "failed", id, error: String(e) }); });
  };
  const add = async () => {
    if (!target.trim() || adding) return;
    setAdding(true);
    setAddErr("");
    try {
      const before = new Set((hosts ?? []).map((h) => h.id));
      const list = await sshApi.save(null, name, target);
      set({ sshHosts: list });
      setName("");
      setTarget("");
      const added = list.find((h) => !before.has(h.id));
      if (added) test(added.id);
    } catch (e) {
      setAddErr(String(e));
    } finally {
      setAdding(false);
    }
  };
  const settings = () => { close(); go("settings", { settingsTab: "remote" }); };

  if (step.k === "browse") {
    const { id } = step;
    return (
      <PickerDialog
        opts={{ mode: "open", directory: true, multiple: false, title: `Open a folder on ${hostName(id)}`, defaultPath: step.test.home }}
        list={(path, dirsOnly) => sshApi.listDir(id, path, dirsOnly)}
        where={`On ${hostName(id)} over SSH`}
        onDone={(v) => {
          const path = Array.isArray(v) ? v[0] : v;
          if (!path) { setStep({ k: "ok", id, test: step.test }); return; }
          close();
          void openProject(sshUri(id, path));
        }} />
    );
  }

  const testing = step.k === "testing";
  return (
    <Modal onClose={close} ariaLabel="Open over SSH" style={{ width: "min(520px, calc(100vw - 24px))" }}>
      <div className="mh">
        <Server size={15} strokeWidth={1.7} aria-hidden="true" />
        <span style={{ flex: 1, minWidth: 0 }}>Open over SSH</span>
        <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
      </div>
      <div className="mb">
        {step.k === "hosts" ? <>
          <div className="desc" style={{ lineHeight: 1.5 }}>The project stays on that machine and the agent's commands, file edits and git run there. Uses your <span className="mono">ssh</span> client, config and keys; password prompts aren't supported.</div>
          <div className="sgroup" role="list" aria-label="Saved SSH hosts" style={{ borderRadius: 10, overflow: "hidden" }}>
            {hosts === null && <div className="srowx"><Loader size={12} label="Loading hosts…" /></div>}
            {hosts?.map((h, i) => (
              <button key={h.id} type="button" role="listitem" className="srowx" onClick={() => test(h.id)}
                style={{ width: "100%", textAlign: "left", cursor: "pointer", background: "none", border: 0, borderTop: i ? "1px solid rgba(255,255,255,0.05)" : 0 }}>
                <span style={{ color: "var(--mut2)", display: "flex", flex: "none" }}><Server size={14} strokeWidth={1.7} aria-hidden="true" /></span>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 500 }}>{h.name}</div>
                  <div className="desc mono" style={{ fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>ssh {h.target}</div>
                </div>
                <span style={{ color: "#45454b", flex: "none" }}>›</span>
              </button>
            ))}
            {hosts?.length === 0 && <div className="srowx desc">No SSH hosts saved yet. Add one below.</div>}
            <div className="srowx" style={{ gap: 8, flexWrap: "wrap", borderTop: hosts?.length ? "1px solid rgba(255,255,255,0.05)" : 0 }}>
              <Input aria-label="Host name" className="input" style={{ width: 130, flex: "1 1 110px" }} value={name} placeholder="Dev box" onChange={(e) => setName(e.currentTarget.value)} />
              <Input aria-label="SSH destination" className="input mono" style={{ flex: "2 1 160px", minWidth: 0 }} value={target} placeholder="me@devbox or an ssh alias" spellCheck={false} autoCapitalize="off" autoCorrect="off"
                onChange={(e) => setTarget(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void add(); } }} />
              <Button disabled={!target.trim() || adding} onClick={() => void add()}>{adding ? "Adding…" : "Add"}</Button>
            </div>
          </div>
          {addErr && <div role="alert" style={{ color: "#ff8a8a", fontSize: 12, overflowWrap: "anywhere" }}>{addErr}</div>}
        </> : (
          <div aria-live="polite" style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            <div style={{ fontWeight: 500 }}>{hostName(step.id)}</div>
            {testing && <div className="desc" style={{ display: "flex", alignItems: "center", gap: 8 }}><Loader size={12} /> Connecting and checking the machine…</div>}
            {step.k === "ok" && <>
              <div className="desc" style={{ lineHeight: 1.5 }}>Connected · {step.test.os || "unknown OS"} · home <span className="mono">{step.test.home}</span></div>
              {(!step.test.has_git || !step.test.has_rg) && <div className="desc" style={{ lineHeight: 1.5 }}>Not installed there: {[!step.test.has_git && "git", !step.test.has_rg && "rg"].filter(Boolean).join(", ")}.</div>}
            </>}
            {step.k === "failed" && <>
              <div role="alert" style={{ color: "#ff8a8a", fontSize: 12, whiteSpace: "pre-wrap", overflowWrap: "anywhere", maxHeight: 220, overflowY: "auto" }}>{step.error}</div>
              <div className="desc" style={{ lineHeight: 1.5 }}>Check that <span className="mono">ssh {hosts?.find((h) => h.id === step.id)?.target ?? "…"}</span> works in a terminal without asking for a password.</div>
            </>}
          </div>
        )}
      </div>
      <div className="mf">
        <span className="desc" style={{ flex: 1, minWidth: 0, fontSize: 11 }}>
          <TextButton link onClick={settings}>Manage hosts in Settings → Remote</TextButton>
        </span>
        {step.k !== "hosts" && <Button variant="ghost" onClick={() => { seq.current++; setStep({ k: "hosts" }); }}>Back</Button>}
        {step.k === "failed" && <Button onClick={() => test(step.id)}>Retry</Button>}
        {step.k === "ok" && <Button variant="primary" onClick={() => setStep({ k: "browse", id: step.id, test: step.test })}>Browse folders</Button>}
        {step.k === "hosts" && <Button variant="ghost" onClick={close}>Cancel</Button>}
        {testing && <Button variant="primary" disabled onClick={() => {}}><Loader size={12} /> Testing…</Button>}
      </div>
    </Modal>
  );
}
