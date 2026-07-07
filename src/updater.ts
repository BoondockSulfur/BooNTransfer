import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";

export interface UpdateInfo {
  version: string;
  notes?: string;
}

// The live Update handle is not serialisable, so it lives here rather than in
// the store; the store only holds the display info.
let pending: Update | null = null;

/** Checks the release endpoint. Returns update info if newer, else null. */
export async function checkForUpdate(): Promise<UpdateInfo | null> {
  try {
    const update = await check();
    if (update) {
      pending = update;
      return { version: update.version, notes: update.body ?? undefined };
    }
  } catch {
    // Updater unavailable (dev build, no window, offline) — silently ignore.
  }
  return null;
}

/** Downloads and installs the pending update, then relaunches the app. */
export async function installPendingUpdate(): Promise<void> {
  if (!pending) return;
  await pending.downloadAndInstall();
  await relaunch();
}
