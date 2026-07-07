import { create } from "zustand";
import { detectLang, type Lang } from "../i18n";
import type { ConnectResult, SessionInfo, TransferItem } from "../types";

export type Theme = "light" | "dark";
export type StatusKind = "info" | "error" | "success";

interface AppStore {
  theme: Theme;
  toggleTheme: () => void;

  lang: Lang;
  setLang: (l: Lang) => void;

  sessions: SessionInfo[];
  setSessions: (s: SessionInfo[]) => void;

  connection: ConnectResult | null;
  connecting: boolean;
  setConnection: (c: ConnectResult | null) => void;
  setConnecting: (v: boolean) => void;

  status: { text: string; kind: StatusKind };
  setStatus: (text: string, kind?: StatusKind) => void;

  transfers: TransferItem[];
  setTransfers: (t: TransferItem[]) => void;
  updateProgress: (id: string, transferred: number) => void;

  // Refresh signals: bumping forces the matching pane to reload.
  refreshLocal: number;
  refreshRemote: number;
  bumpLocal: () => void;
  bumpRemote: () => void;
}

const initialTheme: Theme =
  (localStorage.getItem("linuxscp.theme") as Theme) ??
  (window.matchMedia?.("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark");

export const useStore = create<AppStore>((set) => ({
  theme: initialTheme,
  toggleTheme: () =>
    set((s) => {
      const theme: Theme = s.theme === "dark" ? "light" : "dark";
      localStorage.setItem("linuxscp.theme", theme);
      return { theme };
    }),

  lang: detectLang(),
  setLang: (lang) => {
    localStorage.setItem("boon.lang", lang);
    set({ lang });
  },

  sessions: [],
  setSessions: (sessions) => set({ sessions }),

  connection: null,
  connecting: false,
  setConnection: (connection) => set({ connection }),
  setConnecting: (connecting) => set({ connecting }),

  status: { text: "", kind: "info" },
  setStatus: (text, kind = "info") => set({ status: { text, kind } }),

  transfers: [],
  setTransfers: (transfers) => set({ transfers }),
  updateProgress: (id, transferred) =>
    set((s) => ({
      transfers: s.transfers.map((t) =>
        t.id === id ? { ...t, transferred } : t,
      ),
    })),

  refreshLocal: 0,
  refreshRemote: 0,
  bumpLocal: () => set((s) => ({ refreshLocal: s.refreshLocal + 1 })),
  bumpRemote: () => set((s) => ({ refreshRemote: s.refreshRemote + 1 })),
}));
