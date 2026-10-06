// Clipboard share core: pure, DOM-free decision logic for the shared
// clipboard feature. No Tauri, no React — runs under `node --test`
// (type stripping).
//
// Contract (mirrors src-tauri/src/clipboard.rs + hub/client.rs):
// - `read_clipboard()` → Ok(text) | Err("invalid-text") (empty or over cap).
// - `write_clipboard(text)` → Ok(()) | Err.
// - `hub_send_clipboard(key, id, text)` → Ok("sent") | Err(
//   "pair-required" | "peer-offline" | "hub-unavailable" | "invalid-text").
// - Inbound `hub-clipboard-received` payloads are already committed natively
//   BEFORE the event: the UI hydrates with the WIRE id and must NOT append.
//
// The native cap counts CHARS (not bytes): MAX_CLIPBOARD_TEXT = 64_000.

export const MAX_CLIPBOARD_TEXT = 64_000;

// ── Bounds ──────────────────────────────────────────────────────────────────

/** Mirrors native `bound_clipboard_text`: non-blank (no whitespace-only) and ≤ MAX chars. */
export function isValidClipboardText(text: string): boolean {
  return text.trim().length > 0 && [...text].length <= MAX_CLIPBOARD_TEXT;
}

// ── Target resolution (current pairing + presence) ──────────────────────────

export interface ClipboardTargetOption {
  key: string;
  name: string;
  online: boolean;
  /** Which transport carries the send for this option. */
  route: "hub" | "lan";
}

/** LAN device row the shortcut may offer (hasPin = stored pairing pin). */
export interface LanTargetDevice {
  key: string;
  name: string;
  online: boolean;
  hasPin: boolean;
}

export type ClipboardTarget =
  | { kind: "none"; reason: "no-paired" | "all-offline" }
  | { kind: "single"; key: string; name: string; route: "hub" | "lan" }
  | { kind: "pick"; options: ClipboardTargetOption[] };

/**
 * Picks where a clipboard send should go, from the CURRENT contact rows plus
 * the LAN devices seen in the last scan. Hub contacts are deduped by key
 * (first name wins); LAN devices only qualify when online WITH a stored pin.
 * Online targets are preferred: exactly one online → single; several → picker
 * (online only); none online → none/all-offline so the toast can be honest.
 */
export function resolveTarget(
  contacts: ReadonlyArray<{ key: string; name: string }>,
  onlineKeys: ReadonlySet<string>,
  lanDevices: ReadonlyArray<LanTargetDevice> = [],
): ClipboardTarget {
  const deduped: ClipboardTargetOption[] = [];
  const seen = new Set<string>();
  for (const c of contacts) {
    if (!c.key.startsWith("hub:")) continue;
    if (seen.has(c.key)) continue;
    const name = c.name.trim();
    if (!name) continue;
    seen.add(c.key);
    deduped.push({ key: c.key, name, online: onlineKeys.has(c.key), route: "hub" });
  }
  for (const d of lanDevices) {
    const name = d.name.trim();
    if (!name || !d.hasPin) continue;
    if (seen.has(d.key)) continue;
    seen.add(d.key);
    deduped.push({ key: d.key, name, online: d.online, route: "lan" });
  }
  if (deduped.length === 0) return { kind: "none", reason: "no-paired" };
  const online = deduped.filter((o) => o.online);
  if (online.length === 1) {
    return {
      kind: "single",
      key: online[0].key,
      name: online[0].name,
      route: online[0].route,
    };
  }
  if (online.length > 1) return { kind: "pick", options: online };
  return { kind: "none", reason: "all-offline" };
}

// ── Toast copy (honest Spanish) ─────────────────────────────────────────────

export const CLIP_EMPTY_OR_LONG = "Portapapeles vacío o demasiado largo";
export const RESOLVE_NO_PAIRED = "Todavía no hay navegadores vinculados";
export const RESOLVE_ALL_OFFLINE = "Nadie vinculado está en línea";
export const SEND_PAIR_REQUIRED = "Vinculá el navegador desde la lista para enviarle el portapapeles";
export const SEND_PEER_OFFLINE = "El contacto no está conectado al hub; el portapapeles no salió";
export const SEND_HUB_UNAVAILABLE =
  "El hub no está disponible ahora mismo; el portapapeles no salió";
export const SEND_UNKNOWN = "No se pudo enviar el portapapeles";
export const WRITE_COPY_FAILED = "No se pudo copiar automáticamente — tocá el mensaje";

/** Toast for an unresolvable target; null when the flow can proceed. */
export function planClipboardResolve(target: ClipboardTarget): string | null {
  if (target.kind !== "none") return null;
  return target.reason === "no-paired" ? RESOLVE_NO_PAIRED : RESOLVE_ALL_OFFLINE;
}

export type ClipboardSendResult = { ok: true } | { ok: false; error: string };

export type ClipboardSendOutcome =
  | { kind: "sent"; toast: string }
  | { kind: "failed"; toast: string };

/** Maps one hub_send_clipboard result to its honest toast. */
export function planClipboardSend(
  result: ClipboardSendResult,
  name: string,
): ClipboardSendOutcome {
  if (result.ok) return { kind: "sent", toast: `Enviado a ${name}` };
  switch (result.error) {
    case "invalid-text":
      return { kind: "failed", toast: CLIP_EMPTY_OR_LONG };
    case "pair-required":
      return { kind: "failed", toast: SEND_PAIR_REQUIRED };
    case "peer-offline":
      return { kind: "failed", toast: SEND_PEER_OFFLINE };
    case "hub-unavailable":
      return { kind: "failed", toast: SEND_HUB_UNAVAILABLE };
    default:
      return { kind: "failed", toast: SEND_UNKNOWN };
  }
}

// ── Inbound hydrate ─────────────────────────────────────────────────────────

export interface RawHubClipboard {
  key: string;
  name: string;
  text: string;
  id: string;
}

export interface HubClipboardInboundPlan {
  msg: RawHubClipboard;
  /** Inbound rows are committed natively BEFORE the event: never append again. */
  persist: false;
  toastCopy: string;
}

function parseHubClipboard(raw: unknown): RawHubClipboard | null {
  if (typeof raw !== "object" || raw === null) return null;
  const m = raw as Record<string, unknown>;
  if (typeof m.key !== "string" || !m.key.startsWith("hub:")) return null;
  if (typeof m.name !== "string") return null;
  if (typeof m.text !== "string" || !isValidClipboardText(m.text)) return null;
  if (typeof m.id !== "string" || m.id.length === 0) return null;
  return { key: m.key, name: m.name, text: m.text, id: m.id };
}

/**
 * Decides what to do with one `hub-clipboard-received` payload: hydrate the
 * entry with the wire id (persist:false), copy to the local clipboard and
 * toast who it came from. Malformed payloads are ignored defensively.
 */
export function planHubClipboardInbound(raw: unknown): HubClipboardInboundPlan | null {
  const msg = parseHubClipboard(raw);
  if (!msg) return null;
  const sender = msg.name.trim();
  return {
    msg,
    persist: false,
    toastCopy: sender ? `Copiado de ${sender}` : "Copiado del portapapeles",
  };
}

// ── LAN inbound clipboard ────────────────────────────────────────────────────

export interface LanClipboardInboundPlan {
  toast: string;
  /**
   * The LAN receiver emits BOTH message-received and lan-clipboard-received:
   * the conversation entry is added from message-received ONLY. This event
   * never appends, so the entry can't duplicate.
   */
  append: false;
}

/**
 * Decides what to do with one `lan-clipboard-received` payload: toast who it
 * came from and nothing else — the native side already wrote the local
 * clipboard and message-received already added the entry. Malformed payloads
 * are ignored defensively.
 */
export function planLanClipboardReceived(raw: unknown): LanClipboardInboundPlan | null {
  if (typeof raw !== "object" || raw === null) return null;
  const m = raw as Record<string, unknown>;
  if (typeof m.from !== "string") return null;
  if (typeof m.text !== "string" || m.text.length === 0) return null;
  const sender = m.from.trim();
  return {
    toast: sender ? `Copiado de ${sender}` : "Copiado del portapapeles",
    append: false,
  };
}
