import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { EditorTarget } from "./components/EditorModal";

export type DetachedView = "local" | "remote" | "queue" | "editor";

let seq = 0;

/**
 * Opens a panel in its own OS-level window. The detached window loads the same
 * bundle with `?detached=<view>` and renders just that panel against the shared
 * backend state (connections/queue live in the Rust core, one per app).
 */
export function openDetached(
  view: DetachedView,
  params: Record<string, string>,
  opts: { title: string; width?: number; height?: number },
) {
  const query = new URLSearchParams({ detached: view, ...params }).toString();
  // Window labels must be unique app-wide; seq is per-window, so add a random
  // suffix so editors opened from different windows can't collide.
  const label = `detached-${view}-${seq++}-${Math.random().toString(36).slice(2, 8)}`;
  const win = new WebviewWindow(label, {
    url: `index.html?${query}`,
    title: opts.title,
    width: opts.width ?? 820,
    height: opts.height ?? 620,
    minWidth: 380,
    minHeight: 260,
  });
  win.once("tauri://error", (e) => console.error("detach window error", e));
  return win;
}

/**
 * Opens a file in its own editor window. Each call spawns an independent window,
 * so several files can be edited side by side.
 */
export function openEditorWindow(target: EditorTarget) {
  const params: Record<string, string> = {
    isLocal: target.isLocal ? "1" : "0",
    path: target.path,
    name: target.name,
  };
  if (target.connectionId) params.connectionId = target.connectionId;
  return openDetached("editor", params, {
    title: `✎ ${target.name}`,
    width: 900,
    height: 680,
  });
}
