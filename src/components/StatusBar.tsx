import { useT } from "../i18n";
import { useStore } from "../state/store";
import { formatSize } from "../util";

export default function StatusBar() {
  const t = useT();
  const status = useStore((s) => s.status);
  const connection = useStore((s) => s.connection);
  const transfers = useStore((s) => s.transfers);

  const running = transfers.filter((t) => t.status === "running");
  const totalBytes = running.reduce((a, t) => a + t.size, 0);
  const doneBytes = running.reduce((a, t) => a + t.transferred, 0);

  return (
    <footer className="statusbar">
      <span className={`conn-indicator ${connection ? "on" : "off"}`}>
        {connection ? t("statusbar.on") : t("statusbar.off")}
      </span>
      <span className={`status-msg ${status.kind}`}>
        {status.text || t("status.notConnected")}
      </span>
      <span className="spacer" />
      {running.length > 0 && (
        <span className="xfer-summary">
          ⇅ {running.length} · {formatSize(doneBytes)} /{" "}
          {formatSize(totalBytes)}
        </span>
      )}
    </footer>
  );
}
