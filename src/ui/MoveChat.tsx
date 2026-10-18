import { useEffect } from "react";
import { FolderInput, MonitorUp, SquareTerminal } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, baseName, type WslDistro } from "../api";
import { flash, set, useStore } from "../store";
import { workspacesApi } from "../workspaces";
import { changeChatFolder } from "./Chrome";
import { loadWsl } from "./OpenProject";
import { openTransfer } from "./SendToMachine";
import { IconButton, MenuRow, Modal } from "./primitives";
import { I } from "./icons";

/** One entry point for every "put this chat somewhere else" action. */
export const openMove = (id: string) => set({ move: id, menu: null, pal: false });

/** Point the chat at a folder inside a WSL distro. The `\wsl.localhost\…` path
 *  is an ordinary folder to the backend, which already routes commands into it. */
async function moveToWsl(id: string, d: WslDistro) {
  const slow = window.setTimeout(() => flash(`Starting ${d.name}…`), 600);
  const start = await api.wslStartDir(d.name).catch(() => d.root).finally(() => window.clearTimeout(slow));
  try {
    const dir = await open({ directory: true, multiple: false, title: `Move this chat into ${d.name}`, defaultPath: start });
    if (typeof dir !== "string") return;
    await workspacesApi.changeFolder(id, dir);
    flash(`Chat moved to ${baseName(dir)} in ${d.name}`);
  } catch (e) {
    flash(String(e));
  }
}

/** Mounted once beside the other overlays; shows while `move` names a chat. */
export function MoveChat() {
  const id = useStore((s) => s.move);
  const task = useStore((s) => (id ? s.tasks[id] : null));
  const wsl = useStore((s) => s.wsl);
  useEffect(() => { if (id) void loadWsl(); }, [id]);
  if (!id || !task) return null;
  const close = () => set({ move: null });
  const go = (f: () => void) => () => { close(); f(); };
  const ico = (C: typeof FolderInput) => <C size={13} strokeWidth={1.7} aria-hidden="true" />;
  const rows: { key: string; icon: React.ReactNode; label: string; hint: string; run: () => void }[] = [
    { key: "folder", icon: ico(FolderInput), label: "Another folder", hint: "On this machine; files stay where they are", run: go(() => void changeChatFolder(id)) },
    ...wsl.map((d) => ({ key: "wsl:" + d.name, icon: ico(SquareTerminal), label: `WSL · ${d.name}`, hint: "A folder inside this distro; commands and git run there", run: go(() => void moveToWsl(id, d)) })),
    { key: "machine", icon: ico(MonitorUp), label: "Another machine", hint: "Copy or transfer to OpenLeash on a computer over SSH", run: () => openTransfer(id) },
  ];
  return (
    <Modal onClose={close} ariaLabel="Move chat" style={{ width: "min(440px, calc(100vw - 24px))" }}>
      <div className="mh">
        <span style={{ flex: 1, minWidth: 0 }}>Move chat to…</span>
        <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
      </div>
      <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 2 }}>
        {rows.map((r) => (
          <MenuRow key={r.key} onClick={r.run}>
            <span className="cico">{r.icon}</span>
            <span style={{ flex: 1, minWidth: 0 }}>
              <div>{r.label}</div>
              <div className="desc" style={{ fontSize: 11 }}>{r.hint}</div>
            </span>
          </MenuRow>
        ))}
      </div>
    </Modal>
  );
}
