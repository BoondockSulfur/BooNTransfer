import { useMemo, useState } from "react";
import { useDraggable } from "../hooks";
import { useT } from "../i18n";
import { api } from "../api";
import { useStore } from "../state/store";
import type { AuthType, Protocol, SessionInfo } from "../types";
import { isAppError } from "../types";

interface Props {
  onClose: () => void;
  onConnect: (sessionId: string) => void;
}

const EMPTY: SessionInfo = {
  id: "",
  name: "",
  protocol: "sftp",
  host: "",
  port: 22,
  username: "",
  authType: "password",
  keyPath: "",
  folder: "",
  remoteDir: "",
  localDir: "",
};

export default function ConnectionManager({ onClose, onConnect }: Props) {
  const t = useT();
  const sessions = useStore((s) => s.sessions);
  const setSessions = useStore((s) => s.setSessions);
  const setStatus = useStore((s) => s.setStatus);

  const [query, setQuery] = useState("");
  const [form, setForm] = useState<SessionInfo>({ ...EMPTY });
  const [password, setPassword] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [saving, setSaving] = useState(false);
  const drag = useDraggable();

  const filtered = useMemo(() => {
    const q = query.toLowerCase();
    return [...sessions]
      .filter(
        (s) =>
          s.name.toLowerCase().includes(q) ||
          s.host.toLowerCase().includes(q) ||
          (s.username ?? "").toLowerCase().includes(q),
      )
      .sort((a, b) => (b.lastUsed ?? 0) - (a.lastUsed ?? 0));
  }, [sessions, query]);

  const reload = async () => setSessions(await api.listSessions());

  const edit = (s: SessionInfo) => {
    setForm({ ...s });
    setPassword("");
    setPassphrase("");
  };

  const upd = <K extends keyof SessionInfo>(k: K, v: SessionInfo[K]) =>
    setForm((f) => ({ ...f, [k]: v }));

  const isFtp = form.protocol === "ftp" || form.protocol === "ftps";

  const onProtocol = (proto: Protocol) => {
    const ftp = proto === "ftp" || proto === "ftps";
    setForm((f) => ({
      ...f,
      protocol: proto,
      port: ftp ? 21 : 22,
      // SSH key/agent auth doesn't apply to FTP.
      authType: ftp ? "password" : f.authType,
    }));
  };

  const save = async (): Promise<string | null> => {
    if (!form.name.trim() || !form.host.trim() || !form.username.trim()) {
      setStatus(t("conn.errRequired"), "error");
      return null;
    }
    setSaving(true);
    try {
      const id = await api.saveSession(
        form,
        form.authType === "password" ? password : undefined,
        form.authType === "key" ? passphrase : undefined,
      );
      await reload();
      setForm((f) => ({ ...f, id }));
      setStatus(t("conn.saved"), "success");
      return id;
    } catch (e) {
      setStatus(isAppError(e) ? e.message : String(e), "error");
      return null;
    } finally {
      setSaving(false);
    }
  };

  const remove = async (id: string) => {
    if (!window.confirm(t("conn.confirmDelete"))) return;
    await api.deleteSession(id);
    await reload();
    if (form.id === id) setForm({ ...EMPTY });
  };

  const connect = async () => {
    let id = form.id;
    if (!id) {
      const saved = await save();
      if (!saved) return;
      id = saved;
    }
    onConnect(id);
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal conn-manager"
        style={drag.style}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-drag conn-titlebar" {...drag.handleProps}>
          <span>{t("conn.title")}</span>
        </div>
        <div className="conn-list">
          <div className="conn-list-head">
            <input
              className="search"
              placeholder={t("conn.search")}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            <button
              className="ghost"
              title={t("conn.newSession")}
              onClick={() => edit({ ...EMPTY })}
            >
              ＋
            </button>
          </div>
          <ul>
            {filtered.map((s) => (
              <li
                key={s.id}
                className={s.id === form.id ? "active" : ""}
                onClick={() => edit(s)}
                onDoubleClick={() => onConnect(s.id)}
              >
                <div className="conn-name">{s.name}</div>
                <div className="conn-sub">
                  {s.protocol}://{s.username}@{s.host}:{s.port}
                </div>
                {s.folder ? <span className="tag">{s.folder}</span> : null}
                <button
                  className="mini danger"
                  title={t("conn.delete")}
                  onClick={(e) => {
                    e.stopPropagation();
                    void remove(s.id);
                  }}
                >
                  ✕
                </button>
              </li>
            ))}
            {filtered.length === 0 && (
              <li className="muted">{t("conn.noSessions")}</li>
            )}
          </ul>
        </div>

        <div className="conn-form">
          <h2>{form.id ? t("conn.editSession") : t("conn.newSession")}</h2>
          <label>
            {t("conn.name")}
            <input
              value={form.name}
              onChange={(e) => upd("name", e.target.value)}
            />
          </label>
          <div className="row">
            <label className="grow">
              {t("conn.host")}
              <input
                value={form.host}
                onChange={(e) => upd("host", e.target.value)}
              />
            </label>
            <label className="port">
              {t("conn.port")}
              <input
                type="number"
                value={form.port}
                onChange={(e) => upd("port", Number(e.target.value))}
              />
            </label>
          </div>
          <div className="row">
            <label className="grow">
              {t("conn.user")}
              <input
                value={form.username}
                onChange={(e) => upd("username", e.target.value)}
              />
            </label>
            <label>
              {t("conn.protocol")}
              <select
                value={form.protocol}
                onChange={(e) => onProtocol(e.target.value as Protocol)}
              >
                <option value="sftp">SFTP</option>
                <option value="scp">SCP</option>
                <option value="ftp">FTP</option>
                <option value="ftps">FTPS</option>
              </select>
            </label>
          </div>

          <label>
            {t("conn.auth")}
            <select
              value={form.authType}
              disabled={isFtp}
              onChange={(e) => upd("authType", e.target.value as AuthType)}
            >
              <option value="password">{t("conn.authPassword")}</option>
              {!isFtp && <option value="key">{t("conn.authKey")}</option>}
              {!isFtp && <option value="agent">{t("conn.authAgent")}</option>}
            </select>
          </label>

          {form.authType === "password" && (
            <label>
              {t("conn.password")}
              <input
                type="password"
                autoComplete="new-password"
                placeholder={form.id ? t("conn.unchanged") : ""}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
          )}
          {form.authType === "key" && (
            <>
              <label>
                {t("conn.keyFile")}
                <input
                  value={form.keyPath ?? ""}
                  placeholder="~/.ssh/id_ed25519"
                  onChange={(e) => upd("keyPath", e.target.value)}
                />
              </label>
              <label>
                {t("conn.passphrase")}
                <input
                  type="password"
                  autoComplete="new-password"
                  value={passphrase}
                  onChange={(e) => setPassphrase(e.target.value)}
                />
              </label>
            </>
          )}
          {form.authType === "agent" && (
            <p className="hint">{t("conn.agentHint")}</p>
          )}

          <div className="row">
            <label className="grow">
              {t("conn.remoteStart")}
              <input
                value={form.remoteDir ?? ""}
                placeholder={t("conn.homePlaceholder")}
                onChange={(e) => upd("remoteDir", e.target.value)}
              />
            </label>
            <label className="grow">
              {t("conn.folder")}
              <input
                value={form.folder ?? ""}
                onChange={(e) => upd("folder", e.target.value)}
              />
            </label>
          </div>

          <div className="form-actions">
            <button className="ghost" onClick={onClose}>
              {t("conn.close")}
            </button>
            <span className="spacer" />
            {form.id && (
              <button className="danger ghost" onClick={() => remove(form.id)}>
                {t("conn.delete")}
              </button>
            )}
            <button onClick={() => void save()} disabled={saving}>
              {t("conn.save")}
            </button>
            <button className="primary" onClick={() => void connect()}>
              {t("conn.connect")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
