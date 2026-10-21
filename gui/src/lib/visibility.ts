import { createSignal } from "solid-js";

// Whether anyone can see the app. On macOS closing the window only hides it
// (Orca lives in the menu bar), so without this every poll would keep running
// with nobody looking. The webview's own `document.hidden` covers minimising
// and occlusion; the Rust side also emits `orca-window-visible` when it hides
// or shows the window, since a hidden window isn't guaranteed to flip it.

const [documentVisible, setDocumentVisible] = createSignal(!document.hidden);
const [windowShown, setWindowShown] = createSignal(true);

const listeners = new Set<(visible: boolean) => void>();
let last = documentVisible() && windowShown();

function update() {
  const now = documentVisible() && windowShown();
  if (now === last) return;
  last = now;
  for (const cb of Array.from(listeners)) cb(now);
}

document.addEventListener("visibilitychange", () => {
  setDocumentVisible(!document.hidden);
  update();
});

import("@tauri-apps/api/event")
  .then(({ listen }) =>
    listen<boolean>("orca-window-visible", (e) => {
      setWindowShown(e.payload);
      update();
    }),
  )
  .catch(() => { /* not running under Tauri */ });

/** Reactive: true while the window is shown and not hidden or minimised. */
export const appVisible = () => documentVisible() && windowShown();

/** Call `cb` whenever the app becomes visible or hidden. Returns an unsubscribe function. */
export function onVisibilityChange(cb: (visible: boolean) => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}
