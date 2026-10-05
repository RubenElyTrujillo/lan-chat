// Hub chat core: pure, DOM-free decision logic for conversations with paired
// browser contacts over the hub. No Tauri, no React — the wrappers in
// backend.ts reach the commands and App.tsx wires the results, so this runs
// under `node --test` (type stripping).
//
// Contract (mirrors src-tauri/src/hub/client.rs + inbox.rs):
// - Persisted contacts are `hub:<uuid>` keys (history_load `contacts`).
// - `hub_send_text(key, id, text)` answers Ok("sent") or exactly one of
//   Err("pair-required" | "peer-offline" | "hub-unavailable").
// - Inbound `hub-message-received` payloads are already committed natively:
//   the UI hydrates them with the WIRE id and must NOT append again.
//
// Identity rules: correlation between a live presence conn and a contact is
// DISPLAY grouping only, learned from a unique non-empty name match. Native
// authorization (grants + inbox gates) remains the single source of truth.

import type { DeviceState } from "../types";
import { hubDeviceFromPeer, hubPeerKey, type HubPeer } from "./hub-presence.ts";

// ── Keys ────────────────────────────────────────────────────────────────────

export function isContactKey(key: string): boolean {
  return key.startsWith("hub:") && key.length > "hub:".length;
}

// ── Routing decision ────────────────────────────────────────────────────────

export type SendRoute =
  | { kind: "hub" }
  | { kind: "lan"; device: DeviceState }
  /** Device row without an ip: the demo simulation's optimistic send. */
  | { kind: "optimistic"; device: DeviceState }
  | { kind: "unknown" };

export function routeSend(key: string, device: DeviceState | undefined): SendRoute {
  if (isContactKey(key)) return { kind: "hub" };
  if (!device) return { kind: "unknown" };
  return device.ip ? { kind: "lan", device } : { kind: "optimistic", device };
}

// ── Send outcome mapping (one honest system line per Err) ───────────────────

export type HubSendResult = { ok: true; status: string } | { ok: false; error: string };

export type HubSendOutcome = { kind: "sent" } | { kind: "failed"; systemText: string };

export type HubSendFailure = Extract<HubSendOutcome, { kind: "failed" }>;

export const HUB_SYSTEM_PAIR_REQUIRED = "Vinculá desde el navegador para conversar";
export const HUB_SYSTEM_PEER_OFFLINE =
  "El navegador no está conectado al hub ahora mismo; el mensaje no salió";
export const HUB_SYSTEM_HUB_UNAVAILABLE =
  "El hub no está disponible ahora mismo; el mensaje no salió";
export const HUB_SYSTEM_FILE_TOO_LARGE =
  "El archivo supera el máximo de 25 MB y no se envió.";
export const HUB_SYSTEM_UNKNOWN = "No se pudo enviar el mensaje";

export function hubSendOutcome(result: { ok: false; error: string }): HubSendFailure;
export function hubSendOutcome(result: HubSendResult): HubSendOutcome;
export function hubSendOutcome(result: HubSendResult): HubSendOutcome {
  if (result.ok) return { kind: "sent" };
  switch (result.error) {
    case "pair-required":
      return { kind: "failed", systemText: HUB_SYSTEM_PAIR_REQUIRED };
    case "peer-offline":
      return { kind: "failed", systemText: HUB_SYSTEM_PEER_OFFLINE };
    case "hub-unavailable":
      return { kind: "failed", systemText: HUB_SYSTEM_HUB_UNAVAILABLE };
    case "file-too-large":
      return { kind: "failed", systemText: HUB_SYSTEM_FILE_TOO_LARGE };
    default:
      return { kind: "failed", systemText: HUB_SYSTEM_UNKNOWN };
  }
}

// ── Binding learning (display grouping; auth stays native) ─────────────────

/**
 * Maps conn_id → contact key when the peer's name matches EXACTLY ONE
 * contact name. Duplicates and blank names never bind: names are not
 * identity, so an ambiguous match degrades to separate rows (current
 * presence behavior) instead of guessing.
 */
export function learnContactBindings(
  peers: HubPeer[],
  contacts: Record<string, string>,
): Map<string, string> {
  const keysByName = new Map<string, string[]>();
  for (const [key, rawName] of Object.entries(contacts)) {
    const name = rawName.trim();
    if (!name) continue;
    keysByName.set(name, [...(keysByName.get(name) ?? []), key]);
  }
  const bindings = new Map<string, string>();
  for (const peer of peers) {
    const name = peer.name.trim();
    if (!name) continue;
    const keys = keysByName.get(name);
    if (keys?.length === 1) bindings.set(peer.conn_id, keys[0]);
  }
  return bindings;
}

// ── Grant-commit reload signal ──────────────────────────────────────────────

/**
 * Conns whose grant was committed since the previous pairing snapshot. The
 * native side mints + persists the `hub:<uuid>` contact at commit time, but
 * the UI only sees it by re-reading `contacts` — without this signal a just
 * paired browser shows only its `hub-session:` row (tagged "Vinculado") and
 * clicking it lands on the presence-only detail instead of the conversation.
 */
export function newlyPairedConns(
  prev: ReadonlySet<string>,
  next: ReadonlySet<string>,
): string[] {
  return [...next].filter((conn) => !prev.has(conn));
}

// ── Minted-contact binding (reload delta) ───────────────────────────────────

/**
 * Binds ONE freshly paired conn to the ONE contact key that appeared in the
 * contacts reload triggered by that pairing. Native mints the `hub:<uuid>`
 * contact at grant-commit time, so the delta — not the display name — is the
 * authoritative conn→contact link. Name-based grouping cannot help when
 * several persisted contacts share the browser name (one is minted per hub
 * session), so anything ambiguous (≠1 fresh conn or ≠1 new key) stays
 * unbound and the session keeps its presence-only row.
 */
export function bindMintedContact(
  freshConns: readonly string[],
  before: ReadonlySet<string>,
  after: Readonly<Record<string, string>>,
): [conn: string, key: string] | null {
  if (freshConns.length !== 1) return null;
  const added = Object.keys(after).filter(
    (key) => isContactKey(key) && !before.has(key),
  );
  if (added.length !== 1) return null;
  return [freshConns[0], added[0]];
}

// ── Sidebar rows: contacts merged with live presence ────────────────────────

export interface HubRowsParams {
  contacts: Record<string, string>;
  peers: HubPeer[];
  pairedConnIds: ReadonlySet<string>;
  contactForConn: ReadonlyMap<string, string>;
}

export interface HubRows {
  rows: DeviceState[];
  /** Keys that render the "Vinculado" tag: bound contact keys + paired session keys. */
  pairedKeys: Set<string>;
}

/**
 * Contact keys become conversation rows (offline unless a live paired peer
 * is bound to them). A live peer WITH a grant and a known binding merges
 * into its contact row (single row, online); every other live peer keeps
 * the presence-only `hub-session:<conn_id>` row. Previews come later from
 * `history[row.key]` — the contact key IS the history key.
 */
export function buildHubRows(params: HubRowsParams): HubRows {
  const { contacts, peers, pairedConnIds, contactForConn } = params;
  const boundKeys = new Set<string>();
  for (const conn of pairedConnIds) {
    const key = contactForConn.get(conn);
    if (key && isContactKey(key)) boundKeys.add(key);
  }
  const rows: DeviceState[] = Object.entries(contacts).map(([key, rawName]) => ({
    key,
    name: rawName.trim(),
    kind: "hub" as const,
    online: boundKeys.has(key),
  }));
  const pairedKeys = new Set(boundKeys);
  for (const peer of peers) {
    const paired = pairedConnIds.has(peer.conn_id);
    if (paired && boundKeys.has(contactForConn.get(peer.conn_id) ?? "")) continue;
    rows.push(hubDeviceFromPeer(peer));
    if (paired) pairedKeys.add(hubPeerKey(peer.conn_id));
  }
  return { rows, pairedKeys };
}

export const HUB_PAIRED_TAG = "Vinculado";

export function isGrantBacked(key: string, pairedKeys: ReadonlySet<string>): boolean {
  return pairedKeys.has(key);
}

// ── Inbound hydrate ─────────────────────────────────────────────────────────

export interface RawHubMessage {
  key: string;
  name: string;
  text: string;
  id: string;
}

export function parseHubMessage(raw: unknown): RawHubMessage | null {
  if (typeof raw !== "object" || raw === null) return null;
  const m = raw as Record<string, unknown>;
  if (typeof m.key !== "string" || !isContactKey(m.key)) return null;
  if (typeof m.name !== "string") return null;
  if (typeof m.text !== "string" || m.text.length === 0) return null;
  if (typeof m.id !== "string" || m.id.length === 0) return null;
  return { key: m.key, name: m.name, text: m.text, id: m.id };
}

export interface HubInboundPlan {
  msg: RawHubMessage;
  /** Inbound rows are committed natively BEFORE the event: never append again. */
  persist: false;
  /** Contact name to learn when the key was unknown so far; null otherwise. */
  upsertName: string | null;
}

/**
 * Decides what to do with one `hub-message-received` payload. Malformed
 * payloads and non-contact keys are ignored (defensive: unpaired peers
 * cannot produce events natively, but the UI never trusts that alone).
 */
export function planHubInbound(
  raw: unknown,
  contacts: Record<string, string>,
): HubInboundPlan | null {
  const msg = parseHubMessage(raw);
  if (!msg) return null;
  const known = Object.prototype.hasOwnProperty.call(contacts, msg.key);
  return { msg, persist: false, upsertName: known ? null : msg.name.trim() };
}

// ── Persist flag ────────────────────────────────────────────────────────────

/** Appends happen exactly once per message; hydrate paths opt out explicitly. */
export function planEntryCommit(opts?: { persist?: boolean }): boolean {
  return opts?.persist !== false;
}

// ── Inbound file hydrate ────────────────────────────────────────────────────

export interface RawHubFile {
  key: string;
  name: string;
  path: string;
  size: number;
  id: string;
}

export function parseHubFile(raw: unknown): RawHubFile | null {
  if (typeof raw !== "object" || raw === null) return null;
  const f = raw as Record<string, unknown>;
  if (typeof f.key !== "string" || !isContactKey(f.key)) return null;
  if (typeof f.name !== "string" || f.name.length === 0) return null;
  if (typeof f.path !== "string" || f.path.length === 0) return null;
  if (typeof f.size !== "number") return null;
  if (typeof f.id !== "string" || f.id.length === 0) return null;
  return { key: f.key, name: f.name, path: f.path, size: f.size, id: f.id };
}

export interface HubFileInboundPlan {
  file: RawHubFile;
  /** Inbound file rows are committed natively BEFORE the event: never append again. */
  persist: false;
  /** Contact name to learn when the key was unknown so far; null otherwise. */
  upsertName: string | null;
}

/** Same contract as planHubInbound, for `hub-file-received` payloads. */
export function planHubFileInbound(
  raw: unknown,
  contacts: Record<string, string>,
): HubFileInboundPlan | null {
  const file = parseHubFile(raw);
  if (!file) return null;
  const known = Object.prototype.hasOwnProperty.call(contacts, file.key);
  return { file, persist: false, upsertName: known ? null : file.name.trim() };
}

// ── Inbound file failure notice ─────────────────────────────────────────────

/**
 * Honest generic copy per receive-failure reason. The notice row is appended
 * by the UI with the default persist (planEntryCommit), so the text stands
 * alone without sender-side context.
 */
export function hubFileErrorNotice(name: string, reason: string): string {
  const cause =
    reason === "file-too-large"
      ? "el archivo supera el máximo de 25 MB"
      : reason === "stage-failed"
        ? "no se pudo guardar el archivo"
        : "error desconocido";
  return `No se pudo recibir ${name}: ${cause}`;
}
