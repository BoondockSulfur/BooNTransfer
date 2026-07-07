import { useMemo } from "react";
import { useDialog, useDraggable } from "../hooks";
import { useT } from "../i18n";
import type { ConflictPolicy, PlannedFile } from "../types";
import { formatSize } from "../util";

interface Props {
  direction: "upload" | "download";
  files: PlannedFile[];
  onResolve: (policy: ConflictPolicy) => void;
  onCancel: () => void;
}

const POLICIES: ConflictPolicy[] = ["overwrite", "resume", "rename", "skip"];

export default function ConflictDialog({
  direction,
  files,
  onResolve,
  onCancel,
}: Props) {
  const t = useT();
  const conflicts = useMemo(() => files.filter((f) => f.exists), [files]);
  const anyResumable = conflicts.some((f) => f.destSize < f.size);
  const drag = useDraggable();
  const dialogRef = useDialog(onCancel);

  return (
    <div className="modal-backdrop" onClick={onCancel}>
      <div
        ref={dialogRef}
        className="modal conflict"
        role="dialog"
        aria-modal="true"
        aria-label={t("conflict.title")}
        tabIndex={-1}
        style={drag.style}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="modal-drag" {...drag.handleProps}>
          {t("conflict.title")}
        </h2>
        <p>
          {t("conflict.message", {
            n: conflicts.length,
            total: files.length,
            side:
              direction === "upload"
                ? t("conflict.sideRemote")
                : t("conflict.sideLocal"),
          })}
        </p>

        <ul className="conflict-list">
          {conflicts.slice(0, 60).map((f) => (
            <li key={f.remote + f.local}>
              <span className="cf-name" title={`${f.local} → ${f.remote}`}>
                {f.name}
              </span>
              <span className="cf-sizes muted">
                {t("conflict.sizes", {
                  new: formatSize(f.size),
                  old: formatSize(f.destSize),
                })}
              </span>
            </li>
          ))}
          {conflicts.length > 60 && (
            <li className="muted">
              {t("conflict.more", { n: conflicts.length - 60 })}
            </li>
          )}
        </ul>

        <div className="conflict-actions">
          {POLICIES.map((key) => {
            const hint = t(`conflict.${key}Hint`);
            return (
              <button
                key={key}
                className={key === "overwrite" ? "primary" : ""}
                disabled={key === "resume" && !anyResumable}
                title={hint}
                onClick={() => onResolve(key)}
              >
                {t(`conflict.${key}`)}
                <small>{hint}</small>
              </button>
            );
          })}
        </div>
        <div className="form-actions">
          <button className="ghost" onClick={onCancel}>
            {t("hostkey.cancel")}
          </button>
        </div>
      </div>
    </div>
  );
}
