// Hub link status: types, payload validation and the query/listen race-safe
// lifecycle. Pure and DOM-free so it runs under `node --test` (type stripping)
// with an injected adapter; App.tsx supplies the Tauri-backed adapter.

export type HubPhase = "connecting" | "connected" | "backoff" | "disabled";

export interface HubStatus {
  url: string;
  connected: boolean;
  phase: HubPhase;
  session_id?: string;
  reason?: string;
}

export const HUB_PHASES: readonly HubPhase[] = [
  "connecting",
  "connected",
  "backoff",
  "disabled",
];

export function parseHubStatus(payload: unknown): HubStatus | null {
  if (typeof payload !== "object" || payload === null) return null;
  const p = payload as Record<string, unknown>;
  if (typeof p.url !== "string" || typeof p.connected !== "boolean") return null;
  if (typeof p.phase !== "string" || !HUB_PHASES.includes(p.phase as HubPhase)) return null;
  return {
    url: p.url,
    connected: p.connected,
    phase: p.phase as HubPhase,
    ...(typeof p.session_id === "string" ? { session_id: p.session_id } : {}),
    ...(typeof p.reason === "string" ? { reason: p.reason } : {}),
  };
}

export interface HubStatusAdapter {
  get(): Promise<HubStatus>;
  on(handler: (status: HubStatus) => void): Promise<() => void>;
}

export type HubUiState =
  | { kind: "connecting" }
  | { kind: "status"; status: HubStatus }
  | { kind: "unavailable" }
  | { kind: "demo" };

export interface HubStatusHandlers {
  onStatus: (status: HubStatus) => void;
  onUnavailable?: (err: unknown) => void;
}

// Subscribes first, then asks for the initial snapshot. Any parsed event seen
// while the query is in flight marks the snapshot stale, so an old answer can
// never override a newer event. The returned function is safe to call early
// (before the subscription promise resolves) and twice (StrictMode remounts).
export function trackHubStatus(
  adapter: HubStatusAdapter,
  handlers: HubStatusHandlers,
): () => void {
  let disposed = false;
  let unlisten: (() => void) | null = null;
  let queryInFlight = false;
  let eventSinceQueryStart = false;

  const handleEvent = (raw: unknown) => {
    if (disposed) return;
    const status = parseHubStatus(raw);
    if (!status) return;
    if (queryInFlight) eventSinceQueryStart = true;
    handlers.onStatus(status);
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
    let snapshot: HubStatus | null = null;
    try {
      snapshot = parseHubStatus(await adapter.get());
    } catch (err) {
      if (!disposed) handlers.onUnavailable?.(err);
      return;
    }
    queryInFlight = false;
    if (disposed) return;
    if (snapshot && !eventSinceQueryStart) handlers.onStatus(snapshot);
  };

  void start();

  return () => {
    if (disposed) return;
    disposed = true;
    unlisten?.();
    unlisten = null;
  };
}

// UI vocabulary: one Spanish word per state, so state never relies on color
// alone; tooltips stay honest (a connected hub is a link, not chat).
export function hubStatusLabel(state: HubUiState): string {
  switch (state.kind) {
    case "demo":
      return "Hub · demo";
    case "unavailable":
      return "Hub no disponible";
    case "connecting":
      return "Conectando al hub";
    case "status":
      switch (state.status.phase) {
        case "connected":
          return "Hub conectado";
        case "connecting":
          return "Conectando al hub";
        case "backoff":
          return "Reintentando hub";
        case "disabled":
          return "Hub no configurado";
      }
  }
}

export type HubTone = "ok" | "wait" | "retry" | "off" | "demo";

export function hubStatusTone(state: HubUiState): HubTone {
  switch (state.kind) {
    case "demo":
      return "demo";
    case "unavailable":
      return "off";
    case "connecting":
      return "wait";
    case "status":
      switch (state.status.phase) {
        case "connected":
          return "ok";
        case "connecting":
          return "wait";
        case "backoff":
          return "retry";
        case "disabled":
          return "off";
      }
  }
}

export function hubStatusTitle(state: HubUiState): string {
  switch (state.kind) {
    case "demo":
      return "Demostración: sin hub real; nada se conecta";
    case "unavailable":
      return "El hub no está accesible ahora mismo";
    case "connecting":
      return "Buscando el hub en la red";
    case "status":
      switch (state.status.phase) {
        case "connected":
          return "Enlace con el hub activo; el chat sigue siendo local";
        case "connecting":
          return "Estableciendo el enlace con el hub";
        case "backoff":
          return "Sin enlace con el hub; reintento automático en curso";
        case "disabled":
          return "Hub no configurado — modo red local. Conectá un hub con LANCHAT_HUB_URL.";
      }
  }
}
