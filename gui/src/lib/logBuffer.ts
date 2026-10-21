// Data structures behind the log viewers. A busy container can log hundreds
// of lines per second into a 20k-line buffer, so everything here is
// incremental: appending a batch costs O(batch), not O(buffer).

/**
 * Capped append-only buffer. Every item gets a sequence number that stays
 * stable while older items are trimmed off the head, so views (filters,
 * measured row heights, scroll anchors) can refer to items by seq.
 */
export class LogBuffer<T> {
  private items: T[] = [];
  /** Seq of items[0]. */
  head = 0;
  /** Bumped by reset() — seqs from an older generation are meaningless. */
  generation = 0;

  constructor(readonly max: number) {}

  get length(): number {
    return this.items.length;
  }

  /** Seq one past the newest item. */
  get end(): number {
    return this.head + this.items.length;
  }

  at(seq: number): T {
    return this.items[seq - this.head];
  }

  push(batch: T[]): void {
    for (const item of batch) this.items.push(item);
    const over = this.items.length - this.max;
    if (over > 0) {
      // One splice per batch (a pointer memmove), not a copy per line.
      this.items.splice(0, over);
      this.head += over;
    }
  }

  /** Replace the contents (initial fetch, refresh, dropping a container). */
  reset(items: T[]): void {
    this.head = this.end;
    this.generation++;
    this.items = items.length > this.max ? items.slice(-this.max) : items.slice();
  }

  toArray(): T[] {
    return this.items.slice();
  }
}

/**
 * The seqs of a LogBuffer's items that pass a predicate, kept up to date
 * incrementally: while the predicate is unchanged, update() only drops
 * trimmed seqs and tests the newly appended items.
 *
 * The predicate returns a weight — 0 excludes the item, anything else
 * includes it and is summed into `total` (the viewers use it for the
 * number of search matches).
 */
export class FilteredView<T> {
  seqs: number[] = [];
  total = 0;
  private weights: number[] = [];
  private scannedTo = 0;
  private generation = -1;
  private pred: ((item: T) => number) | null | undefined = undefined;

  update(buf: LogBuffer<T>, pred: ((item: T) => number) | null): void {
    if (pred !== this.pred || buf.generation !== this.generation) {
      this.pred = pred;
      this.generation = buf.generation;
      this.seqs = [];
      this.weights = [];
      this.total = 0;
      this.scannedTo = buf.head;
    }
    let drop = 0;
    while (drop < this.seqs.length && this.seqs[drop] < buf.head) {
      this.total -= this.weights[drop];
      drop++;
    }
    if (drop > 0) {
      this.seqs.splice(0, drop);
      this.weights.splice(0, drop);
    }
    for (let seq = Math.max(this.scannedTo, buf.head); seq < buf.end; seq++) {
      const w = pred ? pred(buf.at(seq)) : 1;
      if (w > 0) {
        this.seqs.push(seq);
        this.weights.push(w);
        this.total += w;
      }
    }
    this.scannedTo = buf.end;
  }
}

/**
 * Collects items and hands them to `flush` at most once per animation
 * frame, so a burst of log events becomes one state update. `cap` bounds
 * the queue while frames aren't firing (window hidden/minimised).
 */
export function createFrameBatcher<T>(flush: (items: T[]) => void, cap: number) {
  let queue: T[] = [];
  let frame = 0;
  const run = () => {
    frame = 0;
    const items = queue;
    queue = [];
    flush(items);
  };
  return {
    push(items: T[]) {
      for (const item of items) queue.push(item);
      if (queue.length > cap) queue.splice(0, queue.length - cap);
      if (!frame) frame = requestAnimationFrame(run);
    },
    cancel() {
      if (frame) cancelAnimationFrame(frame);
      frame = 0;
      queue = [];
    },
  };
}
