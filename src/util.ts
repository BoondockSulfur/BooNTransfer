import type { FileEntry } from "./types";

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`;
}

export function formatDate(unixSecs: number | null): string {
  if (!unixSecs) return "";
  const d = new Date(unixSecs * 1000);
  return d.toLocaleString(undefined, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** rwxr-xr-x style string from a unix mode. */
export function formatPerms(mode: number | null): string {
  if (mode == null) return "";
  const bits = ["r", "w", "x"];
  let out = "";
  for (let shift = 6; shift >= 0; shift -= 3) {
    const group = (mode >> shift) & 0b111;
    for (let b = 0; b < 3; b++) {
      out += group & (0b100 >> b) ? bits[b] : "-";
    }
  }
  return out;
}

/** POSIX join used for the remote pane (and fine for local on Linux). */
export function posixJoin(base: string, name: string): string {
  if (name.startsWith("/")) return name;
  const trimmed = base.replace(/\/+$/, "");
  return trimmed === "" ? `/${name}` : `${trimmed}/${name}`;
}

export function posixParent(path: string): string {
  const norm = path.replace(/\/+$/, "");
  if (norm === "" || norm === "/") return "/";
  const idx = norm.lastIndexOf("/");
  return idx <= 0 ? "/" : norm.slice(0, idx);
}

export function breadcrumbs(path: string): { label: string; path: string }[] {
  const parts = path.split("/").filter(Boolean);
  const crumbs = [{ label: "/", path: "/" }];
  let acc = "";
  for (const p of parts) {
    acc += `/${p}`;
    crumbs.push({ label: p, path: acc });
  }
  return crumbs;
}

/** Extensions that must never be opened in the built-in text editor. */
const BINARY_EXTS = new Set([
  "jar", "class", "war", "ear", "wasm", "pyc", "pyo",
  "zip", "gz", "bz2", "xz", "7z", "rar", "tar", "tgz", "tbz", "z", "lz4", "zst",
  "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "tif", "tiff", "psd", "pdf",
  "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp",
  "exe", "dll", "so", "dylib", "bin", "o", "obj", "lib", "a",
  "mp3", "mp4", "avi", "mkv", "mov", "flac", "wav", "ogg", "webm", "m4a", "aac",
  "woff", "woff2", "ttf", "otf", "eot",
  "db", "sqlite", "sqlite3", "mdb",
  "iso", "dmg", "deb", "rpm", "appimage", "img", "msi",
]);

/** Files the built-in editor may open (unknown/extensionless files count as text). */
export function isEditableFile(name: string): boolean {
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return true; // no extension (or dotfile) → assume text
  return !BINARY_EXTS.has(name.slice(dot + 1).toLowerCase());
}

export type SortKey = "name" | "size" | "modified" | "permissions";

export function sortEntries(
  entries: FileEntry[],
  key: SortKey,
  asc: boolean,
  showHidden: boolean,
): FileEntry[] {
  const filtered = showHidden
    ? entries
    : entries.filter((e) => !e.name.startsWith("."));
  const dir = asc ? 1 : -1;
  return [...filtered].sort((a, b) => {
    // directories first, always
    if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
    switch (key) {
      case "size":
        return (a.size - b.size) * dir;
      case "modified":
        return ((a.modified ?? 0) - (b.modified ?? 0)) * dir;
      case "permissions":
        return ((a.permissions ?? 0) - (b.permissions ?? 0)) * dir;
      default:
        return a.name.localeCompare(b.name) * dir;
    }
  });
}
