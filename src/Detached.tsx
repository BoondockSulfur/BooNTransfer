import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useEffect } from "react";
import {
  api,
  emitFileSaved,
  emitTransferRequest,
  onFileSaved,
  onQueueUpdate,
  onTransferProgress,
} from "./api";
import EditorModal, { type EditorTarget } from "./components/EditorModal";
import FilePane from "./components/FilePane";
import TerminalView from "./components/TerminalView";
import TransferQueue from "./components/TransferQueue";
import { useT } from "./i18n";
import { useStore } from "./state/store";
import { openEditorWindow, openTerminalWindow } from "./windows";

/**
 * Standalone renderer for a detached window. It talks to the same Rust backend
 * (connections/queue are shared app-wide), so a detached pane browses the live
 * connection and a detached queue mirrors every transfer. A "editor" window
 * hosts one file; opening a file from anywhere spawns its own such window.
 */
export default function Detached({
  view,
  params,
}: {
  view: string;
  params: URLSearchParams;
}) {
  const t = useT();
  const theme = useStore((s) => s.theme);
  const setStatus = useStore((s) => s.setStatus);
  const setTransfers = useStore((s) => s.setTransfers);
  const updateProgress = useStore((s) => s.updateProgress);
  const bumpLocal = useStore((s) => s.bumpLocal);
  const bumpRemote = useStore((s) => s.bumpRemote);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  // A detached pane refreshes when a file is saved in any editor window.
  useEffect(() => {
    if (view !== "local" && view !== "remote") return;
    const u = onFileSaved((isLocal) => (isLocal ? bumpLocal() : bumpRemote()));
    return () => void u.then((f) => f());
  }, [view, bumpLocal, bumpRemote]);

  useEffect(() => {
    if (view !== "queue") return;
    void (async () => setTransfers(await api.listTransfers()))();
    const unlisteners = [
      onQueueUpdate(setTransfers),
      onTransferProgress(updateProgress),
    ];
    return () => unlisteners.forEach((p) => void p.then((u) => u()));
  }, [view, setTransfers, updateProgress]);

  if (view === "queue") {
    return (
      <div className="detached detached-queue">
        <TransferQueue />
      </div>
    );
  }

  if (view === "terminal") {
    return (
      <div className="detached detached-terminal">
        <TerminalView connectionId={params.get("connectionId") ?? ""} />
      </div>
    );
  }

  if (view === "editor") {
    const target: EditorTarget = {
      isLocal: params.get("isLocal") === "1",
      connectionId: params.get("connectionId") ?? undefined,
      path: params.get("path") ?? "",
      name: params.get("name") ?? "",
    };
    return (
      <div className="detached detached-editor">
        <EditorModal
          {...target}
          windowed
          onClose={() => void getCurrentWebviewWindow().close()}
          onSaved={(isLocal) => emitFileSaved(isLocal)}
        />
      </div>
    );
  }

  if (view === "local" || view === "remote") {
    return (
      <div className="detached detached-pane">
        <FilePane
          kind={view}
          title={
            view === "local"
              ? t("pane.local")
              : t("pane.remoteWith", { name: params.get("connectionId") ?? "" })
          }
          rootPath={params.get("path") ?? undefined}
          connectionId={params.get("connectionId") ?? undefined}
          onStatus={setStatus}
          onTransferFiles={(source, paths) =>
            emitTransferRequest({
              source,
              connectionId: params.get("connectionId"),
              paths,
            })
          }
          onTerminal={
            view === "remote"
              ? () => {
                  const cid = params.get("connectionId") ?? "";
                  openTerminalWindow(cid, cid);
                }
              : undefined
          }
          onOpenFile={openEditorWindow}
          detached
        />
      </div>
    );
  }

  return <div className="detached">{view}</div>;
}
