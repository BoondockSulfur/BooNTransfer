import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ConnectResult,
  FileEntry,
  PlannedFile,
  ResolvedFile,
  SessionInfo,
  SyncDirection,
  SyncItem,
  TransferItem,
  VerifyResult,
} from "./types";

export type TransferDir = "upload" | "download";

export const api = {
  // sessions
  listSessions: () => invoke<SessionInfo[]>("list_sessions"),
  saveSession: (session: SessionInfo, password?: string, passphrase?: string) =>
    invoke<string>("save_session", { session, password, passphrase }),
  deleteSession: (id: string) => invoke<void>("delete_session", { id }),

  // connection
  connect: (sessionId: string, trustHostKey?: boolean) =>
    invoke<ConnectResult>("connect", { sessionId, trustHostKey }),
  disconnect: (connectionId: string) =>
    invoke<void>("disconnect", { connectionId }),

  // local fs
  localHome: () => invoke<string>("local_home"),
  listLocal: (path: string) => invoke<FileEntry[]>("list_local", { path }),
  makeDirLocal: (path: string) => invoke<void>("make_dir_local", { path }),
  deleteLocal: (path: string, isDir: boolean) =>
    invoke<void>("delete_local", { path, isDir }),
  renameLocal: (from: string, to: string) =>
    invoke<void>("rename_local", { from, to }),
  setPermsLocal: (path: string, mode: number) =>
    invoke<void>("set_permissions_local", { path, mode }),

  // remote fs
  listRemote: (connectionId: string, path: string) =>
    invoke<FileEntry[]>("list_remote", { connectionId, path }),
  makeDirRemote: (connectionId: string, path: string) =>
    invoke<void>("make_dir_remote", { connectionId, path }),
  deleteRemote: (connectionId: string, path: string, isDir: boolean) =>
    invoke<void>("delete_remote", { connectionId, path, isDir }),
  renameRemote: (connectionId: string, from: string, to: string) =>
    invoke<void>("rename_remote", { connectionId, from, to }),
  setPermsRemote: (connectionId: string, path: string, mode: number) =>
    invoke<void>("set_permissions_remote", { connectionId, path, mode }),

  // transfers (two-step: plan → resolve conflicts → enqueue)
  planTransfers: (
    connectionId: string,
    direction: TransferDir,
    paths: string[],
    destDir: string,
  ) =>
    invoke<PlannedFile[]>("plan_transfers", {
      connectionId,
      direction,
      paths,
      destDir,
    }),
  enqueueTransfers: (
    connectionId: string,
    direction: TransferDir,
    destDir: string,
    files: ResolvedFile[],
  ) =>
    invoke<number>("enqueue_transfers", {
      connectionId,
      direction,
      destDir,
      files,
    }),
  planSync: (
    connectionId: string,
    localDir: string,
    remoteDir: string,
    direction: SyncDirection,
  ) =>
    invoke<SyncItem[]>("plan_sync", {
      connectionId,
      localDir,
      remoteDir,
      direction,
    }),
  readText: (
    connectionId: string | undefined,
    path: string,
    isLocal: boolean,
  ) => invoke<string>("read_text", { connectionId, path, isLocal }),
  writeText: (
    connectionId: string | undefined,
    path: string,
    isLocal: boolean,
    content: string,
  ) => invoke<void>("write_text", { connectionId, path, isLocal, content }),
  listTransfers: () => invoke<TransferItem[]>("list_transfers"),
  verifyTransfer: (
    connectionId: string,
    localPath: string,
    remotePath: string,
  ) =>
    invoke<VerifyResult>("verify_transfer", {
      connectionId,
      localPath,
      remotePath,
    }),
  transferPause: (id: string) => invoke<void>("transfer_pause", { id }),
  transferResume: (id: string) => invoke<void>("transfer_resume", { id }),
  transferCancel: (id: string) => invoke<void>("transfer_cancel", { id }),
  transferRetry: (id: string) => invoke<void>("transfer_retry", { id }),
  clearFinished: () => invoke<void>("clear_finished"),

  // interactive terminal (SSH)
  terminalOpen: (
    connectionId: string,
    terminalId: string,
    cols: number,
    rows: number,
  ) => invoke<void>("terminal_open", { connectionId, terminalId, cols, rows }),
  terminalWrite: (terminalId: string, data: number[]) =>
    invoke<void>("terminal_write", { terminalId, data }),
  terminalResize: (terminalId: string, cols: number, rows: number) =>
    invoke<void>("terminal_resize", { terminalId, cols, rows }),
  terminalClose: (terminalId: string) =>
    invoke<void>("terminal_close", { terminalId }),
};

export function onQueueUpdate(
  cb: (items: TransferItem[]) => void,
): Promise<UnlistenFn> {
  return listen<TransferItem[]>("queue-update", (e) => cb(e.payload));
}

export function onTransferProgress(
  cb: (id: string, transferred: number) => void,
): Promise<UnlistenFn> {
  return listen<[string, number]>("transfer-progress", (e) =>
    cb(e.payload[0], e.payload[1]),
  );
}

/** Broadcast that a file was saved, so other windows can refresh their panes. */
export function emitFileSaved(isLocal: boolean): void {
  void emit("boon:file-saved", { isLocal });
}

export function onFileSaved(
  cb: (isLocal: boolean) => void,
): Promise<UnlistenFn> {
  return listen<{ isLocal: boolean }>("boon:file-saved", (e) =>
    cb(e.payload.isLocal),
  );
}

export function onTerminalOutput(
  terminalId: string,
  cb: (data: Uint8Array) => void,
): Promise<UnlistenFn> {
  return listen<number[]>(`terminal-output:${terminalId}`, (e) =>
    cb(new Uint8Array(e.payload)),
  );
}

export function onTerminalExit(
  terminalId: string,
  cb: () => void,
): Promise<UnlistenFn> {
  return listen(`terminal-exit:${terminalId}`, () => cb());
}

export interface TransferRequest {
  source: "local" | "remote";
  connectionId: string | null;
  paths: string[];
}

/** A detached pane asks the main window to run a transfer for its selection. */
export function emitTransferRequest(req: TransferRequest): void {
  void emit("boon:transfer-request", req);
}

export function onTransferRequest(
  cb: (req: TransferRequest) => void,
): Promise<UnlistenFn> {
  return listen<TransferRequest>("boon:transfer-request", (e) => cb(e.payload));
}
