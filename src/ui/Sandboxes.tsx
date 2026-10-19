import { useEffect, useMemo, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  ArrowLeft, Bot, Terminal, Camera, Copy, Download, FolderOpen, HardDrive, Hand, Link2, Link2Off, Monitor, Play, Plus, RotateCcw, Square, Trash2, Upload, User, X,
} from "lucide-react";
import { fmtBytes, sandboxApi, useSandboxThumb, useSandboxes, type Sandbox, type SandboxFile } from "../sandboxApi";
import { flash, go, useStore } from "../store";
import { baseName } from "../api";
import { CONTAINER_LABEL, workspacesApi, type ContainerState } from "../workspaces";
import { getStudioView, patchStudioView } from "./studioViewState";
import { Button, ConfirmDialog, Input, Switch, TextArea, Tooltip } from "./primitives";

/** The artifict studio, for containers. A card per sandbox (a computer of the
 *  agent's own), a live view of the one you open, and everything you can do to
 *  it: take the mouse, move files, snapshot, clone, reset, delete. */
export function Sandboxes({ taskId, browseOnly = false }: { taskId: string; browseOnly?: boolean }) {
  const { data, error, reload } = useSandboxes();
  const attached = useStore((s) => s.tasks[taskId]?.sandbox ?? null);
  const [openId, setOpenId] = useState<string | null>(() => (browseOnly ? null : getStudioView(taskId).sandboxId ?? null));
  useEffect(() => { if (!browseOnly) patchStudioView(taskId, { sandboxId: openId }); }, [browseOnly, taskId, openId]);
  const [creating, setCreating] = useState(false);
  const [build, setBuild] = useState("");
  useEffect(() => {
    let un: (() => void) | undefined; let dead = false;
    void Promise.resolve(listen<{ line?: string; done?: boolean; error?: string | null }>("ol://sandbox-build", ({ payload }) => {
      if (payload.done) { setBuild(""); if (payload.error) flash(payload.error); }
      else if (payload.line) setBuild(payload.line);
    })).then((u) => { if (typeof u === "function") { if (dead) u(); else un = u; } });
    return () => { dead = true; un?.(); };
  }, []);
  // Install the desktop image as soon as the view opens, so starting a sandbox
  // never waits on it. `preparing` also keeps "New sandbox" from racing it.
  const [preparing, setPreparing] = useState(false);
  const needsImage = !!data && !data.dockerError && !data.imageReady;
  useEffect(() => {
    if (!needsImage || preparing) return;
    setPreparing(true);
    sandboxApi.prepare().catch((e) => flash(String(e))).finally(() => { setPreparing(false); void reload(); });
  }, [needsImage]); // eslint-disable-line react-hooks/exhaustive-deps
  const list = data?.sandboxes ?? [];
  const open = list.find((s) => s.id === openId) ?? null;
  useEffect(() => { if (openId && data && !open) setOpenId(null); }, [openId, data, open]);

  if (open) return <SandboxDetail sb={open} taskId={taskId} browseOnly={browseOnly} attached={attached === open.id} build={build} onBack={() => setOpenId(null)} reload={reload} />;
  return <div className="sbx-gallery">
    {data?.dockerError && <div className="sbx-banner" role="alert"><HardDrive size={14} aria-hidden="true" /><span>{data.dockerError} Sandboxes run in Docker.</span></div>}
    {error && <div className="sbx-banner" role="alert">{error}</div>}
    {preparing && !build && <div className="sbx-banner info" role="status"><span className="studio-agent-dot" aria-hidden="true" /><span>Installing the desktop image{data?.image ? ` (${data.image})` : " (Ubuntu + XFCE)"}… first time only.</span></div>}
    {build && <div className="sbx-banner info" role="status"><span className="studio-agent-dot" aria-hidden="true" /><span className="sbx-build">{build}</span></div>}
    <div className="sbx-cards">
      {list.map((sb) => <SandboxCard key={sb.id} sb={sb} here={attached === sb.id} onOpen={() => setOpenId(sb.id)} />)}
      <button type="button" className="sbx-new" onClick={() => setCreating(true)} disabled={!!data?.dockerError || preparing}><Plus size={18} aria-hidden="true" /><span>New sandbox</span></button>
    </div>
    <TaskContainers />
    {data && !list.length && <div className="artifict-empty">
      <Monitor size={22} aria-hidden="true" />
      <p className="studio-blank-title">No sandboxes yet.</p>
      <p>A sandbox is a computer of the agent's own: a Linux desktop in a container. Make one here, or just ask the agent to launch one. You can watch it live, take the mouse, move files in and out, and swap which sandbox a chat uses at any time.</p>
    </div>}
    {creating && <NewSandboxDialog onClose={() => setCreating(false)} onCreated={(sb) => { setCreating(false); void reload(); setOpenId(sb.id); }} />}
  </div>;
}

/** The containers behind docker-workspace chats (the "Docker" choice on the new-task screen). They are
 *  command containers with no desktop, so they get a card each with start/stop and a way to the chat. */
function TaskContainers() {
  const tasks = useStore((s) => s.tasks);
  const rows = Object.values(tasks).filter((t) => t.docker && !t.archived);
  const [busy, setBusy] = useState("");
  if (!rows.length) return null;
  const act = (id: string, f: () => Promise<unknown>) => { setBusy(id); f().catch((e) => flash(String(e))).finally(() => setBusy("")); };
  return <>
    <div className="sbx-section">Task containers <span className="desc">from chats started in Docker</span></div>
    <div className="sbx-cards">{rows.map((t) => {
      const state: ContainerState = t.container ?? "unknown";
      const on = state === "running" || state === "starting" || state === "building";
      return <div key={t.id} className="artifict-card sbx-card" data-state={on ? "running" : "stopped"}>
        <div className="artifict-card-frame sbx-frame"><div className="sbx-off"><Terminal size={22} aria-hidden="true" /><span>{CONTAINER_LABEL[state]}</span></div>
          <span className="sbx-state"><i data-state={state === "running" ? "running" : "stopped"} />{CONTAINER_LABEL[state]}</span></div>
        <span className="artifict-row-title">{t.title || "Untitled chat"}</span>
        <span className="artifict-row-meta">{baseName(t.project)}{t.branch ? ` · ${t.branch}` : ""}</span>
        <div className="sbx-actions" style={{ marginTop: 0, position: "relative", zIndex: 3 }}>
          <Button onClick={() => go("session", { task: t.id })}>Open chat</Button>
          {on ? <Button disabled={busy === t.id} onClick={() => act(t.id, () => workspacesApi.stop(t.id))}><Square size={12} aria-hidden="true" /> Stop</Button>
            : <Button disabled={busy === t.id} onClick={() => act(t.id, () => workspacesApi.start(t.id))}><Play size={12} aria-hidden="true" /> Start</Button>}
        </div>
      </div>;
    })}</div>
  </>;
}

function DriverBadge({ sb }: { sb: Sandbox }) {
  if (sb.driver.kind === "agent") return <span className="sbx-badge agent"><Bot size={10} aria-hidden="true" />Agent driving</span>;
  if (sb.driver.kind === "user") return <span className="sbx-badge you"><Hand size={10} aria-hidden="true" />You</span>;
  return null;
}

function SandboxCard({ sb, here, onOpen }: { sb: Sandbox; here: boolean; onOpen: () => void }) {
  const src = useSandboxThumb(sb.id, sb.state === "running", 6000, JSON.stringify(sb.driver).length);
  return <div className="artifict-card sbx-card" data-state={sb.state}>
    <div className="artifict-card-frame sbx-frame">
      {src ? <img className="artifict-thumb-media" alt="" src={src} /> : <div className="sbx-off"><Monitor size={22} aria-hidden="true" /><span>{sb.state === "running" ? "Loading…" : "Powered off"}</span></div>}
      <span className="sbx-state"><i data-state={sb.state} />{sb.state === "running" ? "Running" : sb.state === "stopped" ? "Stopped" : "Not started"}</span>
      <DriverBadge sb={sb} />
    </div>
    <span className="artifict-row-title">{sb.name}</span>
    <span className="artifict-row-meta">{sb.width}×{sb.height}{sb.snapshots.length ? ` · ${sb.snapshots.length} snapshot${sb.snapshots.length === 1 ? "" : "s"}` : ""}{sb.files ? ` · ${sb.files} file${sb.files === 1 ? "" : "s"}` : ""}{here ? " · in this chat" : ""}</span>
    <button type="button" className="artifict-card-open" aria-label={`Open ${sb.name}`} onClick={onOpen} />
  </div>;
}

function NewSandboxDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (sb: Sandbox) => void }) {
  const [name, setName] = useState("");
  const [size, setSize] = useState("1280x800");
  const [busy, setBusy] = useState(false);
  const submit = () => {
    const [w, h] = size.split("x").map(Number);
    setBusy(true);
    sandboxApi.create({ name: name.trim() || "My sandbox", width: w, height: h }).then(onCreated).catch((e) => flash(String(e))).finally(() => setBusy(false));
  };
  return <ConfirmDialog title="New sandbox" confirm={busy ? "Creating…" : "Create"} onClose={onClose} onConfirm={submit} body={<div className="sbx-form">
    <label>Name<Input aria-label="Sandbox name" autoFocus value={name} placeholder="Design lab" onChange={(e) => setName(e.currentTarget.value)} /></label>
    <label>Screen<select aria-label="Screen size" className="sbx-select" value={size} onChange={(e) => setSize(e.currentTarget.value)}>
      {["1024x768", "1280x800", "1440x900", "1920x1080"].map((s) => <option key={s} value={s}>{s.replace("x", " × ")}</option>)}
    </select></label>
    <div className="desc">Open it to start it. The first one builds a desktop image (a few minutes, once).</div>
  </div>} />;
}

type Side = "settings" | "snapshots" | "files";
type Run = (key: string, f: () => Promise<unknown>, done?: string) => void;

function SandboxDetail({ sb, taskId, browseOnly, attached, build, onBack, reload }: { sb: Sandbox; taskId: string; browseOnly: boolean; attached: boolean; build: string; onBack: () => void; reload: () => Promise<unknown> }) {
  const [side, setSide] = useState<Side | null>(null);
  const [busy, setBusy] = useState("");
  const [takeover, setTakeover] = useState(false);
  const [confirm, setConfirm] = useState<null | "delete" | "reset">(null);
  const [name, setName] = useState(sb.name);
  useEffect(() => setName(sb.name), [sb.name]);
  // The mouse is handed back whenever this view goes away, so closing the tab
  // can't leave the agent locked out of its own computer.
  useEffect(() => () => { void sandboxApi.control(sb.id, false).catch(() => undefined); }, [sb.id]);
  useEffect(() => { if (sb.state !== "running") setTakeover(false); }, [sb.state]);
  const run: Run = (key, f, done) => {
    setBusy(key);
    f().then(() => { if (done) flash(done); return reload(); }).catch((e) => flash(String(e))).finally(() => setBusy(""));
  };
  const toggleTakeover = () => {
    const next = !takeover;
    setTakeover(next);
    sandboxApi.control(sb.id, next).catch((e) => { setTakeover(!next); flash(String(e)); });
  };
  // View-only unless you took over: you and the agent never fight for the pointer.
  const url = useMemo(() => sb.url ? `${sb.url}${takeover ? "" : "&view_only=1"}` : null, [sb.url, takeover]);
  const running = sb.state === "running";
  const rename = () => { const n = name.trim(); if (n && n !== sb.name) run("rename", () => sandboxApi.update(sb.id, { name: n })); else setName(sb.name); };
  const start = <Button variant="primary" onClick={() => run("start", () => sandboxApi.start(sb.id))} disabled={!!busy}><Play size={12} aria-hidden="true" /> {busy === "start" ? "Starting…" : "Start"}</Button>;
  return <div className="sbx-detail">
    <div className="sbx-bar" role="toolbar" aria-label="Sandbox tools">
      <Tooltip content="All sandboxes" side="bottom"><button type="button" className="studio-tool" aria-label="All sandboxes" onClick={onBack}><ArrowLeft size={15} aria-hidden="true" /></button></Tooltip>
      <input className="sbx-name" aria-label="Sandbox name" value={name} onChange={(e) => setName(e.currentTarget.value)} onBlur={rename} onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} />
      <span className="sbx-state"><i data-state={sb.state} />{running ? "Running" : sb.state === "stopped" ? "Stopped" : "Not started"}</span>
      <DriverBadge sb={sb} />
      <span className="studio-spacer" />
      {running && <Tooltip content={takeover ? "Give the mouse and keyboard back to the agent" : "Take the mouse and keyboard. The agent waits until you hand it back."} side="bottom"><button type="button" className={`studio-tool${takeover ? " on" : ""}`} aria-pressed={takeover} onClick={toggleTakeover}><Hand size={14} aria-hidden="true" />{takeover ? "Hand back" : "Take over"}</button></Tooltip>}
      {!browseOnly && <Tooltip content={attached ? "This chat stops using it" : "Make this chat use this sandbox for the screen and commands"} side="bottom"><button type="button" className={`studio-tool${attached ? " on" : ""}`} disabled={!!busy} onClick={() => run("attach", () => sandboxApi.attach(taskId, attached ? null : sb.id), attached ? "Chat detached" : "This chat now uses it")}>{attached ? <Link2Off size={14} aria-hidden="true" /> : <Link2 size={14} aria-hidden="true" />}{attached ? "Detach" : "Use in this chat"}</button></Tooltip>}
      {running && !browseOnly && <Tooltip content="Save what the screen shows as an artifict" side="bottom"><button type="button" className="studio-tool" aria-label="Screenshot to artifict" disabled={!!busy} onClick={() => run("shot", () => sandboxApi.publish(taskId, sb.id, `${sb.name} screen`), "Saved to Artificts")}><Camera size={14} aria-hidden="true" /></button></Tooltip>}
      {running ? <Button onClick={() => run("stop", () => sandboxApi.stop(sb.id))} disabled={!!busy}><Square size={12} aria-hidden="true" /> Stop</Button> : start}
      <div className="studio-bar-group studio-seg" role="group" aria-label="Side panel">
        {(["settings", "snapshots", "files"] as Side[]).map((k) => <button key={k} type="button" aria-pressed={side === k} className={side === k ? "on" : ""} onClick={() => setSide(side === k ? null : k)}>{k.charAt(0).toUpperCase() + k.slice(1)}{k === "snapshots" && sb.snapshots.length ? ` ${sb.snapshots.length}` : ""}{k === "files" && sb.files ? ` ${sb.files}` : ""}</button>)}
      </div>
    </div>
    <div className="sbx-body">
      <div className="sbx-stage" aria-label="Sandbox screen">
        {running && url
          ? <iframe key={url} title={`${sb.name} screen`} className="sbx-live" src={url} allow="clipboard-read; clipboard-write" />
          : <div className="sbx-stage-off">
              <div className="sbx-stage-msg"><Monitor size={26} aria-hidden="true" /><b>{sb.state === "absent" ? "Not started yet" : "Powered off"}</b>
                {build && busy === "start" ? <span className="sbx-build">{build}</span> : <span>Everything on its disk is kept.</span>}
                {start}</div>
            </div>}
        {takeover && <div className="studio-hint sbx-hint" role="status">You have the mouse and keyboard — the agent is paused</div>}
      </div>
      {side && <aside className="sbx-side" aria-label={`Sandbox ${side}`}>
        {side === "settings" && <SettingsPanel sb={sb} busy={busy} run={run} onClone={() => run("clone", () => sandboxApi.clone(sb.id, `${sb.name} copy`), "Cloned")} onReset={() => setConfirm("reset")} onDelete={() => setConfirm("delete")} />}
        {side === "snapshots" && <SnapshotsPanel sb={sb} busy={busy} run={run} />}
        {side === "files" && <FilesPanel sb={sb} run={run} />}
      </aside>}
    </div>
    {confirm === "delete" && <ConfirmDialog danger title={`Delete ${sb.name}?`} body="Its disk, snapshots and shared files are removed for good. Chats using it go back to the host." confirm="Delete" onClose={() => setConfirm(null)} onConfirm={() => { sandboxApi.remove(sb.id).then(() => { flash("Deleted"); onBack(); return reload(); }).catch((e) => flash(String(e))); }} />}
    {confirm === "reset" && <ConfirmDialog danger title={`Reset ${sb.name}?`} body="The machine is thrown away and starts again from a fresh desktop. Shared files and snapshots are kept." confirm="Reset" onClose={() => setConfirm(null)} onConfirm={() => run("reset", () => sandboxApi.reset(sb.id), "Reset")} />}
  </div>;
}

function Row({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return <div className="sbx-row"><div style={{ flex: 1 }}><div>{label}</div>{hint && <div className="desc">{hint}</div>}</div>{children}</div>;
}

function SettingsPanel({ sb, busy, run, onClone, onReset, onDelete }: { sb: Sandbox; busy: string; run: Run; onClone: () => void; onReset: () => void; onDelete: () => void }) {
  const [notes, setNotes] = useState(sb.notes);
  const [mem, setMem] = useState(sb.memory);
  const [cpus, setCpus] = useState(String(sb.cpus || ""));
  useEffect(() => { setNotes(sb.notes); setMem(sb.memory); setCpus(String(sb.cpus || "")); }, [sb.id, sb.notes, sb.memory, sb.cpus]);
  return <div className="studio-panel-body">
    <Row label="Agent may use the screen" hint="Click and type on this desktop."><Switch label="Agent may use the screen" checked={sb.computer} onChange={(v) => run("computer", () => sandboxApi.update(sb.id, { computer: v }))} /></Row>
    <Row label="Agent may run commands" hint="A shell inside this sandbox only."><Switch label="Agent may run commands" checked={sb.shell} onChange={(v) => run("shell", () => sandboxApi.update(sb.id, { shell: v }))} /></Row>
    <Row label="Memory" hint="e.g. 2g. Empty for no cap."><Input aria-label="Memory limit" style={{ width: 80 }} value={mem} onChange={(e) => setMem(e.currentTarget.value)} onBlur={() => mem !== sb.memory && run("limits", () => sandboxApi.update(sb.id, { memory: mem }))} /></Row>
    <Row label="CPUs" hint="0 for no cap."><Input aria-label="CPU limit" style={{ width: 80 }} value={cpus} onChange={(e) => setCpus(e.currentTarget.value)} onBlur={() => Number(cpus || 0) !== sb.cpus && run("limits", () => sandboxApi.update(sb.id, { cpus: Number(cpus || 0) }))} /></Row>
    <div className="sbx-row col"><div>Notes</div><TextArea aria-label="Notes" rows={3} value={notes} placeholder="What this computer is for" onChange={(e) => setNotes(e.currentTarget.value)} onBlur={() => notes !== sb.notes && run("notes", () => sandboxApi.update(sb.id, { notes }))} /></div>
    <div className="sbx-actions">
      <Button onClick={onClone} disabled={!!busy}><Copy size={12} aria-hidden="true" /> Clone</Button>
      <Button onClick={onReset} disabled={!!busy}><RotateCcw size={12} aria-hidden="true" /> Reset</Button>
      <Button onClick={onDelete} disabled={!!busy} style={{ color: "#ff8a8a" }}><Trash2 size={12} aria-hidden="true" /> Delete</Button>
    </div>
    <div className="desc" style={{ marginTop: 10 }}>Created {new Date(sb.created).toLocaleDateString()} · {sb.width}×{sb.height}{sb.fromSnapshot ? " · from a snapshot" : ""}. Memory and CPU apply live; screen size changes on Reset.</div>
  </div>;
}

function SnapshotsPanel({ sb, busy, run }: { sb: Sandbox; busy: string; run: Run }) {
  const [name, setName] = useState("");
  return <div className="studio-panel-body">
    <div className="sbx-row"><Input aria-label="Snapshot name" value={name} placeholder="Before the install" onChange={(e) => setName(e.currentTarget.value)} /><Button disabled={!!busy || sb.state === "absent"} onClick={() => run("snap", async () => { await sandboxApi.snapshot(sb.id, name.trim() || new Date().toLocaleString()); setName(""); }, "Snapshot saved")}><Camera size={12} aria-hidden="true" /> Save</Button></div>
    <div className="desc">A snapshot freezes the disk (not running programs). Restore it to go back; the current disk is discarded.</div>
    {sb.snapshots.length === 0 && <div className="studio-empty-note"><p>No snapshots yet.</p></div>}
    {[...sb.snapshots].reverse().map((s) => <div key={s.id} className="sbx-snap">
      <div style={{ flex: 1, minWidth: 0 }}><div className="sbx-snap-name">{s.name}</div><div className="desc">{new Date(s.created).toLocaleString()}</div></div>
      <Button disabled={!!busy} onClick={() => run("restore", () => sandboxApi.restore(sb.id, s.id), "Restored — start it to continue")}>Restore</Button>
      <Tooltip content="Delete snapshot" side="left"><button type="button" className="studio-tool" aria-label={`Delete snapshot ${s.name}`} disabled={!!busy} onClick={() => run("snapdel", () => sandboxApi.deleteSnapshot(sb.id, s.id))}><X size={13} aria-hidden="true" /></button></Tooltip>
    </div>)}
  </div>;
}

function FilesPanel({ sb, run }: { sb: Sandbox; run: Run }) {
  const [files, setFiles] = useState<SandboxFile[]>([]);
  useEffect(() => { let dead = false; sandboxApi.files(sb.id).then((f) => { if (!dead) setFiles(f); }).catch(() => undefined); return () => { dead = true; }; }, [sb.id, sb.files]);
  const add = async (directory: boolean) => {
    const picked = await openDialog({ multiple: true, directory });
    const paths = picked ? (Array.isArray(picked) ? picked : [picked]) : [];
    if (paths.length) run("import", () => sandboxApi.importFiles(sb.id, paths), "Copied in");
  };
  const take = async (f: SandboxFile) => {
    const dest = await saveDialog({ defaultPath: f.path.split("/").pop() });
    if (dest) run("export", () => sandboxApi.exportFile(sb.id, f.path, dest), "Saved");
  };
  return <div className="studio-panel-body">
    <div className="sbx-actions" style={{ marginTop: 0 }}>
      <Button onClick={() => void add(false)}><Upload size={12} aria-hidden="true" /> Add files</Button>
      <Button onClick={() => void add(true)}><FolderOpen size={12} aria-hidden="true" /> Add folder</Button>
    </div>
    <div className="desc">This folder is /home/agent/shared inside the sandbox. Drop things there for the agent; it leaves results here for you.</div>
    {files.length === 0 && <div className="studio-empty-note"><User size={14} aria-hidden="true" /><p>Nothing shared yet.</p></div>}
    {files.map((f) => <div key={f.path} className="sbx-file">
      <span className="sbx-file-name" title={f.path}>{f.dir ? "📁 " : ""}{f.path}</span>
      <span className="desc">{f.dir ? "" : fmtBytes(f.size)}</span>
      {!f.dir && <Tooltip content="Save a copy…" side="left"><button type="button" className="studio-tool" aria-label={`Save ${f.path}`} onClick={() => void take(f)}><Download size={13} aria-hidden="true" /></button></Tooltip>}
      <Tooltip content="Delete" side="left"><button type="button" className="studio-tool" aria-label={`Delete ${f.path}`} onClick={() => run("rmfile", () => sandboxApi.deleteFile(sb.id, f.path))}><Trash2 size={13} aria-hidden="true" /></button></Tooltip>
    </div>)}
  </div>;
}
