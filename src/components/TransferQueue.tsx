import { useState } from "react";
import { api } from "../api";
import { useStore } from "../state/store";
import type { TransferItem } from "../types";
import { formatSize } from "../util";

type CheckState = "pending" | "ok" | "fail" | "na" | "err";

function pct(t: TransferItem): number {
  if (t.size === 0) return t.status === "completed" ? 100 : 0;
  return Math.min(100, Math.round((t.transferred / t.size) * 100));
}

const CHECK_LABEL: Record<CheckState, string> = {
  pending: "⏳ SHA-256 …",
  ok: "✓ SHA-256",
  fail: "✗ SHA-256",
  na: "SHA-256 n. v.",
  err: "⚠ SHA-256",
};

function Row({
  t,
  check,
  onVerify,
}: {
  t: TransferItem;
  check?: CheckState;
  onVerify: (t: TransferItem) => void;
}) {
  const p = pct(t);
  return (
    <li className={`queue-row status-${t.status}`}>
      <span className="dir" title={t.direction}>
        {t.direction === "upload" ? "△" : "▽"}
      </span>
      <span className="qname" title={`${t.localPath} ↔ ${t.remotePath}`}>
        {t.name}
      </span>
      <span className="qbar">
        <span className="qbar-fill" style={{ width: `${p}%` }} />
      </span>
      <span className="qmeta">
        {formatSize(t.transferred)} / {formatSize(t.size)} · {p}%
      </span>
      <span className="qstatus">{t.status}</span>
      <span className="qactions">
        {t.status === "running" && (
          <button title="Pause" onClick={() => api.transferPause(t.id)}>
            ⏸
          </button>
        )}
        {t.status === "paused" && (
          <button title="Fortsetzen" onClick={() => api.transferResume(t.id)}>
            ▶
          </button>
        )}
        {(t.status === "running" ||
          t.status === "paused" ||
          t.status === "queued") && (
          <button title="Abbrechen" onClick={() => api.transferCancel(t.id)}>
            ✕
          </button>
        )}
        {(t.status === "failed" || t.status === "cancelled") && (
          <button title="Wiederholen" onClick={() => api.transferRetry(t.id)}>
            ↻
          </button>
        )}
        {t.status === "completed" && (
          <button
            title="SHA-256-Prüfsumme lokal ↔ remote vergleichen"
            onClick={() => onVerify(t)}
            disabled={check === "pending"}
          >
            ✓?
          </button>
        )}
        {check && <span className={`check check-${check}`}>{CHECK_LABEL[check]}</span>}
      </span>
      {t.error && (
        <span className="qerror" title={t.error}>
          ⚠ {t.error}
        </span>
      )}
    </li>
  );
}

export default function TransferQueue({ onDetach }: { onDetach?: () => void }) {
  const transfers = useStore((s) => s.transfers);
  const setStatus = useStore((s) => s.setStatus);
  const [checks, setChecks] = useState<Record<string, CheckState>>({});
  const active = transfers.filter(
    (t) => t.status === "running" || t.status === "queued" || t.status === "paused",
  ).length;

  const verify = async (t: TransferItem) => {
    setChecks((c) => ({ ...c, [t.id]: "pending" }));
    try {
      const r = await api.verifyTransfer(t.connectionId, t.localPath, t.remotePath);
      const state: CheckState = !r.supported ? "na" : r.matches ? "ok" : "fail";
      setChecks((c) => ({ ...c, [t.id]: state }));
      if (state === "fail") setStatus(`SHA-256 stimmt NICHT: ${t.name}`, "error");
      else if (state === "ok") setStatus(`SHA-256 verifiziert: ${t.name}`, "success");
      else setStatus(`SHA-256 für ${t.name} nicht verfügbar`);
    } catch (e) {
      setChecks((c) => ({ ...c, [t.id]: "err" }));
      setStatus(String(e), "error");
    }
  };

  return (
    <div className="queue">
      <div className="queue-head">
        <strong>Übertragungen</strong>
        <span className="muted">
          {active} aktiv · {transfers.length} gesamt
        </span>
        <span className="spacer" />
        <button className="ghost mini" onClick={() => api.clearFinished()}>
          Abgeschlossene entfernen
        </button>
        {onDetach && (
          <button className="ghost mini" title="In eigenem Fenster öffnen" onClick={onDetach}>
            ⧉
          </button>
        )}
      </div>
      {transfers.length === 0 ? (
        <div className="queue-empty muted">Keine Übertragungen</div>
      ) : (
        <ul className="queue-list">
          {transfers.map((t) => (
            <Row key={t.id} t={t} check={checks[t.id]} onVerify={verify} />
          ))}
        </ul>
      )}
    </div>
  );
}
