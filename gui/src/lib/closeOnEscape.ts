import { onCleanup } from "solid-js";

// Escape closes the innermost open thing: a menu or popover registered with
// closeOnEscape, else the topmost dialog. One capture-phase listener on window
// runs before the components' own Escape handlers, and stops the event when it
// closes something, so a single key press never closes two layers.

type Entry = { isOpen: () => boolean; close: () => void };

// Most recently mounted last: a dropdown inside a dialog mounts after the
// dialog, so it closes first.
const stack: Entry[] = [];

/**
 * Close a menu or popover on Escape. Call it in a component body.
 */
export function closeOnEscape(isOpen: () => boolean, close: () => void) {
  const entry = { isOpen, close };
  stack.push(entry);
  onCleanup(() => {
    const i = stack.indexOf(entry);
    if (i >= 0) stack.splice(i, 1);
  });
}

/**
 * The layer on top: overlays render while open, later ones above earlier.
 * Layers marked `data-own-escape` (command palette, confirm dialog, drawer…)
 * handle Escape themselves.
 */
function topLayer(): HTMLElement | null {
  const all = document.querySelectorAll<HTMLElement>(".modal-overlay, [data-own-escape]");
  for (let i = all.length - 1; i >= 0; i--) {
    if (all[i].getClientRects().length > 0) return all[i];
  }
  return null;
}

function onKey(e: KeyboardEvent) {
  if (e.key !== "Escape" || e.isComposing) return;
  for (let i = stack.length - 1; i >= 0; i--) {
    if (stack[i].isOpen()) {
      e.stopPropagation();
      e.preventDefault();
      stack[i].close();
      return;
    }
  }
  // In a code editor Escape dismisses autocomplete and the like; closing the
  // dialog there could throw away edits.
  if ((e.target as HTMLElement | null)?.closest?.(".monaco-editor")) return;
  const overlay = topLayer();
  if (!overlay || overlay.hasAttribute("data-own-escape")) return;
  e.stopPropagation();
  e.preventDefault();
  // Same as clicking the backdrop, so each dialog's own close rules apply
  // (several refuse to close while they are working).
  overlay.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
  overlay.dispatchEvent(new MouseEvent("click", { bubbles: true }));
}

/** Call once at startup. */
export function installEscapeHandling() {
  window.addEventListener("keydown", onKey, true);
}
