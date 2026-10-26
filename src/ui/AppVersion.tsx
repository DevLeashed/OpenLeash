import { useEffect, useState } from "react";
import { appVersion } from "../version";
import { saveSettings, useStore } from "../store";
import { AppUpdateActions, appUpdateText } from "./AppUpdate";
import { Switch } from "./primitives";

export function AppVersion() {
  const update = useStore((s) => s.appUpdate);
  const actionError = useStore((s) => s.appUpdateActionError);
  const settings = useStore((s) => s.settings);
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    void appVersion().then(
      (value) => { if (active) setVersion(`v${value}`); },
      () => { if (active) setVersion("Unavailable"); },
    );
    return () => { active = false; };
  }, []);
  return <>
    <div className="settings-section-label">About</div>
    <div className="sgroup">
      <div className="srowx">
        <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>OpenLeash</div><div className="desc">Releases come from GitHub, and updating takes one click: <b>Update now</b> finds, downloads and applies a newer release, restarting OpenLeash once nothing is running. Both background switches are off by default: nothing is checked, downloaded or installed until you turn one on.</div></div>
        <span className="mono sel" role="status" aria-label="App version" style={{ fontSize: 12 }}>{version ?? "Loading…"}</span>
      </div>
      {/* Both switches are off by default, and off is the point: a background
          check tells GitHub this machine's address on a schedule nobody asked
          for, and a background install closes the app. Neither happens until
          the user turns it on. */}
      <div className="srowx" style={{ alignItems: "flex-start" }}>
        <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Check for updates automatically</div><div className="desc">Look for a newer OpenLeash release every few hours, without you asking. This sends a request to GitHub — your IP address, and nothing about your chats.</div></div>
        <Switch label="Check for updates automatically" checked={settings?.auto_check_updates === true} onChange={(checked) => void saveSettings({ auto_check_updates: checked })} />
      </div>
      <div className="srowx" style={{ alignItems: "flex-start" }}>
        <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Update automatically</div><div className="desc">Download and install a newer release as soon as nothing is running — no chat mid-turn, no subagent, no command. OpenLeash restarts itself to finish. Off by default: installing closes the app, and it never interrupts work that is still going.</div></div>
        <Switch label="Update automatically" checked={settings?.auto_update === true} onChange={(checked) => void saveSettings({ auto_update: checked })} />
      </div>
      <div className="srowx app-update-about">
        <div role="status" aria-label="Update status">{appUpdateText(update)}</div>
        {(update.error || actionError) && <div className="desc app-update-error" role="alert">{actionError || update.error}</div>}
        {update.status !== "current" && !update.can_install && <div className="desc">In-app installation is unavailable for this build. Download the latest release manually.</div>}
        <AppUpdateActions check />
      </div>
    </div>
  </>;
}
