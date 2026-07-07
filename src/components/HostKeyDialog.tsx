import { useDraggable } from "../hooks";

interface Props {
  host: string;
  fingerprint: string;
  keyType: string;
  changed: boolean;
  onAccept: () => void;
  onReject: () => void;
}

export default function HostKeyDialog({
  host,
  fingerprint,
  keyType,
  changed,
  onAccept,
  onReject,
}: Props) {
  const drag = useDraggable();
  return (
    <div className="modal-backdrop">
      <div className="modal hostkey" style={drag.style}>
        <h2
          className={`modal-drag ${changed ? "warn" : ""}`}
          {...drag.handleProps}
        >
          {changed ? "⚠ Host-Key HAT SICH GEÄNDERT" : "Unbekannter Host-Key"}
        </h2>
        <p>
          Der Server <strong>{host}</strong> präsentiert einen{" "}
          {changed ? "anderen als den gespeicherten" : "bisher unbekannten"}{" "}
          Host-Key.
        </p>
        {changed && (
          <p className="danger">
            Dies kann auf einen Man-in-the-Middle-Angriff hindeuten. Nur
            fortfahren, wenn die Änderung erwartet wird.
          </p>
        )}
        <dl className="kv">
          <dt>Schlüsseltyp</dt>
          <dd className="mono">{keyType}</dd>
          <dt>Fingerprint</dt>
          <dd className="mono break">{fingerprint}</dd>
        </dl>
        <p className="hint">
          Bei Bestätigung wird der Schlüssel in <code>~/.ssh/known_hosts</code>{" "}
          gespeichert.
        </p>
        <div className="form-actions">
          <button className="ghost" onClick={onReject}>
            Abbrechen
          </button>
          <span className="spacer" />
          <button
            className={changed ? "danger" : "primary"}
            onClick={onAccept}
          >
            Vertrauen &amp; verbinden
          </button>
        </div>
      </div>
    </div>
  );
}
