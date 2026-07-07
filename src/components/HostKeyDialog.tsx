import { useDraggable } from "../hooks";
import { useT } from "../i18n";

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
  const t = useT();
  const drag = useDraggable();
  return (
    <div className="modal-backdrop">
      <div className="modal hostkey" style={drag.style}>
        <h2
          className={`modal-drag ${changed ? "warn" : ""}`}
          {...drag.handleProps}
        >
          {changed ? t("hostkey.changed") : t("hostkey.unknown")}
        </h2>
        <p>
          {t("hostkey.presents", {
            host,
            kind: changed ? t("hostkey.kindChanged") : t("hostkey.kindUnknown"),
          })}
        </p>
        {changed && <p className="danger">{t("hostkey.warn")}</p>}
        <dl className="kv">
          <dt>{t("hostkey.keyType")}</dt>
          <dd className="mono">{keyType}</dd>
          <dt>{t("hostkey.fingerprint")}</dt>
          <dd className="mono break">{fingerprint}</dd>
        </dl>
        <p className="hint">{t("hostkey.hint")}</p>
        <div className="form-actions">
          <button className="ghost" onClick={onReject}>
            {t("hostkey.cancel")}
          </button>
          <span className="spacer" />
          <button className={changed ? "danger" : "primary"} onClick={onAccept}>
            {t("hostkey.trust")}
          </button>
        </div>
      </div>
    </div>
  );
}
