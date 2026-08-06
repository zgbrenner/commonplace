/**
 * A latest-wins guard for overlapping async requests.
 *
 * Search-as-you-type fires a request per (debounced) keystroke, and the
 * responses can come back out of order. Callers mark each request as the
 * newest with `begin(key)` just before sending it, then check
 * `isCurrent(key)` when the response arrives; a response for anything but
 * the newest key is stale and should be ignored.
 *
 * Pure and framework-free so it can be unit tested directly.
 */
export interface LatestGuard<K> {
  /** Mark `key` as the newest request, invalidating everything before it. */
  begin(key: K): void;
  /** True while `key` is still the newest request begun. */
  isCurrent(key: K): boolean;
}

export function createLatestGuard<K>(): LatestGuard<K> {
  let latest: K | undefined;
  let anyBegun = false;
  return {
    begin(key) {
      latest = key;
      anyBegun = true;
    },
    isCurrent(key) {
      return anyBegun && Object.is(latest, key);
    },
  };
}
