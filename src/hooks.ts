import { useCallback, useRef, useState } from "react";

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
    style: { transform: `translate(${pos.x}px, ${pos.y}px)` } as React.CSSProperties,
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
