import { useState } from "react";
import { useDraggable } from "../hooks";
import { useT } from "../i18n";
import type { SyncDirection, SyncItem } from "../types";
import { formatSize } from "../util";

interface Props {
  localDir: string;
  remoteDir: string;
  onAnalyze: (direction: SyncDirection) => Promise<SyncItem[]>;
  onRun: (items: SyncItem[]) => Promise<void>;
  onClose: () => void;
}

const DIRECTIONS: SyncDirection[] = ["upload", "download", "both"];
const REASON_KEY: Record<SyncItem["reason"], string> = {
  new: "sync.reasonNew",
  newer: "sync.reasonNewer",
  size: "sync.reasonSize",
};

export default function SyncDialog({
  localDir,
  remoteDir,
  onAnalyze,
  onRun,
  onClose,
}: Props) {
  const t = useT();
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
          {t("sync.title")}
        </h2>
        <div className="sync-paths mono">
          <div title={localDir}>📁 {localDir || "—"}</div>
          <div title={remoteDir}>🖥 {remoteDir || "—"}</div>
        </div>

        <div className="sync-dir">
          {DIRECTIONS.map((d) => (
            <label key={d}>
              <input
                type="radio"
                name="syncdir"
                checked={direction === d}
                onChange={() => {
                  setDirection(d);
                  setItems(null);
                }}
              />
              {t(`sync.${d}`)}
            </label>
          ))}
        </div>

        {items && (
          <div className="sync-list">
            {items.length === 0 ? (
              <div className="muted">{t("sync.synced")}</div>
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
                    <span className="muted">{t(REASON_KEY[i.reason])}</span>
                    <span className="muted">{formatSize(i.size)}</span>
                  </li>
                ))}
                {items.length > 200 && (
                  <li className="muted">
                    {t("sync.more", { n: items.length - 200 })}
                  </li>
                )}
              </ul>
            )}
          </div>
        )}

        <p className="hint">{t("sync.hint")}</p>

        <div className="form-actions">
          <button className="ghost" onClick={onClose}>
            {t("sync.close")}
          </button>
          <span className="spacer" />
          <button onClick={analyze} disabled={busy}>
            {busy && !items ? t("sync.analyzing") : t("sync.analyze")}
          </button>
          <button
            className="primary"
            onClick={run}
            disabled={busy || !items || items.length === 0}
          >
            {t("sync.transfer")}
            {items && items.length > 0 ? ` (${items.length})` : ""}
          </button>
        </div>
      </div>
    </div>
  );
}
