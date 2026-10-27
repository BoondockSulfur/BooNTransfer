import { FitAddon } from "@xterm/addon-fit";
import { Terminal as Xterm } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef } from "react";
import { api, onTerminalExit, onTerminalOutput } from "../api";
import { useStore } from "../state/store";

const DARK = {
  background: "#12161c",
  foreground: "#e6ebf1",
  cursor: "#3fb8ab",
};
const LIGHT = {
  background: "#ffffff",
  foreground: "#1c2530",
  cursor: "#1f7a72",
};

/** An interactive SSH shell backed by xterm.js and the terminal_* commands. */
export default function TerminalView({
  connectionId,
}: {
  connectionId: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const termRef = useRef<Xterm | null>(null);
  const theme = useStore((s) => s.theme);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const term = new Xterm({
      fontFamily: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace',
      fontSize: 13,
      cursorBlink: true,
      theme: theme === "dark" ? DARK : LIGHT,
    });
    termRef.current = term;
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();

    const id = crypto.randomUUID();
    const enc = new TextEncoder();
    const unlisteners: Array<() => void> = [];
    // The view can unmount while the shell is still being opened; the async
    // setup then cleans up after itself instead of leaving a shell behind.
    let disposed = false;

    void (async () => {
      // Subscribe before opening so no early output is missed.
      const offOutput = await onTerminalOutput(id, (data) => term.write(data));
      const offExit = await onTerminalExit(id, () => {
        term.write("\r\n\x1b[90m— session closed —\x1b[0m\r\n");
      });
      if (disposed) {
        offOutput();
        offExit();
        return;
      }
      unlisteners.push(offOutput, offExit);
      await api.terminalOpen(connectionId, id, term.cols, term.rows);
      if (disposed) {
        void api.terminalClose(id);
        return;
      }
      term.focus();
    })();

    const dData = term.onData((d) => {
      void api.terminalWrite(id, Array.from(enc.encode(d)));
    });
    const dResize = term.onResize(({ cols, rows }) => {
      void api.terminalResize(id, cols, rows);
    });
    const ro = new ResizeObserver(() => {
      try {
        fit.fit();
      } catch {
        /* container not laid out yet */
      }
    });
    ro.observe(el);

    return () => {
      disposed = true;
      dData.dispose();
      dResize.dispose();
      ro.disconnect();
      unlisteners.forEach((u) => u());
      void api.terminalClose(id);
      term.dispose();
      termRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionId]);

  useEffect(() => {
    if (termRef.current) {
      termRef.current.options.theme = theme === "dark" ? DARK : LIGHT;
    }
  }, [theme]);

  return <div className="terminal-view" ref={ref} />;
}
