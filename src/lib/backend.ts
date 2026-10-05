import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { HubStatus } from "./hub-status";
import type { HubPresenceSnapshot } from "./hub-presence";
import type { HubPairingSnapshot } from "./hub-pairing";

export interface RawDevice {
  name: string;
  ip: string;
  service: string;
}

export interface RawMessage {
  from: string;
  text: string;
  id?: string;
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function demoRequested(): boolean {
  return new URLSearchParams(window.location.search).has("demo");
}

export function discover(): Promise<RawDevice[]> {
  return invoke<RawDevice[]>("discover_devices");
}

export function sendText(
  ip: string,
  pin: string,
  texto: string,
  id: string,
): Promise<string> {
  return invoke<string>("send_text", { ip, pin, texto, id });
}

export function sendFile(
  ip: string,
  pin: string,
  path: string,
  id: string,
): Promise<string> {
  return invoke<string>("send_file", { ip, pin, path, id });
}

export function regenerateOwnPin(): Promise<string> {
  return invoke("regenerate_own_pin");
}

export function pairVerify(ip: string, pin: string): Promise<void> {
  return invoke("pair_verify", { ip, pin });
}

// PINs de dispositivos emparejados (config local, no historial).
const PINS_KEY = "lanchat.pins.v1";

export function getPinFor(key: string): string {
  try {
    const all = JSON.parse(localStorage.getItem(PINS_KEY) ?? "{}") as Record<
      string,
      string
    >;
    return all[key] ?? "";
  } catch {
    return "";
  }
}

export function setPinFor(key: string, pin: string): void {
  try {
    const all = JSON.parse(localStorage.getItem(PINS_KEY) ?? "{}") as Record<
      string,
      string
    >;
    all[key] = pin;
    localStorage.setItem(PINS_KEY, JSON.stringify(all));
  } catch {
    /* almacenamiento no disponible */
  }
}

export function sendAck(ip: string, payload: string): Promise<void> {
  return invoke("send_ack", { ip, payload });
}

export function sendPairRequest(ip: string): Promise<void> {
  return invoke("send_ack", { ip, payload: JSON.stringify({ kind: "pair-request" }) });
}

export interface RawPairRequest {
  from: string;
  code: string;
}

export function onPairRequest(
  handler: (r: RawPairRequest) => void,
): Promise<() => void> {
  return listen<RawPairRequest>("pair-request", (event) => handler(event.payload));
}

export interface RawPairDone {
  from: string;
  code: string;
}

export function onPairDone(handler: (r: RawPairDone) => void): Promise<() => void> {
  return listen<RawPairDone>("pair-done", (event) => handler(event.payload));
}

export function probePort(ip: string, port = 8787): Promise<boolean> {
  return invoke<boolean>("probe_port", { ip, port });
}

export interface RawReadAck {
  from: string;
  ids: string[];
}

export function onReadAck(handler: (ack: RawReadAck) => void): Promise<() => void> {
  return listen<RawReadAck>("read-ack", (event) => handler(event.payload));
}

export function getDownloadFolder(): Promise<string> {
  return invoke("get_download_folder");
}

export function setDownloadFolder(path: string): Promise<void> {
  return invoke("set_download_folder", { path });
}

export interface RawFile {
  from: string;
  name: string;
  path: string;
  size: number;
  id?: string;
}

export function onMessage(handler: (msg: RawMessage) => void): Promise<() => void> {
  return listen<RawMessage>("message-received", (event) => handler(event.payload));
}

export function onFile(handler: (file: RawFile) => void): Promise<() => void> {
  return listen<RawFile>("file-received", (event) => handler(event.payload));
}

// Estado del enlace con el hub: consulta inicial (`hub_status`) y luego
// actualizaciones push (`hub-state`), ambas con el mismo payload HubStatus.
export function hubStatus(): Promise<HubStatus> {
  return invoke<HubStatus>("hub_status");
}

export function onHubState(handler: (status: HubStatus) => void): Promise<() => void> {
  return listen<HubStatus>("hub-state", (event) => handler(event.payload));
}

// Presencia del hub: consulta inicial (`hub_presence`) y luego push
// (`hub-presence`), ambas con el mismo snapshot de pares.
export function hubPresence(): Promise<HubPresenceSnapshot> {
  return invoke<HubPresenceSnapshot>("hub_presence");
}

export function onHubPresence(
  handler: (snapshot: HubPresenceSnapshot) => void,
): Promise<() => void> {
  return listen<HubPresenceSnapshot>("hub-presence", (event) => handler(event.payload));
}

// Emparejamiento de navegadores vía hub: consulta inicial (`hub_pairing`) y
// luego push (`hub-pairing`), ambas con el mismo snapshot. Los códigos de 8
// dígitos viven SOLO en este snapshot y la UI del escritorio: nunca viajan
// por el relay, ni se registran, ni salen por la red.
export function hubPairing(): Promise<HubPairingSnapshot> {
  return invoke<HubPairingSnapshot>("hub_pairing");
}

export function onHubPairing(
  handler: (snapshot: HubPairingSnapshot) => void,
): Promise<() => void> {
  return listen<HubPairingSnapshot>("hub-pairing", (event) => handler(event.payload));
}

// Cancel nativo (botón de la UI): limpia SOLO el pending propio indicado;
// no envía nada por la red. Devuelve si había un pending coincidente.
export function hubCancelPairing(connId: string, reqId: string): Promise<boolean> {
  return invoke<boolean>("hub_cancel_pairing", { connId, reqId });
}

// Chat con contactos del hub: el backend resuelve la clave `hub:<uuid>` a la
// sesión viva y acusa "sent", o falla con pair-required | peer-offline |
// hub-unavailable. Sin cola: el frontend es dueño del reintento.
export function hubSendText(key: string, id: string, text: string): Promise<string> {
  return invoke<string>("hub_send_text", { key, id, text });
}

export interface RawHubMessage {
  key: string;
  name: string;
  text: string;
  id: string;
}

// Mensaje entrante del hub: ya fue persistido nativamente ANTES del evento
// (la fila existe en la DB); la UI solo hidrata con el id del wire.
export function onHubMessage(
  handler: (msg: RawHubMessage) => void,
): Promise<() => void> {
  return listen<RawHubMessage>("hub-message-received", (event) => handler(event.payload));
}

// Archivo hacia un contacto del hub: mismo contrato que hub_send_text pero
// con path local; falla además con file-too-large si excede el tope nativo.
export function hubSendFile(key: string, id: string, path: string): Promise<string> {
  return invoke<string>("hub_send_file", { key, id, path });
}

export interface RawHubFile {
  key: string;
  name: string;
  path: string;
  size: number;
  id: string;
}

// Archivo entrante del hub: ya fue persistido nativamente ANTES del evento
// (la fila existe en la DB); la UI solo hidrata con el id del wire.
export function onHubFile(handler: (file: RawHubFile) => void): Promise<() => void> {
  return listen<RawHubFile>("hub-file-received", (event) => handler(event.payload));
}

export interface RawHubFileError {
  key: string;
  name: string;
  reason: string;
}

// Recepción de archivo fallida: el nombre viene del wire y la razón ya fue
// resuelta nativamente; la UI agrega la fila de sistema persistida.
export function onHubFileError(
  handler: (err: RawHubFileError) => void,
): Promise<() => void> {
  return listen<RawHubFileError>("hub-file-error", (event) => handler(event.payload));
}
