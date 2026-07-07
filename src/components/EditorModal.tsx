import { ask } from "@tauri-apps/plugin-dialog";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { useDialog, useDraggable } from "../hooks";
import { useT } from "../i18n";
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
  /** Render as a full standalone window instead of a floating modal. */
  windowed?: boolean;
}

export default function EditorModal({
  isLocal,
  connectionId,
  path,
  name,
  onClose,
  onSaved,
  windowed = false,
}: Props) {
  const t = useT();
  const drag = useDraggable();
  const [content, setContent] = useState("");
  const [original, setOriginal] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const dirty = content !== original;

  // Keep the latest dirty flag / translator reachable from the (stable) window
  // close listener without re-registering it on every keystroke.
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const tRef = useRef(t);
  tRef.current = t;

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
      dirtyRef.current = false; // reflect immediately for the close guard
      onSaved(isLocal);
    } catch (e) {
      setError(isAppError(e) ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const close = () => {
    if (windowed) {
      // Closing the window fires onCloseRequested below, which does the
      // unsaved-changes guard for both this button and the native title bar.
      onClose();
      return;
    }
    if (dirty && !window.confirm(t("editor.discard"))) return;
    onClose();
  };
  const dialogRef = useDialog(close);

  // In a standalone window, put the cursor straight into the text.
  useEffect(() => {
    if (windowed && !loading && !error) areaRef.current?.focus();
  }, [windowed, loading, error]);

  // Guard the window (native ✕ included) against discarding unsaved edits.
  useEffect(() => {
    if (!windowed) return;
    const win = getCurrentWebviewWindow();
    const unlisten = win.onCloseRequested(async (event) => {
      if (!dirtyRef.current) return; // clean → let it close
      event.preventDefault();
      const tr = tRef.current;
      const discard = await ask(tr("editor.discard"), {
        title: name,
        kind: "warning",
        okLabel: tr("editor.discardOk"),
        cancelLabel: tr("editor.keepEditing"),
      });
      if (discard) {
        dirtyRef.current = false; // second close passes the guard
        await win.close();
      }
    });
    return () => void unlisten.then((u) => u());
  }, [windowed, name]);

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

  const body = (
    <>
      <div
        className={`editor-head${windowed ? "" : " modal-drag"}`}
        {...(windowed ? {} : drag.handleProps)}
      >
        <span className="editor-title" title={path}>
          ✎ {name}
          {dirty ? " •" : ""}
        </span>
        <span className="editor-loc muted">
          {isLocal
            ? t("editor.local")
            : t("editor.remote", { id: connectionId ?? "" })}
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
        placeholder={loading ? t("editor.loading") : ""}
      />

      <div className="form-actions">
        <span className="muted">
          {loading
            ? t("editor.loading")
            : t("editor.lines", { n: content.split("\n").length })}
        </span>
        <span className="spacer" />
        <button className="ghost" onClick={close}>
          {t("editor.close")}
        </button>
        <button
          onClick={() => void save()}
          disabled={saving || loading || !dirty}
        >
          {saving ? t("editor.saving") : t("editor.save")}
        </button>
        <button
          className="primary"
          disabled={saving || loading}
          onClick={async () => {
            await save();
            onClose();
          }}
        >
          {t("editor.saveClose")}
        </button>
      </div>
    </>
  );

  if (windowed) {
    return <div className="editor editor-window">{body}</div>;
  }

  return (
    <div className="modal-backdrop" onClick={close}>
      <div
        ref={dialogRef}
        className="modal editor"
        role="dialog"
        aria-modal="true"
        aria-label={name}
        tabIndex={-1}
        style={drag.style}
        onClick={(e) => e.stopPropagation()}
      >
        {body}
      </div>
    </div>
  );
}
