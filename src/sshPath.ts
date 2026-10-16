// Remote-SSH project paths: `ssh://<host-id>/<absolute posix path>`, where
// the host id is a saved SSH host (Settings → Remote). Mirrors
// src-tauri/src/agent/host.rs (`ssh_uri` / `parse_ssh`), so a URI built here
// is spelled exactly the way the backend spells it.
import { baseName } from "./api";

const SCHEME = "ssh://";

const unreserved = (b: number) =>
  (b >= 0x30 && b <= 0x39) || (b >= 0x41 && b <= 0x5a) || (b >= 0x61 && b <= 0x7a) || b === 0x2d || b === 0x2e || b === 0x5f || b === 0x7e;

function pctEncode(s: string): string {
  let out = "";
  for (const b of new TextEncoder().encode(s)) out += unreserved(b) ? String.fromCharCode(b) : "%" + b.toString(16).toUpperCase().padStart(2, "0");
  return out;
}

function pctDecode(s: string): string | null {
  const bytes: number[] = [];
  for (let i = 0; i < s.length; ) {
    const c = s.charCodeAt(i);
    if (s[i] === "%") {
      const hex = s.slice(i + 1, i + 3);
      if (!/^[0-9a-fA-F]{2}$/.test(hex)) return null;
      bytes.push(parseInt(hex, 16));
      i += 3;
    } else if (unreserved(c)) { bytes.push(c); i += 1; } else return null;
  }
  try { return new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(bytes)); } catch { return null; }
}

/** Lexical POSIX normalization; never climbs above `/`. */
export function normalizePosix(p: string): string {
  const out: string[] = [];
  for (const c of p.split("/")) {
    if (c === "" || c === ".") continue;
    if (c === "..") out.pop();
    else out.push(c);
  }
  return "/" + out.join("/");
}

export const isSsh = (p: string) => p.slice(0, SCHEME.length).toLowerCase() === SCHEME;

/** `ssh://<host>/<path>` for a saved host id and an absolute remote path. */
export const sshUri = (host: string, path: string) => SCHEME + pctEncode(host) + normalizePosix(path);

/** The host id and path of a project URI, or null if it isn't one. */
export function parseSsh(p: string): { host: string; path: string } | null {
  if (!isSsh(p) || /[\0\r\n]/.test(p)) return null;
  const rest = p.slice(SCHEME.length);
  const i = rest.indexOf("/");
  const host = pctDecode(i < 0 ? rest : rest.slice(0, i));
  if (!host || /\p{Cc}/u.test(host)) return null;
  return { host, path: normalizePosix(i < 0 ? "/" : rest.slice(i)) };
}

export interface HostName { id: string; name: string }

/** The host an SSH project lives on, by its saved name; null for a local path. */
export function projectHost(path: string, hosts: readonly HostName[] | null | undefined): string | null {
  const p = parseSsh(path);
  if (!p) return null;
  return hosts?.find((h) => h.id === p.host)?.name ?? "unknown host";
}

/**
 * How a project is named in lists: the folder name, and for an SSH project
 * `<folder> · <host name>` (the saved name, never the raw id).
 */
export function projectLabel(path: string, hosts: readonly HostName[] | null | undefined): string {
  const p = parseSsh(path);
  if (!p) return baseName(path);
  const folder = p.path === "/" ? "/" : p.path.slice(p.path.lastIndexOf("/") + 1);
  return `${folder} · ${projectHost(path, hosts)}`;
}
