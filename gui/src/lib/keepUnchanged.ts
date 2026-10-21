/**
 * `next`, with each item that is unchanged since `prev` (same key, same
 * content) replaced by its previous object, and `prev` itself when nothing
 * changed. Polled lists arrive as fresh objects every time; `<For>` matches
 * rows by object, so without this every poll rebuilds every row.
 */
export function keepUnchanged<T>(prev: T[], next: T[], key: (item: T) => string = defaultKey): T[] {
  if (prev.length === 0) return next;
  const before = new Map<string, { item: T; json: string }>();
  for (const item of prev) before.set(key(item), { item, json: JSON.stringify(item) });
  let same = prev.length === next.length;
  const result = next.map((item, i) => {
    const old = before.get(key(item));
    if (old && old.json === JSON.stringify(item)) {
      if (prev[i] !== old.item) same = false;
      return old.item;
    }
    same = false;
    return item;
  });
  return same ? prev : result;
}

function defaultKey(item: unknown): string {
  const o = item as { namespace?: string; name?: string; uid?: string; id?: string };
  return `${o.namespace ?? ""}/${o.name ?? o.uid ?? o.id ?? JSON.stringify(item)}`;
}
