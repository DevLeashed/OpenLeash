// Remote control: typed IPC for Settings → Remote, plus helpers for the
// browser side. The Rust half is src-tauri/src/remote/.
import { invoke } from "@tauri-apps/api/core";
import type { FileRef } from "./api";

export interface RemoteSession {
  id: string; label: string; created: number; last_seen: number; connected: number;
  /** ntfy notifications are on for this device (only the device can turn them on). */
  push: boolean;
  /** Whether the device includes chat details in notifications. */
  push_details: boolean;
  /** Whether every notification button requires the confirm page. */
  push_confirm_all: boolean;
  /** Whether question and completion notifications are enabled. */
  push_questions: boolean;
  push_done: boolean;
}
export interface RemoteClient { session: string; label: string; ip: string; since: number }
export interface RemoteStatus {
  enabled: boolean;
  lan: boolean;
  port: number;
  running: boolean;
  /** Why the server isn't running although it is enabled (e.g. port in use). */
  error: string;
  urls: string[];
  /** One-tap pairing link. A secret: anyone holding it can pair a device. */
  pair_url: string;
  qr_svg: string;
  sessions: RemoteSession[];
  clients: RemoteClient[];
  code: { code: string; expires_in: number } | null;
  /** ntfy server notifications are sent through. */
  ntfy_server: string;
  /** Whether an ntfy token is stored. The token itself is write-only. */
  ntfy_token_set: boolean;
}
export interface DirEntry { name: string; path: string; dir: boolean }
export interface DirListing { path: string; parent: string | null; entries: DirEntry[]; roots: string[]; truncated: boolean }

export const remoteApi = {
  status: () => invoke<RemoteStatus>("remote_status"),
  remoteUpload: async (id: string, filename: string, bytes: ArrayBuffer): Promise<FileRef> => {
    // The task id (or, for the new-task composer with no chat yet, the project
    // path it is scoped to) travels in a header rather than the body because the
    // body is the raw file bytes. Both are validated server-side.
    const response = await fetch("/__openleash/upload", {
      method: "POST", credentials: "same-origin",
      headers: { "Content-Type": "application/octet-stream", "X-OpenLeash-Task": encodeURIComponent(id), "X-OpenLeash-Filename": encodeURIComponent(filename) },
      body: bytes,
    });
    const value = await response.json();
    if (!response.ok) throw new Error(value.error ?? `Upload failed (${response.status})`);
    return value as FileRef;
  },
  configure: (enabled: boolean, lan: boolean, port: number) => invoke<RemoteStatus>("remote_configure", { enabled, lan, port }),
  newCode: () => invoke<RemoteStatus>("remote_new_code"),
  revoke: (id: string) => invoke<RemoteStatus>("remote_revoke", { id }),
  reset: () => invoke<RemoteStatus>("remote_reset"),
  listDir: (path: string | null, dirsOnly: boolean) => invoke<DirListing>("remote_list_dir", { path, dirsOnly }),
  /** `token`: null keeps the stored one, "" removes it, otherwise replaces it. */
  ntfyConfigure: (server: string, token: string | null) => invoke<RemoteStatus>("remote_ntfy_configure", { server, token }),
  /** Only this paired device can disable its notifications; enabling is self-service. */
  pushDisable: (id: string) => invoke<RemoteStatus>("remote_push_disable", { id }),
};

/** Notifications configured by this paired browser; authenticated by its session cookie. */
export interface PushState {
  enabled: boolean;
  topic: string;
  server: string;
  subscribe_url: string;
  app_url: string;
  details: boolean;
  confirm_all: boolean;
  questions: boolean;
  done: boolean;
  base_url: string;
  server_public: boolean;
}
export interface PushOptions { details: boolean; confirm_all: boolean; questions: boolean; done: boolean }
async function pushFetch<T>(path: string, body?: unknown): Promise<T> {
  const response = await fetch(`/__openleash/push/${path}`, body === undefined
    ? { method: path === "state" ? "GET" : "POST", credentials: "same-origin" }
    : { method: "POST", credentials: "same-origin", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  let value: unknown = null;
  try { value = await response.json(); } catch { /* non-JSON error page */ }
  if (!response.ok) {
    const error = value && typeof value === "object" && "error" in value && typeof value.error === "string" ? value.error : `Request failed (${response.status})`;
    throw new Error(error);
  }
  return value as T;
}
export const pushApi = {
  state: () => pushFetch<PushState>("state"),
  enable: (opts: PushOptions) => pushFetch<PushState>("enable", opts),
  settings: (opts: PushOptions) => pushFetch<PushState>("settings", opts),
  disable: () => pushFetch<PushState>("disable"),
  test: () => pushFetch<{ ok: true }>("test"),
};

/** What `shim.js` publishes when the UI runs in a browser on another device. */
export interface RemoteBridge {
  remote?: boolean;
  host?: string;
  logout?: () => Promise<void>;
  pick?: (opts: PickOptions) => Promise<string | string[] | null>;
}
export interface PickOptions { mode: "open" | "save"; directory: boolean; multiple: boolean; title: string; defaultPath: string }

declare global {
  interface Window { __OPENLEASH_REMOTE__?: RemoteBridge }
}

/** True when this UI is a remote-control client rather than the desktop window. */
export function isRemoteClient(): boolean {
  return typeof window !== "undefined" && window.__OPENLEASH_REMOTE__?.remote === true;
}

export interface SshHostView { id: string; name: string; target: string; connected: boolean; local_url: string | null; error: string }
export const sshApi = {
  list: () => invoke<SshHostView[]>("ssh_hosts_list"),
  save: (id: string | null, name: string, target: string) => invoke<SshHostView[]>("ssh_host_save", { id, name, target }),
  remove: (id: string) => invoke<SshHostView[]>("ssh_host_remove", { id }),
  connect: (id: string) => invoke<SshHostView[]>("ssh_connect", { id }),
  disconnect: (id: string) => invoke<SshHostView[]>("ssh_disconnect", { id }),
  /** A fresh end-to-end check (a command, then SFTP) of a saved host. */
  test: (id: string) => invoke<SshHostTest>("ssh_host_test", { id }),
  /** Folders on a saved host. Paths in and out are plain absolute POSIX
   *  paths on that host (null = its home); build the project URI with `sshUri`. */
  listDir: (id: string, path: string | null, dirsOnly: boolean) => invoke<DirListing>("ssh_list_dir", { id, path, dirsOnly }),
};
export interface SshHostTest { home: string; os: string; has_git: boolean; has_rg: boolean; has_bash: boolean }

// ── Send a chat to another machine (src-tauri/src/remote/transfer.rs) ──
export interface TransferProbe {
  host: string;
  os: string;
  home: string;
  projects: { path: string; origin_url: string | null }[];
  /** The project there that is the same repository as this chat's. */
  best: string | null;
}
export interface TransferLink { link: string; host_name: string; online: boolean; reason: string; probe: TransferProbe | null }
export interface CodeState { git: boolean; branch: string; dirty: number; ahead: number | null }
export interface ImportReport { task_id: string; title: string; project: string; mapped_by: "argument" | "origin_url" | "same_path" | "home"; warning: string; from_host: string; moved: boolean; resumed: boolean }
export interface TransferResult { status: "imported" | "pending" | "unconfirmed"; message: string; remote: ImportReport | null }
export type TransferMode = "copy" | "move";

export const transferApi = {
  open: (hostId: string, taskId: string) => invoke<TransferLink>("transfer_open", { hostId, taskId }),
  listDir: (link: string, path: string | null) => invoke<DirListing>("transfer_list_dir", { link, path }),
  close: (link: string) => invoke<void>("transfer_close", { link }),
  check: (taskId: string) => invoke<CodeState>("transfer_check", { taskId }),
  send: (link: string, taskId: string, mode: TransferMode, project: string | null, resume: boolean) =>
    invoke<TransferResult>("transfer_send", { link, taskId, mode, project, resume }),
};
