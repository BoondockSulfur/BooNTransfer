import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

export type DetachedView = "local" | "remote" | "queue";

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
  const label = `detached-${view}-${seq++}`;
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
