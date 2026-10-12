import { openUrl } from "@tauri-apps/plugin-opener";
import type { AppUpdateStatus } from "../api";
import { appUpdateKey, appUpdateTasksActive, dismissAppUpdate, flash, runAppUpdate, useStore } from "../store";
import { Button } from "./primitives";
import "./AppUpdate.css";

export const RELEASES_URL = "https://github.com/DevLeashed/OpenLeash/releases/latest";
export function appUpdateText(update: AppUpdateStatus) {
  const version = update.version ? ` v${update.version}` : "";
  switch (update.status) {
    case "checking": return "Checking for updates…";
    case "available": return `OpenLeash${version} is available.`;
    case "downloading": return `Downloading OpenLeash${version}… You can keep working.`;
    case "ready": return `OpenLeash${version} is ready to install.`;
    case "current": return "OpenLeash is up to date.";
    case "error": return "Could not update OpenLeash.";
    default: return "Updates are checked automatically. Installation always requires your approval.";
  }
}
export function AppUpdateActions({ check = false }: { check?: boolean }) {
  const update = useStore((s) => s.appUpdate);
  const busy = useStore((s) => s.appUpdateBusy);
  const active = useStore((s) => appUpdateTasksActive(s.tasks));
  const actionError = useStore((s) => s.appUpdateActionError);
  const working = busy || update.status === "checking" || update.status === "downloading";
  return <div className="app-update-actions">
    {check && <Button disabled={working} onClick={() => void runAppUpdate("check")}>Check for updates</Button>}
    {(update.status === "available" || update.status === "error") && update.can_install && <Button disabled={working} onClick={() => void runAppUpdate("download")}>Retry download</Button>}
    {update.status === "ready" && update.can_install && <>
      <span className="desc">Save your work before restarting.</span>
      {actionError && <span className="app-update-error" role="alert">{actionError}</span>}
      <Button variant="primary" disabled={active || working} onClick={() => void runAppUpdate("install")}>{active ? "Finish or stop tasks first" : "Restart to update"}</Button>
    </>}
    {((update.status === "available" || update.status === "error") && !update.can_install) && <Button variant="ghost" onClick={() => void openUrl(RELEASES_URL).catch(() => flash("Couldn't open the browser"))}>Open latest release</Button>}
  </div>;
}
export function AppUpdateBanner() {
  const update = useStore((s) => s.appUpdate);
  const dismissed = useStore((s) => s.appUpdateDismissed);
  const actionError = useStore((s) => s.appUpdateActionError);
  if (!["available", "downloading", "ready", "error"].includes(update.status) || dismissed === appUpdateKey(update)) return null;
  return <section className="app-update-banner" aria-label="Application update">
    <div role="status" aria-live="polite">{appUpdateText(update)}</div>
    {(actionError || update.error) && <div className="app-update-error" role="alert">{actionError || update.error}</div>}
    <AppUpdateActions />
    <Button variant="ghost" onClick={dismissAppUpdate}>Later</Button>
  </section>;
}
