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

export function sendText(ip: string, texto: string): Promise<void> {
  return invoke("send_text", { ip, texto });
}

export function onMessage(handler: (msg: RawMessage) => void): Promise<() => void> {
  return listen<RawMessage>("message-received", (event) => handler(event.payload));
}
