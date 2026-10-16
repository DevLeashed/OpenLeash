import { useEffect, useRef, useState } from "react";
import { ArrowUp, File, Folder, X } from "lucide-react";
import { isRemoteClient, remoteApi, type DirListing, type PickOptions } from "../remote";
import { Button, IconButton, Input, Loader, Modal } from "./primitives";

type Pending = { opts: PickOptions; resolve: (v: string | string[] | null) => void };

const sepOf = (p: string) => (p.includes("\\") ? "\\" : "/");
const join = (dir: string, name: string) => (dir.endsWith("/") || dir.endsWith("\\") ? dir + name : dir + sepOf(dir) + name);

/**
 * Stands in for the native open/save dialog when the UI runs on another
 * device: a browser can't open a dialog on the host, and one opened on the
 * desktop would sit there with nobody looking at it. `shim.js` routes
 * `plugin:dialog|open` / `save` to the `pick` hook this registers.
 */
export function RemotePicker() {
  const [pending, setPending] = useState<Pending | null>(null);
  useEffect(() => {
    if (!isRemoteClient()) return;
    const bridge = window.__OPENLEASH_REMOTE__!;
    bridge.pick = (opts) => new Promise((resolve) => setPending((prev) => {
      // A second dialog while one is open cancels the first, like a native one would.
      prev?.resolve(null);
      return { opts, resolve };
    }));
    return () => { delete bridge.pick; };
  }, []);
  if (!pending) return null;
  const done = (v: string | string[] | null) => { pending.resolve(v); setPending(null); };
  return <PickerDialog opts={pending.opts} onDone={done} />;
}

export function PickerDialog({ opts, onDone, list = remoteApi.listDir, where = "On the computer" }: { opts: PickOptions; onDone: (v: string | string[] | null) => void; list?: (path: string | null, dirsOnly: boolean) => Promise<DirListing>; where?: string }) {
  const [listing, setListing] = useState<DirListing | null>(null);
  const [err, setErr] = useState("");
  const [loading, setLoading] = useState(false);
  const [typed, setTyped] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [name, setName] = useState(() => (opts.mode === "save" && opts.defaultPath ? opts.defaultPath.split(/[\\/]/).pop() ?? "" : ""));
  const seq = useRef(0);
  const load = (path: string | null) => {
    const n = ++seq.current;
    setLoading(true);
    setErr("");
    void list(path, opts.directory).then((l) => {
      if (n !== seq.current) return;
      setListing(l);
      setTyped(l.path);
      setChosen([]);
    }).catch((e) => { if (n === seq.current) setErr(String(e)); }).finally(() => { if (n === seq.current) setLoading(false); });
  };
  useEffect(() => {
    const start = opts.defaultPath && opts.mode === "save" ? opts.defaultPath.replace(/[\\/][^\\/]*$/, "") : opts.defaultPath;
    load(start || null);
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
  const title = opts.title || (opts.mode === "save" ? "Save as" : opts.directory ? "Choose a folder" : opts.multiple ? "Choose files" : "Choose a file");
  const mobile = isRemoteClient() && window.matchMedia?.("(pointer: coarse)").matches;
  const toggle = (path: string) => setChosen((c) => opts.multiple ? (c.includes(path) ? c.filter((x) => x !== path) : [...c, path]) : [path]);
  const confirm = () => {
    if (!listing) return;
    if (opts.mode === "save") { if (name.trim()) onDone(join(listing.path, name.trim())); return; }
    if (opts.directory) { const pick = chosen[0] ?? listing.path; onDone(opts.multiple ? [pick] : pick); return; }
    if (chosen.length) onDone(opts.multiple ? chosen : chosen[0] ?? null);
  };
  const canConfirm = !!listing && (opts.mode === "save" ? !!name.trim() : opts.directory || chosen.length > 0);
  return (
    <Modal onClose={() => onDone(null)} ariaLabel={title} className={mobile ? "remote-picker-mobile" : undefined} style={{ width: "min(560px, 100%)", maxHeight: "min(640px, 90vh)", display: "flex", flexDirection: "column" }}>
      <div className="mh"><span style={{ flex: 1 }}>{title}</span><span className="desc" style={{ fontSize: 11 }}>{where}</span><IconButton label="Close" onClick={() => onDone(null)}><X size={14} /></IconButton></div>
      <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 8, minHeight: 0, flex: 1 }}>
        <div style={{ display: "flex", gap: 6 }}>
          <IconButton label="Up one folder" onClick={() => listing?.parent && load(listing.parent)}><ArrowUp size={14} /></IconButton>
          <Input aria-label="Folder path" className="input mono" style={{ flex: 1, fontSize: 12 }} value={typed} onChange={(e) => setTyped(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") load(typed); }} />
        </div>
        {listing && listing.roots.length > 1 && <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>{listing.roots.map((r) => <Button key={r} variant="ghost" style={{ height: 24, fontSize: 11 }} onClick={() => load(r)}>{r}</Button>)}</div>}
        {err && <div style={{ color: "#ff8a8a", fontSize: 12 }}>{err}</div>}
        <div role="listbox" aria-label="Folder contents" aria-multiselectable={opts.multiple} style={{ overflow: "auto", flex: 1, minHeight: 200, border: "1px solid var(--line)", borderRadius: 8 }}>
          {loading && !listing ? <Loader size={14} label="Loading…" /> : listing?.entries.map((e) => {
            const sel = chosen.includes(e.path);
            return (
              <div key={e.path} role="option" aria-selected={sel} tabIndex={0} className={"picker-row" + (sel ? " on" : "")}
                style={{ display: "flex", alignItems: "center", gap: 8, padding: "7px 10px", cursor: "pointer", background: sel ? "var(--hover)" : undefined }}
                onClick={() => { if (e.dir && !opts.directory) load(e.path); else if (e.dir) toggle(e.path); else if (opts.mode === "save") setName(e.name); else toggle(e.path); }}
                onDoubleClick={() => { if (e.dir) load(e.path); else if (opts.mode !== "save") onDone(opts.multiple ? [e.path] : e.path); }}
                onKeyDown={(k) => { if (k.key === "Enter") { if (e.dir) load(e.path); else toggle(e.path); } }}>
                {e.dir ? <Folder size={14} strokeWidth={1.7} aria-hidden="true" /> : <File size={14} strokeWidth={1.7} aria-hidden="true" />}
                <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{e.name}</span>
              </div>
            );
          })}
          {listing && listing.entries.length === 0 && <div className="desc" style={{ padding: 10 }}>{opts.directory ? "No folders here." : "This folder is empty."}</div>}
          {listing?.truncated && <div className="desc" style={{ padding: 10 }}>Only the first 2000 entries are shown. Type a path above to go further.</div>}
        </div>
        {opts.mode === "save" && <Input aria-label="File name" className="input mono" value={name} placeholder="File name" onChange={(e) => setName(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") confirm(); }} />}
      </div>
      <div className="mf"><span className="desc mono" style={{ flex: 1, fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{opts.directory && !chosen.length ? listing?.path : chosen.join(", ")}</span>
        <Button variant="ghost" onClick={() => onDone(null)}>Cancel</Button>
        <Button variant="primary" disabled={!canConfirm} onClick={confirm}>{opts.mode === "save" ? "Save" : opts.directory ? (chosen.length ? "Choose" : "Use this folder") : "Open"}</Button>
      </div>
    </Modal>
  );
}
