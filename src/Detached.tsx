import { useEffect, useState } from "react";
import { api, onQueueUpdate, onTransferProgress } from "./api";
import EditorModal, { type EditorTarget } from "./components/EditorModal";
import FilePane from "./components/FilePane";
import TransferQueue from "./components/TransferQueue";
import { useStore } from "./state/store";

/**
 * Standalone renderer for a detached window. It talks to the same Rust backend
 * (connections/queue are shared app-wide), so a detached pane browses the live
 * connection and a detached queue mirrors every transfer.
 */
export default function Detached({
  view,
  params,
}: {
  view: string;
  params: URLSearchParams;
}) {
  const theme = useStore((s) => s.theme);
  const setStatus = useStore((s) => s.setStatus);
  const setTransfers = useStore((s) => s.setTransfers);
  const updateProgress = useStore((s) => s.updateProgress);
  const [editor, setEditor] = useState<EditorTarget | null>(null);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

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

  if (view === "local" || view === "remote") {
    return (
      <div className="detached detached-pane">
        <FilePane
          kind={view}
          title={
            view === "local"
              ? "Lokal"
              : `Remote — ${params.get("connectionId") ?? ""}`
          }
          rootPath={params.get("path") ?? undefined}
          connectionId={params.get("connectionId") ?? undefined}
          onStatus={setStatus}
          onTransferFiles={() => {}}
          onOpenFile={setEditor}
          detached
        />
        {editor && (
          <EditorModal
            {...editor}
            onClose={() => setEditor(null)}
            onSaved={() => {}}
          />
        )}
      </div>
    );
  }

  return <div className="detached">Unbekannte Ansicht: {view}</div>;
}
