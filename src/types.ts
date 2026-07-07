export type Protocol = "sftp" | "scp" | "ftp" | "ftps";
export type AuthType = "password" | "key" | "agent";

export interface FileEntry {
  name: string;
  isDir: boolean;
  isSymlink: boolean;
  size: number;
  modified: number | null;
  permissions: number | null;
  owner: string | null;
  group: string | null;
}

export interface SessionInfo {
  id: string;
  name: string;
  protocol: Protocol;
  host: string;
  port: number;
  username: string;
  authType: AuthType;
  keyPath?: string | null;
  folder?: string | null;
  remoteDir?: string | null;
  localDir?: string | null;
  lastUsed?: number | null;
}

export interface ConnectResult {
  connectionId: string;
  homeDir: string;
  remoteDir?: string | null;
  localDir?: string | null;
}

export type TransferDirection = "upload" | "download";
export type TransferStatus =
  | "queued"
  | "running"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled";

export interface TransferItem {
  id: string;
  connectionId: string;
  direction: TransferDirection;
  localPath: string;
  remotePath: string;
  name: string;
  size: number;
  transferred: number;
  offset: number;
  status: TransferStatus;
  error?: string | null;
}

export interface VerifyResult {
  local: string | null;
  remote: string | null;
  matches: boolean;
  supported: boolean;
}

export type ConflictPolicy = "overwrite" | "skip" | "rename" | "resume";

export interface PlannedFile {
  local: string;
  remote: string;
  name: string;
  size: number;
  exists: boolean;
  destSize: number;
}

export interface ResolvedFile extends PlannedFile {
  policy: ConflictPolicy;
}

export type SyncDirection = "upload" | "download" | "both";

export interface SyncItem {
  direction: "upload" | "download";
  local: string;
  remote: string;
  name: string;
  size: number;
  reason: "new" | "newer" | "size";
}

/** Shape of the serialized backend error (see error.rs). */
export interface AppError {
  kind: "host_key" | "not_connected" | "auth_failed" | "cancelled" | "error";
  message: string;
  fingerprint?: string;
  keyType?: string;
  changed?: boolean;
}

export function isAppError(e: unknown): e is AppError {
  return typeof e === "object" && e !== null && "kind" in e && "message" in e;
}
