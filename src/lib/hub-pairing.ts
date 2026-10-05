// Hub browser-pairing UI: types, payload validation, the query/listen
// race-safe lifecycle, countdown helpers and cancellation bookkeeping.
// Pure and DOM-free so it runs under `node --test` (type stripping) with an
// injected adapter; App.tsx supplies the Tauri-backed adapter.
//
// Contract (mirrors src-tauri/src/hub/pairing_wire.rs):
// - The snapshot is `{ pending: [...], paired: [...] }`. A pending entry is
//   (conn_id, name, req_id, code, expires_in_ms); paired is (conn_id, req_id).
// - The 8-digit code exists ONLY in this snapshot and the desktop UI: it is
//   never logged, copied, or relayed anywhere by the frontend.
// - Snapshots replace the previous one wholesale; an empty snapshot means no
//   live pairing state. Correlation is ALWAYS the hub conn id: names and sids
//   never identify, merge or authorize anything.

// Same charset/bounds rule as presence sids and pairing req ids.
import { isValidSid, MAX_CONN_ID_LEN, MAX_PEER_NAME_LEN } from "./hub-presence.ts";

export interface HubPendingPairing {
  conn_id: string;
  name: string;
  req_id: string;
  code: string;
  expires_in_ms: number;
}

export interface HubPairedPairing {
  conn_id: string;
  req_id: string;
}

export interface HubPairingSnapshot {
  pending: HubPendingPairing[];
  paired: HubPairedPairing[];
}

// Mirrors the backend: at most 2 live pendings; grants bounded by the
// presence peer registry (32).
export const MAX_PENDING = 2;
export const MAX_PAIRED = 32;
export const CODE_LEN = 8;
export const MAX_NAME_LEN = MAX_PEER_NAME_LEN;

function truncateChars(s: string, max: number): string {
  return [...s].slice(0, max).join("");
}

function isBoundedId(v: unknown): v is string {
  return typeof v === "string" && isValidSid(v);
}

// The code is ALWAYS exactly 8 ASCII digits; anything else is invalid.
function isCode(v: unknown): v is string {
  return typeof v === "string" && new RegExp(`^\\d{${CODE_LEN}}$`).test(v);
}

function parsePending(entry: unknown): HubPendingPairing | null {
  if (typeof entry !== "object" || entry === null) return null;
  const e = entry as Record<string, unknown>;
  if (typeof e.conn_id !== "string" || e.conn_id.length === 0) return null;
  if (e.conn_id.length > MAX_CONN_ID_LEN) return null;
  if (!isBoundedId(e.req_id)) return null;
  if (!isCode(e.code)) return null;
  if (typeof e.expires_in_ms !== "number" || !Number.isSafeInteger(e.expires_in_ms)) return null;
  if (e.expires_in_ms < 0) return null;
  return {
    conn_id: e.conn_id,
    name: typeof e.name === "string" ? truncateChars(e.name, MAX_NAME_LEN) : "",
    req_id: e.req_id,
    code: e.code,
    expires_in_ms: e.expires_in_ms,
  };
}

function parsePaired(entry: unknown): HubPairedPairing | null {
  if (typeof entry !== "object" || entry === null) return null;
  const e = entry as Record<string, unknown>;
  if (typeof e.conn_id !== "string" || e.conn_id.length === 0) return null;
  if (e.conn_id.length > MAX_CONN_ID_LEN) return null;
  if (!isBoundedId(e.req_id)) return null;
  return { conn_id: e.conn_id, req_id: e.req_id };
}

export function parseHubPairing(payload: unknown): HubPairingSnapshot | null {
  if (typeof payload !== "object" || payload === null) return null;
  const p = payload as Record<string, unknown>;
  if (!Array.isArray(p.pending) || !Array.isArray(p.paired)) return null;
  const pending: HubPendingPairing[] = [];
  for (const entry of p.pending) {
    const item = parsePending(entry);
    if (item) {
      pending.push(item);
      if (pending.length >= MAX_PENDING) break;
    }
  }
  const paired: HubPairedPairing[] = [];
  for (const entry of p.paired) {
    const item = parsePaired(entry);
    if (item) {
      paired.push(item);
      if (paired.length >= MAX_PAIRED) break;
    }
  }
  return { pending, paired };
}

export const EMPTY_PAIRING: HubPairingSnapshot = { pending: [], paired: [] };

// ── Countdown helpers (monotonic local receipt timestamp) ───────────────────

// Anchor: the moment the snapshot was received locally + the remaining ms.
export function receiptDeadline(expiresInMs: number, nowMs: number): number {
  return nowMs + expiresInMs;
}

// Never negative: a past deadline is simply expired.
export function remainingMs(deadlineMs: number, nowMs: number): number {
  return Math.max(0, deadlineMs - nowMs);
}

export function isExpired(deadlineMs: number, nowMs: number): boolean {
  return remainingMs(deadlineMs, nowMs) <= 0;
}

export function formatCountdown(ms: number): string {
  const total = Math.max(0, Math.ceil(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

// ── Cancellation bookkeeping ─────────────────────────────────────────────────

// The UI captured (conn_id, req_id) at click time. When the cancel promise
// resolves — however late — only THAT exact entry leaves the view. A new
// request that replaced it (same conn, fresh req id) survives untouched.
export function applyCancelResult(
  pending: HubPendingPairing[],
  connId: string,
  reqId: string,
): HubPendingPairing[] {
  return pending.filter((p) => !(p.conn_id === connId && p.req_id === reqId));
}

// ── Paired view (for HubPeerDetail) ─────────────────────────────────────────

// Honest vocabulary: paired means authorized under the CURRENT hub session.
// Chat over the hub is live (hub_send_text); the short tag says exactly that.
export const HUB_PAIRED_LABEL = "Vinculado";

export function pairedConnIds(snapshot: HubPairingSnapshot): Set<string> {
  return new Set(snapshot.paired.map((g) => g.conn_id));
}

// ── Lifecycle ────────────────────────────────────────────────────────────────

export interface HubPairingAdapter {
  get(): Promise<HubPairingSnapshot>;
  on(handler: (snapshot: HubPairingSnapshot) => void): Promise<() => void>;
}

export interface HubPairingHandlers {
  onSnapshot: (snapshot: HubPairingSnapshot) => void;
  onUnavailable?: (err: unknown) => void;
}

// Same race-safe lifecycle as trackHubStatus/trackHubPresence: subscribe
// first, then ask for the initial snapshot. Any parsed event seen while the
// query is in flight marks the snapshot stale. The returned function is safe
// to call early (before the subscription promise resolves) and twice
// (StrictMode remounts).
export function trackHubPairing(
  adapter: HubPairingAdapter,
  handlers: HubPairingHandlers,
): () => void {
  let disposed = false;
  let unlisten: (() => void) | null = null;
  let queryInFlight = false;
  let eventSinceQueryStart = false;

  const handleEvent = (raw: unknown) => {
    if (disposed) return;
    const snapshot = parseHubPairing(raw);
    if (!snapshot) return;
    if (queryInFlight) eventSinceQueryStart = true;
    handlers.onSnapshot(snapshot);
  };

  const start = async () => {
    let off: () => void;
    try {
      off = await adapter.on(handleEvent);
    } catch (err) {
      if (!disposed) handlers.onUnavailable?.(err);
      return;
    }
    if (disposed) {
      off();
      return;
    }
    unlisten = off;
    queryInFlight = true;
    eventSinceQueryStart = false;
    let snapshot: HubPairingSnapshot | null = null;
    try {
      snapshot = parseHubPairing(await adapter.get());
    } catch (err) {
      if (!disposed) handlers.onUnavailable?.(err);
      return;
    }
    queryInFlight = false;
    if (disposed) return;
    if (snapshot && !eventSinceQueryStart) handlers.onSnapshot(snapshot);
  };

  void start();

  return () => {
    if (disposed) return;
    disposed = true;
    unlisten?.();
    unlisten = null;
  };
}
