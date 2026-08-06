/**
 * Native file drag-and-drop, via Tauri's webview event.
 *
 * The webview's own HTML5 drop events only expose file *names* — the real
 * absolute paths never reach `DataTransfer` for security reasons — so this
 * hook listens to Tauri's drag-drop event instead, which carries the paths
 * the backend needs. The event is window-wide: dropping anywhere in the app
 * counts, which is fine for a single-composer window.
 */
import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";

/**
 * Subscribe to native file drops for the lifetime of the component.
 * Returns whether a drag is currently hovering the window, for a
 * "drop files to attach" highlight.
 */
export function useFileDrop(onDrop: (paths: string[]) => void): boolean {
  const [dragging, setDragging] = useState(false);

  // Keep the newest callback in a ref so the listener — registered once —
  // always sees fresh props without re-subscribing on every render.
  const onDropRef = useRef(onDrop);
  useEffect(() => {
    onDropRef.current = onDrop;
  });

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") {
          setDragging(true);
        } else if (event.payload.type === "leave") {
          setDragging(false);
        } else {
          // "drop": the payload carries real absolute paths.
          setDragging(false);
          if (event.payload.paths.length > 0) {
            onDropRef.current(event.payload.paths);
          }
        }
      })
      .then((fn) => {
        // The component can unmount before registration resolves; clean up
        // either way so the webview doesn't accumulate dead handlers.
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch(() => {
        // Outside a Tauri window (tests, a plain browser tab) there is no
        // native event source; drag-and-drop simply stays inactive.
      });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  return dragging;
}
