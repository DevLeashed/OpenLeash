import { useEffect, useRef, useState } from "react";
import { MonitorUp } from "lucide-react";
import { sshApi, transferApi, type CodeState, type SshHostView, type TransferLink, type TransferMode, type TransferResult } from "../remote";
import { go, set, useStore } from "../store";
import { I } from "./icons";
import { PickerDialog } from "./RemotePicker";
import { Button, ChoiceRow, IconButton, Input, Loader, Modal, Segmented, TextButton } from "./primitives";

const MODES = [
  { value: "copy" as const, label: "Copy", hint: "Leave this chat here and start an independent copy there" },
  { value: "move" as const, label: "Transfer", hint: "Stop this chat, send it, and archive it here once the other machine confirms the import" },
];

export const openTransfer = (id: string) => set({ transfer: id, menu: null, pal: false });

/** The warning shown when the code would not travel with the chat. Exported for tests. */
export function codeWarning(c: CodeState | null): string {
  if (!c || !c.git) return "";
  const parts: string[] = [];
  if (c.dirty) parts.push(`${c.dirty} uncommitted change${c.dirty === 1 ? "" : "s"}`);
  if (c.ahead === null) parts.push(`branch ${c.branch || "HEAD"} has no upstream`);
  else if (c.ahead > 0) parts.push(`${c.ahead} unpushed commit${c.ahead === 1 ? "" : "s"} on ${c.branch}`);
  return parts.length ? `Only the conversation travels, not the code: this project has ${parts.join(" and ")}. Push or sync it first if the other machine needs it.` : "";
}

/** Mounted once beside the other overlays; shows while `transfer` names a chat. */
export function SendToMachine() {
  const id = useStore((s) => s.transfer);
  const task = useStore((s) => (id ? s.tasks[id] : null));
  if (!id || !task) return null;
  return <SendDialog key={id} taskId={id} title={task.title} running={task.status === "running" || task.status === "waiting"} />;
}

type Phase = "pick" | "connecting" | "ready" | "sending" | "done";

export function SendDialog({ taskId, title, running }: { taskId: string; title: string; running: boolean }) {
  const [hosts, setHosts] = useState<SshHostView[] | null>(null);
  const [hostId, setHostId] = useState<string | null>(null);
  const [link, setLink] = useState<TransferLink | null>(null);
  const [phase, setPhase] = useState<Phase>("pick");
  const [mode, setMode] = useState<TransferMode>("copy");
  const [project, setProject] = useState("");
  const [resume, setResume] = useState(false);
  const [code, setCode] = useState<CodeState | null>(null);
  const [err, setErr] = useState("");
  const [result, setResult] = useState<TransferResult | null>(null);
  const [browsing, setBrowsing] = useState(false);
  // The open link, for closing it however the dialog goes away.
  const live = useRef<string | null>(null);
  // False once the dialog has gone: a link that opens after that is closed
  // straight away instead of being left running with nobody to close it.
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    sshApi.list().then((h) => { if (alive.current) setHosts(h); }).catch((e) => { if (alive.current) { setHosts([]); setErr(String(e)); } });
    transferApi.check(taskId).then((c) => { if (alive.current) setCode(c); }).catch(() => { if (alive.current) setCode(null); });
    return () => {
      alive.current = false;
      if (live.current) void transferApi.close(live.current).catch(() => {});
      live.current = null;
    };
  }, [taskId]);

  const close = () => set({ transfer: null });

  const pickHost = async (h: SshHostView) => {
    if (phase === "connecting" || phase === "sending") return;
    if (live.current) { void transferApi.close(live.current).catch(() => {}); live.current = null; }
    setHostId(h.id);
    setLink(null);
    setErr("");
    setPhase("connecting");
    try {
      const l = await transferApi.open(h.id, taskId);
      if (!alive.current) { void transferApi.close(l.link).catch(() => {}); return; }
      live.current = l.link;
      setLink(l);
      setProject(l.probe?.best ?? "");
      setPhase("ready");
    } catch (e) {
      if (!alive.current) return;
      setErr(String(e));
      setPhase("pick");
    }
  };

  const send = async () => {
    if (!link || phase !== "ready") return;
    setPhase("sending");
    setErr("");
    try {
      const r = await transferApi.send(link.link, taskId, mode, project.trim() || null, resume);
      if (!alive.current) return;
      setResult(r);
      setPhase("done");
    } catch (e) {
      if (!alive.current) return;
      setErr(String(e));
      setPhase("ready");
    }
  };

  const warning = codeWarning(code);
  const machine = link?.probe ? `${link.host_name} (${link.probe.host}, ${link.probe.os})` : link?.host_name ?? "";

  // The folder picker replaces the dialog while it is open rather than
  // stacking on it: two modals would both take the same Escape.
  if (browsing && link) {
    return (
      <PickerDialog opts={{ mode: "open", directory: true, multiple: false, title: `A folder on ${link.host_name}`, defaultPath: project || link.probe?.home || "" }}
        list={(p) => transferApi.listDir(link.link, p)} where={`On ${link.host_name}`}
        onDone={(v) => { setBrowsing(false); if (typeof v === "string") setProject(v); }} />
    );
  }

  return (
    <Modal onClose={close} ariaLabel="Send to another machine" style={{ width: "min(520px, calc(100vw - 24px))" }}>
      <div className="mh">
        <MonitorUp size={15} strokeWidth={1.7} aria-hidden="true" />
        <span style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>Send to another machine</span>
        <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
      </div>
      <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        {phase === "done" && result ? <Done result={result} mode={mode} machine={link?.host_name ?? ""} /> : <>
          <div className="desc" style={{ lineHeight: 1.5 }}>
            Sends <span style={{ color: "var(--mut)" }}>{title || "this chat"}</span> over SSH to OpenLeash on another computer. The agents there are told they moved.
          </div>
          <div role="radiogroup" aria-label="Machine" className="sgroup" style={{ borderRadius: 10, overflow: "hidden" }}>
            {hosts === null && <div className="srowx"><Loader size={13} label="Loading machines…" /></div>}
            {hosts?.map((h) => (
              <ChoiceRow key={h.id} variant="radio" selected={hostId === h.id} onClick={() => void pickHost(h)} className="srowx">
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 500 }}>{h.name}</div>
                  <div className="desc mono" style={{ fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{h.target}</div>
                </div>
                {hostId === h.id && phase === "connecting" && <Loader size={12} />}
              </ChoiceRow>
            ))}
            {hosts?.length === 0 && <div className="srowx desc">No machines yet.</div>}
          </div>
          <div className="desc" style={{ fontSize: 11 }}>
            <TextButton link onClick={() => { close(); go("settings", { settingsTab: "remote" }); }}>Add a machine in Settings → Remote</TextButton>
          </div>
          {link && <>
            <div className="desc" style={{ fontSize: 11.5, lineHeight: 1.5 }}>
              {link.online
                ? <>Connected to {machine}.</>
                : <>OpenLeash isn't reachable on {link.host_name}{link.reason ? ` (${link.reason})` : ""}. The chat can still be left there; it opens the next time OpenLeash starts on that machine.</>}
            </div>
            <Segmented label="Copy or transfer" options={MODES} value={mode} onChange={setMode} />
            <div className="desc" style={{ fontSize: 11 }}>{MODES.find((m) => m.value === mode)?.hint}.</div>
            <label className="desc" htmlFor="xfer-project" style={{ fontSize: 11.5 }}>Project folder on {link.host_name}</label>
            <div style={{ display: "flex", gap: 6 }}>
              <Input id="xfer-project" className="input mono" style={{ flex: 1, minWidth: 0, fontSize: 12 }} spellCheck={false}
                placeholder={link.online ? "Pick one, or leave empty to match it automatically" : "Leave empty to match it automatically"}
                value={project} onChange={(e) => setProject(e.currentTarget.value)} />
              <Button disabled={!link.online} onClick={() => setBrowsing(true)}>Browse…</Button>
            </div>
            {link.probe?.best && project === link.probe.best && <div className="desc" style={{ fontSize: 11 }}>Same repository as this chat's project.</div>}
            {running && (
              <ChoiceRow variant="check" selected={resume} onClick={() => setResume(!resume)} className="opt">
                <span>Continue running there{link.online ? "" : " (only when OpenLeash is running there now)"}</span>
              </ChoiceRow>
            )}
          </>}
          {warning && <div role="note" style={{ color: "#f0c674", fontSize: 12, lineHeight: 1.5 }}>{warning}</div>}
          {err && <div role="alert" style={{ color: "#ff8a8a", fontSize: 12, whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{err}</div>}
        </>}
      </div>
      <div className="mf">
        <span className="desc" style={{ flex: 1, minWidth: 0, fontSize: 11 }}>{phase === "sending" ? (mode === "move" ? "Stopping and sending…" : "Sending…") : ""}</span>
        <Button variant="ghost" onClick={close}>{phase === "done" ? "Close" : "Cancel"}</Button>
        {phase !== "done" && (
          <Button variant="primary" disabled={phase !== "ready"} onClick={() => void send()}>
            {phase === "sending" ? <><Loader size={12} /> Sending…</> : mode === "move" ? "Transfer" : "Copy"}
          </Button>
        )}
      </div>
    </Modal>
  );
}

function Done({ result, mode, machine }: { result: TransferResult; mode: TransferMode; machine: string }) {
  const verb = mode === "move" ? "Transferred" : "Copied";
  const r = result.remote;
  return (
    <div role="status" style={{ display: "flex", flexDirection: "column", gap: 8, lineHeight: 1.5 }}>
      <div style={{ fontWeight: 500 }}>{result.status === "imported" ? `${verb} to ${machine}` : `Sent to ${machine}`}</div>
      {r && <div className="desc">It's in <span className="mono">{r.project}</span>{r.resumed ? " and running there." : ", stopped, waiting for you there."}</div>}
      {r?.warning && <div style={{ color: "#f0c674", fontSize: 12 }}>{r.warning}</div>}
      {result.message && <div className="desc">It's {result.message}.</div>}
      {mode === "move" && (result.status === "imported"
        ? <div className="desc">This chat was archived here.</div>
        : <div className="desc">{machine} hasn't confirmed the import, so this chat was not archived here. It's stopped; archive it yourself once it shows up on {machine}.</div>)}
    </div>
  );
}
