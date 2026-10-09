import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";
import { version } from "../package.json";

// The installed binary is authoritative; package metadata is only for Vite's
// browser preview, where no native IPC host exists.
export async function appVersion(): Promise<string> {
  return isTauri() ? getVersion() : version;
}
