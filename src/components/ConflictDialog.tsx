import { useMemo } from "react";
import { useDraggable } from "../hooks";
import type { ConflictPolicy, PlannedFile } from "../types";
import { formatSize } from "../util";

interface Props {
  direction: "upload" | "download";
  files: PlannedFile[];
  onResolve: (policy: ConflictPolicy) => void;
  onCancel: () => void;
}

const POLICIES: { key: ConflictPolicy; label: string; hint: string }[] = [
  { key: "overwrite", label: "Überschreiben", hint: "Zieldateien ersetzen" },
  { key: "resume", label: "Fortsetzen", hint: "ab vorhandener Größe weiterladen" },
  { key: "rename", label: "Umbenennen", hint: "als „Name (1)“ speichern" },
  { key: "skip", label: "Überspringen", hint: "vorhandene Dateien auslassen" },
];

export default function ConflictDialog({
  direction,
  files,
  onResolve,
  onCancel,
}: Props) {
  const conflicts = useMemo(() => files.filter((f) => f.exists), [files]);
  const anyResumable = conflicts.some((f) => f.destSize < f.size);
  const drag = useDraggable();

  return (
    <div className="modal-backdrop" onClick={onCancel}>
      <div
        className="modal conflict"
        style={drag.style}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="modal-drag" {...drag.handleProps}>
          Namenskonflikt
        </h2>
        <p>
          {conflicts.length} von {files.length} Datei(en) existieren bereits im{" "}
          {direction === "upload" ? "Remote-" : "lokalen "}Zielordner. Wie soll
          für <strong>alle</strong> verfahren werden?
        </p>

        <ul className="conflict-list">
          {conflicts.slice(0, 60).map((f) => (
            <li key={f.remote + f.local}>
              <span className="cf-name" title={`${f.local} → ${f.remote}`}>
                {f.name}
              </span>
              <span className="cf-sizes muted">
                neu {formatSize(f.size)} · vorhanden {formatSize(f.destSize)}
              </span>
            </li>
          ))}
          {conflicts.length > 60 && (
            <li className="muted">… und {conflicts.length - 60} weitere</li>
          )}
        </ul>

        <div className="conflict-actions">
          {POLICIES.map((p) => (
            <button
              key={p.key}
              className={p.key === "overwrite" ? "primary" : ""}
              disabled={p.key === "resume" && !anyResumable}
              title={p.hint}
              onClick={() => onResolve(p.key)}
            >
              {p.label}
              <small>{p.hint}</small>
            </button>
          ))}
        </div>
        <div className="form-actions">
          <button className="ghost" onClick={onCancel}>
            Abbrechen
          </button>
        </div>
      </div>
    </div>
  );
}
