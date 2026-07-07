import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { useDraggable } from "../hooks";
import { isAppError } from "../types";

export interface EditorTarget {
  isLocal: boolean;
  connectionId?: string;
  path: string;
  name: string;
}

interface Props extends EditorTarget {
  onClose: () => void;
  onSaved: (isLocal: boolean) => void;
}

export default function EditorModal({
  isLocal,
  connectionId,
  path,
  name,
  onClose,
  onSaved,
}: Props) {
  const drag = useDraggable();
  const [content, setContent] = useState("");
  const [original, setOriginal] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const dirty = content !== original;

  useEffect(() => {
    void (async () => {
      setLoading(true);
      setError(undefined);
      try {
        const text = await api.readText(connectionId, path, isLocal);
        setContent(text);
        setOriginal(text);
      } catch (e) {
        setError(isAppError(e) ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const save = async () => {
    if (saving) return;
    setSaving(true);
    setError(undefined);
    try {
      await api.writeText(connectionId, path, isLocal, content);
      setOriginal(content);
      onSaved(isLocal);
    } catch (e) {
      setError(isAppError(e) ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const close = () => {
    if (dirty && !window.confirm("Ungespeicherte Änderungen verwerfen?")) return;
    onClose();
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      void save();
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      const el = e.currentTarget;
      const start = el.selectionStart;
      const end = el.selectionEnd;
      const next = content.slice(0, start) + "  " + content.slice(end);
      setContent(next);
      requestAnimationFrame(() => {
        el.selectionStart = el.selectionEnd = start + 2;
      });
    }
  };

  return (
    <div className="modal-backdrop" onClick={close}>
      <div className="modal editor" style={drag.style} onClick={(e) => e.stopPropagation()}>
        <div className="editor-head modal-drag" {...drag.handleProps}>
          <span className="editor-title" title={path}>
            ✎ {name}
            {dirty ? " •" : ""}
          </span>
          <span className="editor-loc muted">
            {isLocal ? "lokal" : `remote: ${connectionId ?? ""}`}
          </span>
        </div>

        {error && <div className="editor-error">⚠ {error}</div>}

        <textarea
          ref={areaRef}
          className="editor-area mono"
          value={content}
          spellCheck={false}
          disabled={loading || !!error}
          onChange={(e) => setContent(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={loading ? "Lädt …" : ""}
        />

        <div className="form-actions">
          <span className="muted">
            {loading ? "Lädt …" : `${content.split("\n").length} Zeilen`}
          </span>
          <span className="spacer" />
          <button className="ghost" onClick={close}>
            Schließen
          </button>
          <button onClick={() => void save()} disabled={saving || loading || !dirty}>
            {saving ? "Speichert …" : "Speichern (Strg+S)"}
          </button>
          <button
            className="primary"
            disabled={saving || loading}
            onClick={async () => {
              await save();
              onClose();
            }}
          >
            Speichern &amp; Schließen
          </button>
        </div>
      </div>
    </div>
  );
}
