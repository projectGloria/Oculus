import { useRef } from "react";
import { api } from "../api.js";

/** Start native file dragging only after a deliberate movement; retain click/keyboard Open. */
export function useFileDrag(path, onError) {
  const start = useRef(null);
  const dragged = useRef(false);
  return {
    onPointerDown(e) {
      start.current = e.button === 0 ? { x: e.clientX, y: e.clientY } : null;
      dragged.current = false;
      if (start.current) e.currentTarget.setPointerCapture(e.pointerId);
    },
    onPointerMove(e) {
      if (!start.current || !(e.buttons & 1) || dragged.current) return;
      if (Math.hypot(e.clientX - start.current.x, e.clientY - start.current.y) < 6) return;
      dragged.current = true;
      start.current = null;
      if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
      api.dragSavedFile(path).catch((error) => onError(`Could not drag saved file: ${error}`));
    },
    onPointerUp() { start.current = null; },
    onPointerCancel() { start.current = null; },
    onDragStart(e) { e.preventDefault(); },
    onClickCapture(e) {
      if (dragged.current && e.detail !== 0) { e.preventDefault(); e.stopPropagation(); }
    },
  };
}
