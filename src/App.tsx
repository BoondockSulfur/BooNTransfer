import { useEffect, useRef, useState } from "react";
import { api, onFileSaved, onQueueUpdate, onTransferProgress } from "./api";
import { useT } from "./i18n";
import { checkForUpdate, installPendingUpdate } from "./updater";
import ConflictDialog from "./components/ConflictDialog";
import ConnectionManager from "./components/ConnectionManager";
import FilePane, {
  type PaneHandle,
  type PaneKind,
} from "./components/FilePane";
import HostKeyDialog from "./components/HostKeyDialog";
import StatusBar from "./components/StatusBar";
import SyncDialog from "./components/SyncDialog";
import TransferQueue from "./components/TransferQueue";
import { clamp, useSplitter } from "./hooks";
import { useStore } from "./state/store";
import { isAppError } from "./types";
import type {
  ConflictPolicy,
  PlannedFile,
  ResolvedFile,
  SyncDirection,
  SyncItem,
} from "./types";
import { openDetached, openEditorWindow } from "./windows";

type TransferDir = "upload" | "download";

interface ConflictState {
  direction: TransferDir;
  dest: string;
  planned: PlannedFile[];
}

interface HostKeyReq {
  sessionId: string;
  host: string;
  fingerprint: string;
  keyType: string;
  changed: boolean;
}

/** One connected remote session shown as a tab. */
interface Tab {
  id: string; // connectionId
  title: string;
  home: string;
  remoteRoot: string; // last-visited remote dir (restored on tab switch)
}

export default function App() {
  const tr = useT();
  const lang = useStore((s) => s.lang);
  const setLang = useStore((s) => s.setLang);
  const update = useStore((s) => s.update);
  const setUpdate = useStore((s) => s.setUpdate);
  const updating = useStore((s) => s.updating);
  const setUpdating = useStore((s) => s.setUpdating);
  const theme = useStore((s) => s.theme);
  const toggleTheme = useStore((s) => s.toggleTheme);
  const sessions = useStore((s) => s.sessions);
  const setSessions = useStore((s) => s.setSessions);
  const connection = useStore((s) => s.connection);
  const setConnection = useStore((s) => s.setConnection);
  const connecting = useStore((s) => s.connecting);
  const setConnecting = useStore((s) => s.setConnecting);
  const setStatus = useStore((s) => s.setStatus);
  const setTransfers = useStore((s) => s.setTransfers);
  const updateProgress = useStore((s) => s.updateProgress);
  const bumpLocal = useStore((s) => s.bumpLocal);
  const bumpRemote = useStore((s) => s.bumpRemote);

  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const activeTab = tabs.find((t) => t.id === activeId) ?? null;

  const [localHome, setLocalHome] = useState<string>();
  const [showConn, setShowConn] = useState(false);
  const [hostKey, setHostKey] = useState<HostKeyReq | null>(null);
  const [conflict, setConflict] = useState<ConflictState | null>(null);
  const [sync, setSync] = useState<{
    localDir: string;
    remoteDir: string;
  } | null>(null);

  const localRef = useRef<PaneHandle>(null);
  const remoteRef = useRef<PaneHandle>(null);
  const completedIds = useRef<Set<string>>(new Set());

  // --- resizable layout ---
  const panesRef = useRef<HTMLElement>(null);
  const [leftFrac, setLeftFrac] = useState(0.5);
  const leftFracRef = useRef(leftFrac);
  leftFracRef.current = leftFrac;
  const [queueHeight, setQueueHeight] = useState(210);
  const queueHeightRef = useRef(queueHeight);
  queueHeightRef.current = queueHeight;

  const startPaneDrag = useSplitter(
    leftFracRef,
    setLeftFrac,
    (start, dx) => {
      const w = panesRef.current?.clientWidth ?? window.innerWidth;
      return clamp(start + dx / w, 0.15, 0.85);
    },
    "col-resize",
  );
  const startQueueDrag = useSplitter(
    queueHeightRef,
    setQueueHeight,
    (start, _dx, dy) => clamp(start - dy, 90, 560),
    "row-resize",
  );

  // theme + language -> DOM
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);
  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  // Refresh the matching pane when a file is saved in any editor window.
  useEffect(() => {
    const u = onFileSaved((isLocal) => (isLocal ? bumpLocal() : bumpRemote()));
    return () => void u.then((f) => f());
  }, [bumpLocal, bumpRemote]);

  // Check for an app update on startup (no-op in dev / browser).
  useEffect(() => {
    void checkForUpdate().then((info) => {
      if (info) setUpdate(info);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const runUpdate = async () => {
    setUpdating(true);
    try {
      await installPendingUpdate(); // relaunches on success
    } catch (e) {
      setUpdating(false);
      setStatus(tr("update.failed", { error: String(e) }), "error");
    }
  };

  // initial data + event wiring
  useEffect(() => {
    void (async () => {
      setLocalHome(await api.localHome());
      setSessions(await api.listSessions());
      setTransfers(await api.listTransfers());
    })();

    const unlisteners: Promise<() => void>[] = [
      onQueueUpdate((items) => {
        setTransfers(items);
        let newlyDone = false;
        for (const t of items) {
          if (t.status === "completed" && !completedIds.current.has(t.id)) {
            completedIds.current.add(t.id);
            newlyDone = true;
          }
        }
        if (newlyDone) {
          bumpLocal();
          bumpRemote();
        }
      }),
      onTransferProgress((id, transferred) => updateProgress(id, transferred)),
    ];
    return () => {
      unlisteners.forEach((p) => void p.then((u) => u()));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Keep the store's "connection" in sync with the active tab (StatusBar etc.).
  useEffect(() => {
    setConnection(
      activeTab
        ? {
            connectionId: activeTab.id,
            homeDir: activeTab.home,
            remoteDir: activeTab.remoteRoot,
            localDir: null,
          }
        : null,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeId, tabs.length]);

  const doConnect = async (sessionId: string, trust = false) => {
    setConnecting(true);
    setStatus(tr("status.connecting"));
    try {
      const result = await api.connect(sessionId, trust);
      const session = sessions.find((s) => s.id === sessionId);
      const title = session
        ? session.name
        : `${result.homeDir}`.split("/").pop() || "Sitzung";
      const tab: Tab = {
        id: result.connectionId,
        title,
        home: result.homeDir,
        remoteRoot: result.remoteDir || result.homeDir,
      };
      setTabs((ts) => [...ts, tab]);
      setActiveId(result.connectionId);
      setHostKey(null);
      setShowConn(false);
      setStatus(tr("status.connected", { name: title }), "success");
      setSessions(await api.listSessions());
    } catch (e) {
      if (isAppError(e) && e.kind === "host_key") {
        const s = sessions.find((x) => x.id === sessionId);
        setHostKey({
          sessionId,
          host: s?.host ?? "",
          fingerprint: e.fingerprint ?? "",
          keyType: e.keyType ?? "",
          changed: !!e.changed,
        });
      } else {
        setStatus(isAppError(e) ? e.message : String(e), "error");
      }
    } finally {
      setConnecting(false);
    }
  };

  const switchTab = (id: string) => {
    if (id === activeId) return;
    // Remember the current remote directory of the tab we're leaving.
    const dir = remoteRef.current?.currentDir();
    if (dir && activeId) {
      setTabs((ts) =>
        ts.map((t) => (t.id === activeId ? { ...t, remoteRoot: dir } : t)),
      );
    }
    setActiveId(id);
  };

  const closeTab = async (id: string) => {
    await api.disconnect(id);
    const remaining = tabs.filter((t) => t.id !== id);
    setTabs(remaining);
    if (activeId === id) {
      setActiveId(remaining.length ? remaining[remaining.length - 1].id : null);
    }
    setStatus(tr("status.disconnected"));
  };

  const enqueueResolved = async (
    direction: TransferDir,
    dest: string,
    planned: PlannedFile[],
    conflictPolicy: ConflictPolicy,
  ) => {
    if (!connection) return;
    // Files that don't exist yet always "overwrite" (= create); existing files
    // use the policy chosen in the dialog. The backend drops "skip" entries.
    const resolved: ResolvedFile[] = planned.map((f) => ({
      ...f,
      policy: f.exists ? conflictPolicy : "overwrite",
    }));
    const n = await api.enqueueTransfers(
      connection.connectionId,
      direction,
      dest,
      resolved,
    );
    setStatus(
      tr(direction === "upload" ? "status.queuedUp" : "status.queuedDown", {
        n,
        dest,
      }),
    );
  };

  const handleTransfer = async (
    source: PaneKind,
    paths: string[],
    destDir?: string,
  ) => {
    if (!connection) {
      setStatus(tr("status.noConnection"), "error");
      return;
    }
    if (paths.length === 0) return;
    const direction: TransferDir = source === "local" ? "upload" : "download";
    const dest =
      destDir ??
      (source === "local"
        ? remoteRef.current?.currentDir()
        : localRef.current?.currentDir());
    if (!dest) return;
    try {
      const planned = await api.planTransfers(
        connection.connectionId,
        direction,
        paths,
        dest,
      );
      if (planned.length === 0) {
        setStatus(tr("status.noFiles"));
        return;
      }
      if (planned.some((f) => f.exists)) {
        setConflict({ direction, dest, planned });
      } else {
        await enqueueResolved(direction, dest, planned, "overwrite");
      }
    } catch (e) {
      setStatus(isAppError(e) ? e.message : String(e), "error");
    }
  };

  const remoteRoot = activeTab?.remoteRoot;

  const detachLocal = () =>
    openDetached(
      "local",
      { path: localRef.current?.currentDir() ?? localHome ?? "/" },
      { title: `${tr("pane.local")} — BooN Transfer` },
    );
  const detachRemote = () => {
    if (!connection) return;
    openDetached(
      "remote",
      {
        connectionId: connection.connectionId,
        path: remoteRef.current?.currentDir() ?? remoteRoot ?? "/",
      },
      { title: tr("pane.remoteWith", { name: connection.connectionId }) },
    );
  };
  const detachQueue = () =>
    openDetached(
      "queue",
      {},
      {
        title: `${tr("queue.title")} — BooN Transfer`,
        width: 760,
        height: 420,
      },
    );

  const openSync = () => {
    if (!connection) return;
    const localDir = localRef.current?.currentDir();
    const remoteDir = remoteRef.current?.currentDir();
    if (localDir && remoteDir) setSync({ localDir, remoteDir });
  };

  const analyzeSync = (direction: SyncDirection): Promise<SyncItem[]> => {
    if (!connection || !sync) return Promise.resolve([]);
    return api.planSync(
      connection.connectionId,
      sync.localDir,
      sync.remoteDir,
      direction,
    );
  };

  const runSync = async (items: SyncItem[]) => {
    if (!connection || !sync) return;
    const toResolved = (i: SyncItem): ResolvedFile => ({
      local: i.local,
      remote: i.remote,
      name: i.name,
      size: i.size,
      exists: true,
      destSize: 0,
      policy: "overwrite",
    });
    const uploads = items
      .filter((i) => i.direction === "upload")
      .map(toResolved);
    const downloads = items
      .filter((i) => i.direction === "download")
      .map(toResolved);
    let n = 0;
    if (uploads.length)
      n += await api.enqueueTransfers(
        connection.connectionId,
        "upload",
        sync.remoteDir,
        uploads,
      );
    if (downloads.length)
      n += await api.enqueueTransfers(
        connection.connectionId,
        "download",
        sync.localDir,
        downloads,
      );
    setStatus(tr("status.synced", { n }));
  };

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="logo">⇅</span>
          <span className="brand-name">BooN&nbsp;Transfer</span>
        </div>
        <div className="topbar-actions">
          <button className="primary" onClick={() => setShowConn(true)}>
            {connecting ? tr("topbar.connecting") : tr("topbar.add")}
          </button>
          <span className="spacer" />
          <button
            className="ghost icon-btn"
            title={tr("topbar.lang")}
            onClick={() => setLang(lang === "de" ? "en" : "de")}
          >
            {lang === "de" ? "EN" : "DE"}
          </button>
          <button
            className="ghost icon-btn"
            title={tr("topbar.theme")}
            onClick={toggleTheme}
          >
            {theme === "dark" ? "☀" : "☾"}
          </button>
        </div>
      </header>

      {update && (
        <div className="update-bar" role="status">
          <span className="update-msg">
            ⬆ {tr("update.available", { version: update.version })}
          </span>
          <span className="spacer" />
          <button
            className="ghost mini"
            disabled={updating}
            onClick={() => setUpdate(null)}
          >
            {tr("update.dismiss")}
          </button>
          <button
            className="primary mini"
            disabled={updating}
            onClick={() => void runUpdate()}
          >
            {updating ? tr("update.installing") : tr("update.install")}
          </button>
        </div>
      )}

      {tabs.length > 0 && (
        <nav className="tabbar">
          {tabs.map((t) => (
            <div
              key={t.id}
              className={`tab ${t.id === activeId ? "active" : ""}`}
              onClick={() => switchTab(t.id)}
              onMouseDown={(e) => {
                if (e.button === 1) {
                  e.preventDefault();
                  void closeTab(t.id);
                }
              }}
              title={t.title}
            >
              <span className="tab-dot" />
              <span className="tab-title">{t.title}</span>
              <button
                className="tab-close"
                title={tr("tab.close")}
                onClick={(e) => {
                  e.stopPropagation();
                  void closeTab(t.id);
                }}
              >
                ✕
              </button>
            </div>
          ))}
          <button
            className="tab-add"
            title={tr("tab.new")}
            onClick={() => setShowConn(true)}
          >
            ＋
          </button>
        </nav>
      )}

      <main
        className="panes"
        ref={panesRef}
        style={{ gridTemplateColumns: `${leftFrac}fr 46px ${1 - leftFrac}fr` }}
      >
        <FilePane
          ref={localRef}
          kind="local"
          title={tr("pane.local")}
          rootPath={localHome}
          onStatus={setStatus}
          onTransferFiles={handleTransfer}
          onDetach={detachLocal}
          onOpenFile={openEditorWindow}
        />

        <div
          className="pane-divider"
          onMouseDown={startPaneDrag}
          title={tr("divider.dragCols")}
        >
          <button
            onMouseDown={(e) => e.stopPropagation()}
            title={tr("divider.upload")}
            disabled={!connection}
            onClick={() =>
              handleTransfer("local", localRef.current?.selectedPaths() ?? [])
            }
          >
            ▶
          </button>
          <button
            onMouseDown={(e) => e.stopPropagation()}
            title={tr("divider.download")}
            disabled={!connection}
            onClick={() =>
              handleTransfer("remote", remoteRef.current?.selectedPaths() ?? [])
            }
          >
            ◀
          </button>
          <button
            onMouseDown={(e) => e.stopPropagation()}
            title={tr("divider.sync")}
            disabled={!connection}
            onClick={openSync}
          >
            ⇄
          </button>
        </div>

        <FilePane
          key={activeId ?? "none"}
          ref={remoteRef}
          kind="remote"
          title={
            activeTab
              ? tr("pane.remoteWith", { name: activeTab.title })
              : tr("pane.remote")
          }
          rootPath={remoteRoot}
          connectionId={activeId ?? undefined}
          onStatus={setStatus}
          onTransferFiles={handleTransfer}
          onDetach={activeTab ? detachRemote : undefined}
          onOpenFile={openEditorWindow}
        />
      </main>

      <div className="queue-wrap" style={{ height: queueHeight }}>
        <div
          className="row-resize"
          onMouseDown={startQueueDrag}
          title={tr("divider.dragRows")}
        />
        <TransferQueue onDetach={detachQueue} />
      </div>
      <StatusBar />

      {showConn && (
        <ConnectionManager
          onClose={() => setShowConn(false)}
          onConnect={(id) => void doConnect(id)}
        />
      )}
      {sync && (
        <SyncDialog
          localDir={sync.localDir}
          remoteDir={sync.remoteDir}
          onAnalyze={analyzeSync}
          onRun={runSync}
          onClose={() => setSync(null)}
        />
      )}
      {conflict && (
        <ConflictDialog
          direction={conflict.direction}
          files={conflict.planned}
          onResolve={(policy) => {
            void enqueueResolved(
              conflict.direction,
              conflict.dest,
              conflict.planned,
              policy,
            );
            setConflict(null);
          }}
          onCancel={() => setConflict(null)}
        />
      )}
      {hostKey && (
        <HostKeyDialog
          host={hostKey.host}
          fingerprint={hostKey.fingerprint}
          keyType={hostKey.keyType}
          changed={hostKey.changed}
          onAccept={() => void doConnect(hostKey.sessionId, true)}
          onReject={() => setHostKey(null)}
        />
      )}
    </div>
  );
}
