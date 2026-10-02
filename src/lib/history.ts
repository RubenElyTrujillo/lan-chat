import { invoke } from "@tauri-apps/api/core";
import type { History } from "../types";

const LEGACY_KEY = "lanchat.history.v1";

// Migración: historial de la era localStorage (antes de SQLite).
export function readLegacyHistory(): History {
  try {
    const raw = localStorage.getItem(LEGACY_KEY);
    return raw ? (JSON.parse(raw) as History) : {};
  } catch {
    return {};
  }
}

export async function loadHistory(): Promise<History> {
  return invoke<History>("load_history");
}

export async function saveHistory(history: History): Promise<void> {
  await invoke("save_history", { history });
}
