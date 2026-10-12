import { useEffect, useState } from "react";
import { appVersion } from "../version";
import { useStore } from "../store";
import { AppUpdateActions, appUpdateText } from "./AppUpdate";

export function AppVersion() {
  const update = useStore((s) => s.appUpdate);
  const actionError = useStore((s) => s.appUpdateActionError);
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
        <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>OpenLeash</div><div className="desc">Updates download automatically when supported. Restart only when you choose.</div></div>
        <span className="mono sel" role="status" aria-label="App version" style={{ fontSize: 12 }}>{version ?? "Loading…"}</span>
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
