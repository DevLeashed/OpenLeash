import { useEffect, useState } from "react";
import { appVersion } from "../version";

export function AppVersion() {
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
        <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>OpenLeash</div><div className="desc">Updates are currently installed manually. Automatic updates are not enabled.</div></div>
        <span className="mono sel" role="status" aria-label="App version" style={{ fontSize: 12 }}>{version ?? "Loading…"}</span>
      </div>
    </div>
  </>;
}
