import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import { api } from "../api";
import { useStore } from "../state/store";
import type { FileEntry } from "../types";
import { isAppError } from "../types";
import {
  breadcrumbs,
  formatDate,
  formatPerms,
  formatSize,
  isEditableFile,
  posixJoin,
  posixParent,
  sortEntries,
  type SortKey,
} from "../util";

export type PaneKind = "local" | "remote";

export interface PaneHandle {
  kind: PaneKind;
  currentDir: () => string;
  selectedPaths: () => string[];
  refresh: () => void;
}

interface Props {
  kind: PaneKind;
  title: string;
  rootPath?: string; // initial directory once available
  connectionId?: string;
  onStatus: (text: string, kind?: "info" | "error" | "success") => void;
  // destDir is set on drag-and-drop (the drop target); omitted for menu/keyboard
  // transfers, where App uses the opposite pane's current directory.
  onTransferFiles: (source: PaneKind, paths: string[], destDir?: string) => void;
  /** Opens this pane in its own OS window (hidden when already detached). */
  onDetach?: () => void;
  /** True when rendered inside a detached window (transfers are disabled). */
  detached?: boolean;
  /** Opens a (text) file in the built-in editor. */
  onOpenFile?: (target: {
    isLocal: boolean;
    connectionId?: string;
    path: string;
    name: string;
  }) => void;
}

const DND_TYPE = "application/x-linuxscp";

function errMsg(e: unknown): string {
  return isAppError(e) ? e.message : String(e);
}

const FilePane = forwardRef<PaneHandle, Props>(function FilePane(props, ref) {
  const { kind, connectionId, onStatus } = props;
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [showHidden, setShowHidden] = useState(false);
  const [sortKey, setSortKey] = useState<SortKey>("name");
  const [sortAsc, setSortAsc] = useState(true);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const [editing, setEditing] = useState<string | null>(null);
  const [dragOver, setDragOver] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; entry: FileEntry } | null>(null);

  const hist = useRef<{ stack: string[]; idx: number }>({ stack: [], idx: -1 });
  const pathRef = useRef(path);
  pathRef.current = path;

  const refreshSignal = useStore((s) =>
    kind === "local" ? s.refreshLocal : s.refreshRemote,
  );

  const isRemote = kind === "remote";
  const disabled = isRemote && !connectionId;

  const load = useCallback(
    async (p: string) => {
      if (isRemote && !connectionId) {
        setEntries([]);
        return;
      }
      setLoading(true);
      setError(undefined);
      try {
        const list = isRemote
          ? await api.listRemote(connectionId!, p)
          : await api.listLocal(p);
        setEntries(list);
        setSelected(new Set());
      } catch (e) {
        setError(errMsg(e));
        setEntries([]);
      } finally {
        setLoading(false);
      }
    },
    [isRemote, connectionId],
  );

  // Adopt the root path when it becomes available (home dir / connect result).
  useEffect(() => {
    if (props.rootPath && props.rootPath !== path) {
      setPath(props.rootPath);
      hist.current = { stack: [props.rootPath], idx: 0 };
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.rootPath]);

  useEffect(() => {
    if (path) void load(path);
  }, [path, refreshSignal, load]);

  const navigate = useCallback((p: string) => {
    const h = hist.current;
    h.stack = h.stack.slice(0, h.idx + 1);
    h.stack.push(p);
    h.idx = h.stack.length - 1;
    setPath(p);
  }, []);

  const goBack = () => {
    const h = hist.current;
    if (h.idx > 0) {
      h.idx--;
      setPath(h.stack[h.idx]);
    }
  };
  const goForward = () => {
    const h = hist.current;
    if (h.idx < h.stack.length - 1) {
      h.idx++;
      setPath(h.stack[h.idx]);
    }
  };
  const goUp = () => navigate(posixParent(path));

  const visible = useMemo(
    () => sortEntries(entries, sortKey, sortAsc, showHidden),
    [entries, sortKey, sortAsc, showHidden],
  );

  const selectedPaths = useCallback(
    () =>
      visible
        .filter((e) => selected.has(e.name))
        .map((e) => posixJoin(pathRef.current, e.name)),
    [visible, selected],
  );

  useImperativeHandle(
    ref,
    () => ({
      kind,
      currentDir: () => pathRef.current,
      selectedPaths: () =>
        visible
          .filter((e) => selected.has(e.name))
          .map((e) => posixJoin(pathRef.current, e.name)),
      refresh: () => void load(pathRef.current),
    }),
    [kind, visible, selected, load],
  );

  const toggleSort = (key: SortKey) => {
    if (sortKey === key) setSortAsc((a) => !a);
    else {
      setSortKey(key);
      setSortAsc(true);
    }
  };

  const onRowClick = (e: React.MouseEvent, entry: FileEntry) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (e.ctrlKey || e.metaKey) {
        if (next.has(entry.name)) next.delete(entry.name);
        else next.add(entry.name);
      } else {
        next.clear();
        next.add(entry.name);
      }
      return next;
    });
  };

  const openEntry = (entry: FileEntry) => {
    if (entry.isDir) {
      navigate(posixJoin(path, entry.name));
      return;
    }
    if (!isEditableFile(entry.name)) {
      onStatus(`„${entry.name}“ ist keine Textdatei und wird nicht geöffnet`);
      return;
    }
    props.onOpenFile?.({
      isLocal: kind === "local",
      connectionId,
      path: posixJoin(path, entry.name),
      name: entry.name,
    });
  };

  // --- actions ---
  const doNewFolder = async () => {
    const name = window.prompt("Name des neuen Ordners:");
    if (!name) return;
    const target = posixJoin(path, name);
    try {
      if (isRemote) await api.makeDirRemote(connectionId!, target);
      else await api.makeDirLocal(target);
      await load(path);
    } catch (e) {
      onStatus(errMsg(e), "error");
    }
  };

  const doDelete = async () => {
    const names = [...selected];
    if (names.length === 0) return;
    if (!window.confirm(`${names.length} Element(e) wirklich löschen?`)) return;
    for (const name of names) {
      const entry = entries.find((x) => x.name === name);
      const target = posixJoin(path, name);
      try {
        if (isRemote)
          await api.deleteRemote(connectionId!, target, !!entry?.isDir);
        else await api.deleteLocal(target, !!entry?.isDir);
      } catch (e) {
        onStatus(errMsg(e), "error");
      }
    }
    await load(path);
  };

  const doRename = async (entry: FileEntry) => {
    const next = window.prompt("Neuer Name:", entry.name);
    if (!next || next === entry.name) return;
    const from = posixJoin(path, entry.name);
    const to = posixJoin(path, next);
    try {
      if (isRemote) await api.renameRemote(connectionId!, from, to);
      else await api.renameLocal(from, to);
      await load(path);
    } catch (e) {
      onStatus(errMsg(e), "error");
    }
  };

  const doChmod = async (entry: FileEntry) => {
    const current = (entry.permissions ?? 0o644).toString(8).padStart(3, "0");
    const input = window.prompt("Rechte (oktal, z. B. 644):", current);
    if (!input) return;
    const mode = parseInt(input, 8);
    if (Number.isNaN(mode)) {
      onStatus("Ungültiger Oktalwert", "error");
      return;
    }
    const target = posixJoin(path, entry.name);
    try {
      if (isRemote) await api.setPermsRemote(connectionId!, target, mode);
      else await api.setPermsLocal(target, mode);
      await load(path);
    } catch (e) {
      onStatus(errMsg(e), "error");
    }
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Delete") void doDelete();
    else if (e.key === "Backspace") goUp();
    else if (e.key === "F2") {
      const one = [...selected][0];
      const entry = entries.find((x) => x.name === one);
      if (entry) void doRename(entry);
    } else if (e.key === "F5") {
      void load(path);
    }
  };

  // --- drag & drop ---
  const onDragStart = (e: React.DragEvent, entry: FileEntry) => {
    const paths = selected.has(entry.name)
      ? selectedPaths()
      : [posixJoin(path, entry.name)];
    e.dataTransfer.setData(DND_TYPE, JSON.stringify({ kind, paths }));
    e.dataTransfer.effectAllowed = "copy";
  };
  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const raw = e.dataTransfer.getData(DND_TYPE);
    if (!raw) return;
    const { kind: src, paths } = JSON.parse(raw) as {
      kind: PaneKind;
      paths: string[];
    };
    if (src === kind) return;
    props.onTransferFiles(src, paths, pathRef.current);
  };
  const onDragOver = (e: React.DragEvent) => {
    if (e.dataTransfer.types.includes(DND_TYPE)) {
      e.preventDefault();
      setDragOver(true);
    }
  };

  const SortHead = ({ label, k }: { label: string; k: SortKey }) => (
    <th onClick={() => toggleSort(k)} className="sortable">
      {label}
      {sortKey === k ? (sortAsc ? " ▲" : " ▼") : ""}
    </th>
  );

  return (
    <section
      className={`pane ${dragOver ? "drag-over" : ""}`}
      tabIndex={0}
      onKeyDown={onKeyDown}
      onDrop={onDrop}
      onDragOver={onDragOver}
      onDragLeave={() => setDragOver(false)}
      onClick={() => setMenu(null)}
    >
      <header className="pane-head">
        <span className="pane-title">
          <span className={`dot ${isRemote ? "remote" : "local"}`} />
          {props.title}
        </span>
        <div className="pane-tools">
          <button title="Zurück" onClick={goBack} disabled={disabled}>
            ◀
          </button>
          <button title="Vor" onClick={goForward} disabled={disabled}>
            ▶
          </button>
          <button title="Übergeordneter Ordner" onClick={goUp} disabled={disabled}>
            ↑
          </button>
          <button title="Aktualisieren" onClick={() => load(path)} disabled={disabled}>
            ⟳
          </button>
          <button
            title="Versteckte Dateien"
            className={showHidden ? "active" : ""}
            onClick={() => setShowHidden((v) => !v)}
            disabled={disabled}
          >
            ⦿
          </button>
          <button title="Neuer Ordner" onClick={doNewFolder} disabled={disabled}>
            ＋
          </button>
          {props.onDetach && (
            <button title="In eigenem Fenster öffnen" onClick={props.onDetach}>
              ⧉
            </button>
          )}
        </div>
      </header>

      <div className="breadcrumb">
        {editing !== null ? (
          <input
            autoFocus
            value={editing}
            onChange={(e) => setEditing(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                navigate(editing || "/");
                setEditing(null);
              } else if (e.key === "Escape") setEditing(null);
            }}
            onBlur={() => setEditing(null)}
          />
        ) : (
          <div className="crumbs" onDoubleClick={() => setEditing(path)}>
            {breadcrumbs(path).map((c, i) => (
              <button key={i} onClick={() => navigate(c.path)}>
                {c.label}
              </button>
            ))}
          </div>
        )}
      </div>

      <div className="pane-body">
        {disabled ? (
          <div className="pane-empty">Keine Verbindung</div>
        ) : (
          <table className="file-table">
            <thead>
              <tr>
                <SortHead label="Name" k="name" />
                <SortHead label="Größe" k="size" />
                <SortHead label="Geändert" k="modified" />
                <SortHead label="Rechte" k="permissions" />
              </tr>
            </thead>
            <tbody>
              {visible.map((entry) => (
                <tr
                  key={entry.name}
                  className={selected.has(entry.name) ? "selected" : ""}
                  draggable
                  onDragStart={(e) => onDragStart(e, entry)}
                  onClick={(e) => onRowClick(e, entry)}
                  onDoubleClick={() => openEntry(entry)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    if (!selected.has(entry.name))
                      setSelected(new Set([entry.name]));
                    setMenu({ x: e.clientX, y: e.clientY, entry });
                  }}
                >
                  <td className="cell-name">
                    <span className="icon">
                      {entry.isSymlink ? "🔗" : entry.isDir ? "📁" : "📄"}
                    </span>
                    {entry.name}
                  </td>
                  <td className="cell-size">
                    {entry.isDir ? "" : formatSize(entry.size)}
                  </td>
                  <td className="cell-date">{formatDate(entry.modified)}</td>
                  <td className="cell-perms mono">
                    {formatPerms(entry.permissions)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {loading && <div className="pane-loading">Lädt …</div>}
        {error && <div className="pane-error">⚠ {error}</div>}
      </div>

      <footer className="pane-foot">
        {selected.size > 0
          ? `${selected.size} ausgewählt`
          : `${visible.length} Element(e)`}
      </footer>

      {menu && (
        <ul
          className="context-menu"
          style={{ top: menu.y, left: menu.x }}
          onClick={() => setMenu(null)}
        >
          <li onClick={() => openEntry(menu.entry)}>
            {menu.entry.isDir ? "Öffnen" : "Im Editor öffnen ✎"}
          </li>
          {!props.detached && (
            <li onClick={() => props.onTransferFiles(kind, selectedPaths())}>
              {isRemote ? "Herunterladen ▽" : "Hochladen △"}
            </li>
          )}
          <li onClick={() => doRename(menu.entry)}>Umbenennen (F2)</li>
          <li onClick={() => doChmod(menu.entry)}>Rechte ändern …</li>
          <li className="danger" onClick={() => doDelete()}>
            Löschen (Entf)
          </li>
        </ul>
      )}
    </section>
  );
});

export default FilePane;
