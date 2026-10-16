import { useEffect } from "react";
import { Server } from "lucide-react";
import { sshApi } from "../remote";
import { isSsh, projectLabel } from "../sshPath";
import { get, set, useStore } from "../store";

let loading: Promise<void> | null = null;

/** Fetch the saved SSH hosts into the store (once at a time). */
export function loadSshHosts(): Promise<void> {
  loading ??= sshApi.list()
    .then((sshHosts) => set({ sshHosts }))
    .catch(() => { if (!get().sshHosts) set({ sshHosts: [] }); })
    .finally(() => { loading = null; });
  return loading;
}

/** The saved hosts, loaded on first use when any of `paths` is an SSH project. */
export function useSshHosts(paths: readonly string[] | undefined = []) {
  const hosts = useStore((s) => s.sshHosts);
  const need = hosts === null && !!paths?.some(isSsh);
  useEffect(() => { if (need) void loadSshHosts(); }, [need]);
  return hosts;
}

/** `projectLabel` with the store's hosts, for string-only places (menus, palette). */
export function useProjectLabel(paths: readonly string[] | undefined = []): (path: string) => string {
  const hosts = useSshHosts(paths);
  return (p: string) => projectLabel(p, hosts);
}

/**
 * A project's display name: the folder, and for a remote-SSH project a small
 * server icon and `<folder> · <host name>`.
 */
export function ProjectName({ path }: { path: string }) {
  const hosts = useSshHosts([path]);
  if (!isSsh(path)) return <>{projectLabel(path, hosts)}</>;
  return (
    <span className="projname-ssh" style={{ display: "inline-flex", alignItems: "center", gap: 4, minWidth: 0, maxWidth: "100%" }}>
      <Server size={11} strokeWidth={1.8} aria-label="SSH" style={{ flex: "none", opacity: 0.75 }} />
      <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{projectLabel(path, hosts)}</span>
    </span>
  );
}
