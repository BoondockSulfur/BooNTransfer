import { useState } from "react";
import { useDraggable } from "../hooks";
import type { SyncDirection, SyncItem } from "../types";
import { formatSize } from "../util";

interface Props {
  localDir: string;
  remoteDir: string;
  onAnalyze: (direction: SyncDirection) => Promise<SyncItem[]>;
  onRun: (items: SyncItem[]) => Promise<void>;
  onClose: () => void;
}

const DIRECTIONS: { key: SyncDirection; label: string }[] = [
  { key: "upload", label: "Lokal → Remote" },
  { key: "download", label: "Remote → Lokal" },
  { key: "both", label: "Beide (neuere gewinnt)" },
];

const REASON: Record<SyncItem["reason"], string> = {
  new: "neu",
  newer: "neuer",
  size: "Größe ≠",
};

export default function SyncDialog({
  localDir,
  remoteDir,
  onAnalyze,
  onRun,
  onClose,
}: Props) {
  const drag = useDraggable();
  const [direction, setDirection] = useState<SyncDirection>("upload");
  const [items, setItems] = useState<SyncItem[] | null>(null);
  const [busy, setBusy] = useState(false);

  const analyze = async () => {
    setBusy(true);
    try {
      setItems(await onAnalyze(direction));
    } finally {
      setBusy(false);
    }
  };

  const run = async () => {
    if (!items) return;
    setBusy(true);
    try {
      await onRun(items);
      onClose();
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal sync" style={drag.style} onClick={(e) => e.stopPropagation()}>
        <h2 className="modal-drag" {...drag.handleProps}>
          ⇄ Verzeichnisse synchronisieren
        </h2>
        <div className="sync-paths mono">
          <div title={localDir}>📁 {localDir || "—"}</div>
          <div title={remoteDir}>🖥 {remoteDir || "—"}</div>
        </div>

        <div className="sync-dir">
          {DIRECTIONS.map((d) => (
            <label key={d.key}>
              <input
                type="radio"
                name="syncdir"
                checked={direction === d.key}
                onChange={() => {
                  setDirection(d.key);
                  setItems(null);
                }}
              />
              {d.label}
            </label>
          ))}
        </div>

        {items && (
          <div className="sync-list">
            {items.length === 0 ? (
              <div className="muted">Bereits synchron — nichts zu tun.</div>
            ) : (
              <ul>
                {items.slice(0, 200).map((i) => (
                  <li key={i.direction + i.name}>
                    <span className="sync-arrow">
                      {i.direction === "upload" ? "▲" : "▼"}
                    </span>
                    <span className="sync-name" title={`${i.local} ↔ ${i.remote}`}>
                      {i.name}
                    </span>
                    <span className="muted">{REASON[i.reason]}</span>
                    <span className="muted">{formatSize(i.size)}</span>
                  </li>
                ))}
                {items.length > 200 && (
                  <li className="muted">… und {items.length - 200} weitere</li>
                )}
              </ul>
            )}
          </div>
        )}

        <p className="hint">
          Es werden keine Dateien gelöscht — nur fehlende bzw. neuere kopiert.
        </p>

        <div className="form-actions">
          <button className="ghost" onClick={onClose}>
            Schließen
          </button>
          <span className="spacer" />
          <button onClick={analyze} disabled={busy}>
            {busy && !items ? "Analysiere …" : "Analysieren"}
          </button>
          <button
            className="primary"
            onClick={run}
            disabled={busy || !items || items.length === 0}
          >
            Übertragen{items && items.length > 0 ? ` (${items.length})` : ""}
          </button>
        </div>
      </div>
    </div>
  );
}
