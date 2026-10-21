import { onMount, onCleanup } from "solid-js";
import { appVisible, onVisibilityChange } from "./visibility";

export interface Poller {
  /** Run now (or once more right after the run in progress) and restart the interval. */
  now(): void;
  stop(): void;
}

/**
 * Run `fn` every `intervalMs`, counted from when the previous run finished,
 * so a slow daemon never gets overlapping requests. Pauses while the app is
 * hidden and runs once as soon as it is visible again.
 */
export function startPoller(fn: () => unknown, intervalMs: number, opts?: { immediate?: boolean }): Poller {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let running = false;
  let again = false;
  let stopped = false;
  let lastRun = 0;

  const schedule = () => {
    if (timer) clearTimeout(timer);
    timer = null;
    if (stopped || !appVisible()) return;
    timer = setTimeout(run, Math.max(0, lastRun + intervalMs - Date.now()));
  };

  const run = async () => {
    if (stopped) return;
    if (running) {
      again = true;
      return;
    }
    if (timer) clearTimeout(timer);
    timer = null;
    running = true;
    try {
      await fn();
    } catch {
      // Callers handle their own errors; a failed run must not stop the poll.
    } finally {
      running = false;
      lastRun = Date.now();
    }
    if (again) {
      again = false;
      void run();
    } else {
      schedule();
    }
  };

  const unsubscribe = onVisibilityChange((visible) => {
    if (!visible) {
      if (timer) clearTimeout(timer);
      timer = null;
    } else if (Date.now() - lastRun >= intervalMs) {
      void run();
    } else {
      schedule();
    }
  });

  if (opts?.immediate === false) {
    lastRun = Date.now();
    schedule();
  } else {
    void run();
  }

  return {
    now: () => void run(),
    stop: () => {
      stopped = true;
      if (timer) clearTimeout(timer);
      timer = null;
      unsubscribe();
    },
  };
}

/** {@link startPoller} for the lifetime of the calling component. */
export function usePoller(fn: () => unknown, intervalMs: number, opts?: { immediate?: boolean }): Poller {
  let poller: Poller | null = null;
  onMount(() => {
    poller = startPoller(fn, intervalMs, opts);
  });
  onCleanup(() => poller?.stop());
  return {
    now: () => poller?.now(),
    stop: () => poller?.stop(),
  };
}
