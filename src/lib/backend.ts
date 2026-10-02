import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

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

export function getOwnPin(): Promise<string> {
  return invoke("get_own_pin");
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
