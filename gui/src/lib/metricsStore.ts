import { batch, createMemo, createRoot, createSignal, type Accessor, type Setter } from "solid-js";
import { showToast } from "../components/Toast";
import { formatBytes } from "./format";

export interface MetricSnapshot {
  timestamp: number;
  cpu: number;
  memory: number;
  memoryLimit: number;
  networkRx: number;
  networkTx: number;
}

const MAX_POINTS = 60;
// Metrics for a container whose most recent snapshot is older than this are
// considered stale and opportunistically pruned on the next recordMetrics()
// call. This guarantees the history map is bounded even when the user
// never opens the Dashboard (which is the only page wiring
// `pruneMetricsExcept`).
const STALE_AFTER_MS = 10 * 60 * 1000;
// One signal per container, so a chart of one container only updates when
// that container gets a new reading. `ids` changes only when containers come
// and go; the dashboard's totals read every container.
type Series = { get: Accessor<MetricSnapshot[]>; set: Setter<MetricSnapshot[]> };
const series = new Map<string, Series>();
const [ids, setIds] = createSignal<string[]>([]);

function seriesFor(id: string): Series {
  let s = series.get(id);
  if (!s) {
    const [get, set] = createSignal<MetricSnapshot[]>([]);
    s = { get, set };
    series.set(id, s);
    setIds([...series.keys()]);
  }
  return s;
}

function dropSeries(drop: (id: string) => boolean) {
  let changed = false;
  for (const id of [...series.keys()]) {
    if (!drop(id)) continue;
    series.delete(id);
    changed = true;
    for (const k of Object.keys(lastAlertTime)) {
      if (k.split(":")[0] === id) delete lastAlertTime[k];
    }
    delete highCpuStreak[id];
  }
  if (changed) setIds([...series.keys()]);
}

// --- Resource usage alerts ---
const MEMORY_THRESHOLD_PERCENT = 90;
const CPU_THRESHOLD_PERCENT = 90;
const CPU_CONSECUTIVE_READINGS = 3;
const ALERT_COOLDOWN_MS = 5 * 60 * 1000; // 5 minutes

// Track last alert time per container+metric to avoid spam
const lastAlertTime: Record<string, number> = {};
// Track consecutive high-CPU readings per container
const highCpuStreak: Record<string, number> = {};

function canAlert(key: string): boolean {
  const now = Date.now();
  const last = lastAlertTime[key];
  if (last && now - last < ALERT_COOLDOWN_MS) return false;
  lastAlertTime[key] = now;
  return true;
}

function evaluateAlerts(containerId: string, containerName: string, snapshot: MetricSnapshot) {
  // Memory alert: only when a limit is set
  if (snapshot.memoryLimit > 0) {
    const memPercent = (snapshot.memory / snapshot.memoryLimit) * 100;
    if (memPercent > MEMORY_THRESHOLD_PERCENT) {
      const key = `${containerId}:memory`;
      if (canAlert(key)) {
        showToast(
          `Container '${containerName}' is using ${Math.round(memPercent)}% of its memory limit (${formatBytes(snapshot.memory)} / ${formatBytes(snapshot.memoryLimit)})`,
          "info"
        );
      }
    }
  }

  // CPU alert: must exceed threshold for N consecutive readings
  if (snapshot.cpu > CPU_THRESHOLD_PERCENT) {
    highCpuStreak[containerId] = (highCpuStreak[containerId] || 0) + 1;
  } else {
    highCpuStreak[containerId] = 0;
  }

  if (highCpuStreak[containerId] >= CPU_CONSECUTIVE_READINGS) {
    const key = `${containerId}:cpu`;
    if (canAlert(key)) {
      showToast(
        `Container '${containerName}' CPU usage is at ${snapshot.cpu.toFixed(1)}% (sustained high usage)`,
        "info"
      );
    }
  }
}

export interface MetricsEntry {
  id: string;
  name?: string;
  snapshot: MetricSnapshot;
}

/** Record one poll's readings for several containers as a single update. */
export function recordMetricsBatch(entries: MetricsEntry[]) {
  // History of a container with no reading for a while (a short-lived
  // container) is dropped here, so the map stays bounded even when the
  // Dashboard, which also prunes, is never opened.
  const cutoff = Date.now() - STALE_AFTER_MS;
  const fresh = new Set(entries.map((e) => e.id));
  batch(() => {
    dropSeries((id) => {
      if (fresh.has(id)) return false;
      const hist = series.get(id)!.get();
      return hist.length === 0 || hist[hist.length - 1].timestamp < cutoff;
    });
    for (const e of entries) {
      seriesFor(e.id).set((prev) => [...prev, e.snapshot].slice(-MAX_POINTS));
    }
  });
  for (const e of entries) evaluateAlerts(e.id, e.name || e.id.slice(0, 12), e.snapshot);
}

export function recordMetrics(containerId: string, snapshot: MetricSnapshot, containerName?: string) {
  recordMetricsBatch([{ id: containerId, name: containerName, snapshot }]);
}

export function getMetricsHistory(containerId: string): MetricSnapshot[] {
  const s = series.get(containerId);
  if (s) return s.get();
  ids(); // re-run once this container's history appears
  return [];
}

export function getCpuHistory(containerId: string): number[] {
  return getMetricsHistory(containerId).map((s) => s.cpu);
}

export function getMemoryHistory(containerId: string): number[] {
  return getMetricsHistory(containerId).map((s) =>
    s.memoryLimit > 0 ? (s.memory / s.memoryLimit) * 100 : 0
  );
}

// --- Chart helpers (return {time, value}[] for TimeChart) ---

/** Sum of one metric over all containers, aligned on the latest readings. */
function totalHistory(pick: (s: MetricSnapshot) => number): Array<{ time: number; value: number }> {
  const all = ids().map((id) => series.get(id)?.get() ?? []);
  if (all.length === 0) return [];
  const longest = all.reduce((a, b) => (a.length >= b.length ? a : b));
  const maxLen = longest.length;
  const result: Array<{ time: number; value: number }> = [];
  for (let i = 0; i < maxLen; i++) {
    let sum = 0;
    for (const arr of all) {
      const idx = arr.length - maxLen + i;
      if (idx >= 0) sum += pick(arr[idx]);
    }
    result.push({ time: longest[i].timestamp, value: sum });
  }
  return result;
}

// Memoised once for the app, so every reader of the totals shares one pass.
const totals = createRoot(() => ({
  cpu: createMemo(() => totalHistory((s) => s.cpu)),
  memory: createMemo(() => totalHistory((s) => s.memory)),
}));

export const getDashboardCpuHistory = () => totals.cpu();
export const getDashboardMemHistory = () => totals.memory();

export function getCpuChartHistory(containerId: string): Array<{ time: number; value: number }> {
  return getMetricsHistory(containerId).map((s) => ({ time: s.timestamp, value: s.cpu }));
}

export function getMemChartHistory(containerId: string): Array<{ time: number; value: number }> {
  return getMetricsHistory(containerId).map((s) => ({ time: s.timestamp, value: s.memory }));
}

export function clearAllMetrics() {
  dropSeries(() => true);
}

// Register a single module-level `orca-host-switch` listener so the
// metrics store is wiped whenever the active host changes — regardless
// of whether the Dashboard is mounted. Previously the listener lived in
// DashboardPage.onMount, which meant metrics leaked across hosts for
// any user who switched hosts without visiting the Dashboard first.
let hostSwitchListenerRegistered = false;
function initHostSwitchListener() {
  if (hostSwitchListenerRegistered) return;
  if (typeof document === "undefined") return;
  document.addEventListener("orca-host-switch", () => {
    clearAllMetrics();
  });
  hostSwitchListenerRegistered = true;
}
initHostSwitchListener();

/**
 * Drop metrics for containers that are no longer present. Call this from a
 * single place (e.g. the Dashboard after it reconciles the container list)
 * so dead IDs don't leak memory across a long session.
 */
export function pruneMetricsExcept(liveIds: Iterable<string>) {
  const keep = new Set(liveIds);
  dropSeries((id) => !keep.has(id));
}
