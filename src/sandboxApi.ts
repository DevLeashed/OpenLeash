// Typed IPC for sandboxes (src-tauri/src/agent/sandbox.rs, commands in lib.rs).
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";

export interface SandboxSnapshot { id: string; name: string; created: string; tag: string }
export type SandboxState = "running" | "stopped" | "absent";
export type SandboxDriver = { kind: "idle" } | { kind: "user" } | { kind: "agent"; task: string; summary: string };

/** One sandbox as the backend reports it. `url` is the live view (127.0.0.1 only). */
export interface Sandbox {
  id: string; name: string; created: string; width: number; height: number;
  memory: string; cpus: number; computer: boolean; shell: boolean; notes: string;
  fromSnapshot: boolean; snapshots: SandboxSnapshot[];
  state: SandboxState; port: number | null; url: string | null;
  driver: SandboxDriver; files: number;
}
export interface SandboxList { sandboxes: Sandbox[]; dockerError: string | null; imageReady: boolean; image: string }
export interface SandboxFile { path: string; size: number; dir: boolean }
export interface SandboxPatch { name?: string; notes?: string; computer?: boolean; shell?: boolean; memory?: string; cpus?: number }
export interface NewSandbox { name: string; width?: number; height?: number; memory?: string; cpus?: number }

export const sandboxApi = {
  list: () => invoke<SandboxList>("sandbox_list"),
  prepare: () => invoke<void>("sandbox_prepare"),
  setImage: (image: string) => invoke<void>("sandbox_image_set", { image }),
  create: (req: NewSandbox) => invoke<Sandbox>("sandbox_create", { ...req }),
  update: (id: string, patch: SandboxPatch) => invoke<Sandbox>("sandbox_update", { id, patch }),
  start: (id: string) => invoke<Sandbox>("sandbox_start", { id }),
  stop: (id: string) => invoke<Sandbox>("sandbox_stop", { id }),
  reset: (id: string, width?: number, height?: number) => invoke<Sandbox>("sandbox_reset", { id, width, height }),
  remove: (id: string) => invoke<void>("sandbox_delete", { id }),
  clone: (id: string, name: string) => invoke<Sandbox>("sandbox_clone", { id, name }),
  snapshot: (id: string, name: string) => invoke<Sandbox>("sandbox_snapshot", { id, name }),
  restore: (id: string, snapshot: string) => invoke<Sandbox>("sandbox_restore", { id, snapshot }),
  deleteSnapshot: (id: string, snapshot: string) => invoke<Sandbox>("sandbox_snapshot_delete", { id, snapshot }),
  control: (id: string, user: boolean) => invoke<void>("sandbox_control", { id, user }),
  thumb: (id: string) => invoke<string | null>("sandbox_thumb", { id }),
  files: (id: string) => invoke<SandboxFile[]>("sandbox_files", { id }),
  importFiles: (id: string, paths: string[]) => invoke<number>("sandbox_files_import", { id, paths }),
  exportFile: (id: string, rel: string, dest: string) => invoke<void>("sandbox_files_export", { id, rel, dest }),
  deleteFile: (id: string, rel: string) => invoke<void>("sandbox_files_delete", { id, rel }),
  attach: (taskId: string, sandbox: string | null) => invoke<void>("task_set_sandbox", { taskId, sandbox }),
  publish: (taskId: string, id: string, title: string, file?: string) => invoke("sandbox_publish", { taskId, id, title, file }),
};

// The last list and screen pictures seen. Rejoining a chat renders from these at once and
// refreshes behind them, instead of showing an empty view while docker answers.
let lastList: SandboxList | null = null;
const lastThumbs = new Map<string, string>();

/** The sandbox list, kept fresh by the backend's `ol://sandbox` events. */
export function useSandboxes() {
  const [data, setData] = useState<SandboxList | null>(lastList);
  const [error, setError] = useState("");
  const alive = useRef(true);
  const reload = () => sandboxApi.list().then((d) => { lastList = d; if (alive.current) { setData(d); setError(""); } }).catch((e) => { if (alive.current) setError(String(e)); });
  useEffect(() => {
    alive.current = true;
    void reload();
    let un: (() => void) | undefined;
    let dead = false;
    void Promise.resolve(listen("ol://sandbox", () => { void reload(); })).then((u) => { if (typeof u === "function") { if (dead) u(); else un = u; } });
    // Containers also change outside the app (docker CLI, Docker Desktop), so
    // the list catches up on its own instead of waiting for an event.
    const poll = setInterval(() => { void reload(); }, 5000);
    return () => { alive.current = false; dead = true; un?.(); clearInterval(poll); };
  }, []);
  return { data, error, reload };
}

/** A picture of a sandbox's screen, refreshed while it runs. */
export function useSandboxThumb(id: string, running: boolean, everyMs = 5000, bump = 0) {
  const [src, setSrc] = useState<string | null>(lastThumbs.get(id) ?? null);
  useEffect(() => {
    if (!running) { setSrc(null); return; }
    let dead = false;
    const tick = () => sandboxApi.thumb(id).then((s) => { if (s) lastThumbs.set(id, s); if (!dead && s) setSrc(s); }).catch(() => undefined);
    void tick();
    const t = setInterval(tick, everyMs);
    return () => { dead = true; clearInterval(t); };
  }, [id, running, everyMs, bump]);
  return src;
}

export function fmtBytes(n: number) {
  if (n >= 1048576) return `${(n / 1048576).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}
