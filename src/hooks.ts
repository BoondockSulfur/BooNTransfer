import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Accessibility helper for modal dialogs: focuses the dialog on open and closes
 * it on Escape. Returns a ref to attach to the dialog container (give it
 * `role="dialog" aria-modal="true"` and an accessible label).
 */
export function useDialog(onClose: () => void) {
  const ref = useRef<HTMLDivElement>(null);
  // Keep the latest handler without re-running the mount effect (which would
  // steal focus back to the container on every render).
  const cb = useRef(onClose);
  cb.current = onClose;
  useEffect(() => {
    ref.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        cb.current();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);
  return ref;
}

/** Window-level mouse-drag helper that reports cumulative delta from the start. */
function beginDrag(onMove: (dx: number, dy: number) => void, cursor?: string) {
  return (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const sx = e.clientX;
    const sy = e.clientY;
    const move = (ev: MouseEvent) => onMove(ev.clientX - sx, ev.clientY - sy);
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.style.userSelect = "";
      if (cursor) document.body.style.cursor = "";
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    document.body.style.userSelect = "none";
    if (cursor) document.body.style.cursor = cursor;
  };
}

/** Makes an element draggable via a handle; returns a transform style + handle props. */
export function useDraggable() {
  const [pos, setPos] = useState({ x: 0, y: 0 });
  const posRef = useRef(pos);
  posRef.current = pos;

  const onMouseDown = useCallback((e: React.MouseEvent) => {
    const start = posRef.current;
    beginDrag((dx, dy) => setPos({ x: start.x + dx, y: start.y + dy }))(e);
  }, []);

  return {
    handleProps: { onMouseDown },
    style: {
      transform: `translate(${pos.x}px, ${pos.y}px)`,
    } as React.CSSProperties,
  };
}

/**
 * Resizable splitter. `onDelta(startValue, dx, dy)` computes the new value from
 * the value captured at drag start, so callers stay stateless per drag.
 */
export function useSplitter(
  valueRef: React.MutableRefObject<number>,
  apply: (next: number) => void,
  fromDelta: (start: number, dx: number, dy: number) => number,
  cursor: string,
) {
  return useCallback(
    (e: React.MouseEvent) => {
      const start = valueRef.current;
      beginDrag((dx, dy) => apply(fromDelta(start, dx, dy)), cursor)(e);
    },
    [valueRef, apply, fromDelta, cursor],
  );
}

export function clamp(v: number, min: number, max: number) {
  return Math.min(max, Math.max(min, v));
}
