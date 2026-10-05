import { invoke } from "@tauri-apps/api/core";
import type { History } from "../types";
import type { HistToken } from "./history-core";

export type { HistToken } from "./history-core";

export interface HistoryLoad {
  history: History;
  epoch: number;
  /** Per-conversation revision counters (hist_rev:<key>, prefix stripped). */
  revs: Record<string, number>;
  legacyImported: boolean;
  /** Contactos persistidos del hub: `hub:<uuid>` → nombre (filas hub_contacts). */
  contacts?: Record<string, string>;
}

export type Invoker = (
  cmd: string,
  args?: Record<string, unknown>,
) => Promise<unknown>;

const LEGACY_KEY = "lanchat.history.v1";

/**
 * Tauri command bindings for the append-model persistence API. The invoker
 * is injectable so the mapping stays testable without a Tauri runtime.
 */
export function makeHistoryApi(invokeFn: Invoker = invoke) {
  return {
    load(): Promise<HistoryLoad> {
      return invokeFn("history_load") as Promise<HistoryLoad>;
    },
    append(key: string, id: string, entry: History[string][number], token: HistToken): Promise<void> {
      return invokeFn("history_append", { key, id, entry, token }).then(() => undefined);
    },
    /** `state` undefined → null so Tauri deserializes Option::None. */
    patchState(
      key: string,
      id: string,
      state: string | undefined,
      read: boolean,
      token: HistToken,
    ): Promise<boolean> {
      return invokeFn("history_patch_state", {
        key,
        id,
        newState: state ?? null,
        read,
        token,
      }) as Promise<boolean>;
    },
    deleteConversation(key: string): Promise<HistToken> {
      return invokeFn("history_delete_conversation", { key }) as Promise<HistToken>;
    },
    deleteAll(): Promise<number> {
      return invokeFn("history_delete_all") as Promise<number>;
    },
    importLegacy(history: History): Promise<boolean> {
      return invokeFn("history_import_legacy", { history }) as Promise<boolean>;
    },
  };
}

export type HistoryApi = ReturnType<typeof makeHistoryApi>;

// Migración: historial de la era localStorage (antes de SQLite).
export function readLegacyHistory(): History {
  try {
    const raw = localStorage.getItem(LEGACY_KEY);
    return raw ? (JSON.parse(raw) as History) : {};
  } catch {
    return {};
  }
}

export function removeLegacySnapshot(): void {
  try {
    localStorage.removeItem(LEGACY_KEY);
  } catch {
    /* almacenamiento no disponible: nada que limpiar */
  }
}

export function readLegacySnapshotOrNull(): History | null {
  try {
    return localStorage.getItem(LEGACY_KEY) === null ? null : readLegacyHistory();
  } catch {
    return null;
  }
}
