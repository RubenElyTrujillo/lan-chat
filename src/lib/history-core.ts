// Pure history-cutover logic: no DOM, no Tauri, fully injectable — the
// backend commands are reached only through the wrappers in history.ts.
// Semantics (Slice 4b): appends happen once per message at send/receive
// time; every later change is a guarded patch that never re-appends; on
// `stale-history` the conversation is reloaded from the DB and the operation
// is retried exactly once.

import type { History } from "../types";

export interface HistToken {
  epoch: number;
  rev: number;
}

/** Token for a never-loaded key on a fresh/upgraded DB (absent = 0). */
export const INITIAL_TOKEN: HistToken = { epoch: 0, rev: 0 };

export function isStaleHistoryError(e: unknown): boolean {
  return String(e).includes("stale-history");
}

/**
 * Merge race at startup: LAN events can append locally before the initial
 * load resolves. The result is the union by id; for equal ids the DB row
 * wins. DB entries keep their order, surviving local-only entries follow
 * ordered by `at`.
 */
export function mergeLoadedHistory(local: History, db: History): History {
  const merged: History = {};
  for (const key of new Set([...Object.keys(db), ...Object.keys(local)])) {
    const dbEntries = db[key] ?? [];
    const seen = new Set(dbEntries.map((e) => e.id));
    const localOnly = (local[key] ?? [])
      .filter((e) => !seen.has(e.id))
      .sort((a, b) => a.at - b.at);
    merged[key] = [...dbEntries, ...localOnly];
  }
  return merged;
}

/**
 * One-time localStorage→DB import decision. When the DB already reports the
 * migration flag, the snapshot is not even read. Returns the snapshot to
 * import, or null when there is nothing to do.
 */
export function planLegacyImport(
  legacyImported: boolean,
  read: () => History | null,
): History | null {
  if (legacyImported) return null;
  const snapshot = read();
  if (!snapshot || Object.keys(snapshot).length === 0) return null;
  return snapshot;
}

/**
 * Runs `op` with the token captured at call time. On `stale-history`
 * (someone deleted concurrently), the caller's `refresh` reloads fresh
 * tokens from the DB and the op runs once more with the new token. Any
 * other error — and a second consecutive staleness — rejects honestly.
 */
export async function withHistoryToken<T>(
  token: HistToken | undefined,
  op: (t: HistToken) => Promise<T>,
  refresh: () => Promise<HistToken>,
): Promise<T> {
  const first = token ?? INITIAL_TOKEN;
  try {
    return await op(first);
  } catch (e) {
    if (!isStaleHistoryError(e)) throw e;
  }
  return op(await refresh());
}

export function tokensFromRevs(
  revs: Record<string, number>,
  epoch: number,
): Record<string, HistToken> {
  const tokens: Record<string, HistToken> = {};
  for (const [key, rev] of Object.entries(revs)) {
    tokens[key] = { epoch, rev };
  }
  return tokens;
}

/** delete_all keeps per-key revs in the DB and bumps the global epoch. */
export function tokensAfterDeleteAll(
  tokens: Record<string, HistToken>,
  epoch: number,
): Record<string, HistToken> {
  const next: Record<string, HistToken> = {};
  for (const [key, t] of Object.entries(tokens)) {
    next[key] = { epoch, rev: t.rev };
  }
  return next;
}
