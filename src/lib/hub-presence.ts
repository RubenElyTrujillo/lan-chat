// Hub presence: types, payload validation, the query/listen race-safe
// lifecycle and the honest UI vocabulary for hub browser peers. Pure and
// DOM-free so it runs under `node --test` (type stripping) with an injected
// adapter; App.tsx supplies the Tauri-backed adapter.
//
// Contract (mirrors src-tauri/src/hub/presence.rs):
// - A peer is keyed by its hub conn id and shows only for kind "web".
// - `sid` is an UNTRUSTED HINT: it never authorizes or merges anything.
//   Duplicate names/sids stay separate entries.
// - Snapshots replace the previous one wholesale; an empty snapshot means
//   every hub session left (or the hub reset).

import type { DeviceState } from "../types";

export interface HubPeer {
  conn_id: string;
  name: string;
  kind: string;
  sid?: string;
  caps: string[];
}

export interface HubPresenceSnapshot {
  peers: HubPeer[];
}

export const MAX_PEERS = 32;
export const MAX_CONN_ID_LEN = 64;
export const MAX_PEER_NAME_LEN = 48;
export const MAX_SID_LEN = 64;
export const MAX_CAPS = 8;
export const MAX_CAP_LEN = 32;

// sid validity, mirroring the hub: 1..=64 chars, [A-Za-z0-9_-] only.
export function isValidSid(sid: string): boolean {
  return (
    sid.length >= 1 &&
    sid.length <= MAX_SID_LEN &&
    /^[A-Za-z0-9_-]+$/.test(sid)
  );
}

function truncateChars(s: string, max: number): string {
  return [...s].slice(0, max).join("");
}

function parseCaps(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((c): c is string => typeof c === "string")
    .map((c) => c.trim())
    .filter((c) => c.length > 0 && [...c].length <= MAX_CAP_LEN)
    .slice(0, MAX_CAPS);
}

export function parseHubPeer(entry: unknown): HubPeer | null {
  if (typeof entry !== "object" || entry === null) return null;
  const e = entry as Record<string, unknown>;
  if (typeof e.conn_id !== "string" || e.conn_id.length === 0) return null;
  if (e.conn_id.length > MAX_CONN_ID_LEN) return null;
  if (typeof e.kind !== "string" || e.kind !== "web") return null;
  const peer: HubPeer = {
    conn_id: e.conn_id,
    name: typeof e.name === "string" ? truncateChars(e.name, MAX_PEER_NAME_LEN) : "",
    kind: e.kind,
    caps: parseCaps(e.caps),
  };
  if (typeof e.sid === "string" && isValidSid(e.sid)) peer.sid = e.sid;
  return peer;
}

export function parseHubPresence(payload: unknown): HubPresenceSnapshot | null {
  if (typeof payload !== "object" || payload === null) return null;
  const p = payload as Record<string, unknown>;
  if (!Array.isArray(p.peers)) return null;
  const peers: HubPeer[] = [];
  for (const entry of p.peers) {
    const peer = parseHubPeer(entry);
    if (peer) {
      peers.push(peer);
      if (peers.length >= MAX_PEERS) break;
    }
  }
  return { peers };
}

// List key: per hub connection, never the persisted sid (that prefix is
// reserved for future history/auth and must not collide with sessions).
export function hubPeerKey(connId: string): string {
  return `hub-session:${connId}`;
}

export function isHubKey(key: string): boolean {
  return key.startsWith("hub-session:");
}

export function hubConnIdFromKey(key: string): string | null {
  if (!isHubKey(key)) return null;
  return key.slice("hub-session:".length);
}

export interface HubPresenceAdapter {
  get(): Promise<HubPresenceSnapshot>;
  on(handler: (snapshot: HubPresenceSnapshot) => void): Promise<() => void>;
}

export interface HubPresenceHandlers {
  onPeers: (peers: HubPeer[]) => void;
  onUnavailable?: (err: unknown) => void;
}

// Same race-safe lifecycle as trackHubStatus: subscribe first, then ask for
// the initial snapshot. Any event seen while the query is in flight marks the
// snapshot stale. The returned function is safe to call early (before the
// subscription promise resolves) and twice (StrictMode remounts).
export function trackHubPresence(
  adapter: HubPresenceAdapter,
  handlers: HubPresenceHandlers,
): () => void {
  let disposed = false;
  let unlisten: (() => void) | null = null;
  let queryInFlight = false;
  let eventSinceQueryStart = false;

  const handleEvent = (raw: unknown) => {
    if (disposed) return;
    const snapshot = parseHubPresence(raw);
    if (!snapshot) return;
    if (queryInFlight) eventSinceQueryStart = true;
    handlers.onPeers(snapshot.peers);
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
    let snapshot: HubPresenceSnapshot | null = null;
    try {
      snapshot = parseHubPresence(await adapter.get());
    } catch (err) {
      if (!disposed) handlers.onUnavailable?.(err);
      return;
    }
    queryInFlight = false;
    if (disposed) return;
    if (snapshot && !eventSinceQueryStart) handlers.onPeers(snapshot.peers);
  };

  void start();

  return () => {
    if (disposed) return;
    disposed = true;
    unlisten?.();
    unlisten = null;
  };
}

// ── UI vocabulary ───────────────────────────────────────────────────────────

// Anonymous browser sessions still need an honest name; "Navegador" says what
// it is without inventing an identity.
export function hubPeerName(peer: HubPeer): string {
  const name = peer.name.trim();
  return name.length > 0 ? name : "Navegador";
}

export const HUB_PEER_PREVIEW = "Vinculación web pendiente";

export function hubDeviceFromPeer(peer: HubPeer): DeviceState {
  // El nombre viaja crudo (trim): la UI aplica el resguardo "Navegador" al
  // renderizar, así una sesión anónima no dice "Navegador" dos veces.
  return {
    key: hubPeerKey(peer.conn_id),
    name: peer.name.trim(),
    kind: "hub",
    online: true,
  };
}

// LAN keeps its order (already sorted by App); hub peers follow, by name.
export function mergeDevices(lan: DeviceState[], hub: DeviceState[]): DeviceState[] {
  return [...lan, ...[...hub].sort((a, b) => a.name.localeCompare(b.name))];
}
