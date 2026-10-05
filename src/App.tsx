import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronLeft, MessageSquare, Server, Wifi } from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { DeviceList } from "./components/DeviceList";
import { Conversation } from "./components/Conversation";
import { HubPairingRequests } from "./components/HubPairingRequests";
import { Avatar } from "./components/Avatar";
import {
  demoRequested,
  discover,
  getDownloadFolder,
  getPinFor,
  hubCancelPairing,
  hubPairing,
  hubPresence,
  hubSendFile,
  hubSendText,
  hubStatus,
  isTauri,
  onHubFile,
  onHubFileError,
  onHubMessage,
  onHubPairing,
  onHubPresence,
  onHubState,
  onPairDone,
  onPairRequest,
  pairVerify,
  sendPairRequest,
  onFile,
  onMessage,
  onReadAck,
  probePort,
  regenerateOwnPin,
  sendAck,
  sendFile,
  sendText,
  setDownloadFolder as persistDownloadFolder,
  setPinFor,
  type RawDevice,
} from "./lib/backend";
import { DEMO_DEVICES, demoHistory, runDemoSim } from "./lib/demo";
import {
  makeHistoryApi,
  readLegacySnapshotOrNull,
  removeLegacySnapshot,
  type HistoryApi,
} from "./lib/history";
import {
  INITIAL_TOKEN,
  mergeLoadedHistory,
  planLegacyImport,
  tokensAfterDeleteAll,
  tokensFromRevs,
  withHistoryToken,
  type HistToken,
} from "./lib/history-core";
import {
  hubStatusLabel,
  hubStatusTitle,
  hubStatusTone,
  trackHubStatus,
  type HubUiState,
} from "./lib/hub-status";
import {
  bindMintedContact,
  buildHubRows,
  hubFileErrorNotice,
  hubSendOutcome,
  isContactKey,
  learnContactBindings,
  newlyPairedConns,
  planEntryCommit,
  planHubFileInbound,
  planHubInbound,
  routeSend,
} from "./lib/hub-chat-core";
import {
  HUB_PEER_PREVIEW,
  hubConnIdFromKey,
  hubPeerKey,
  hubPeerName,
  isHubKey,
  mergeDevices,
  trackHubPresence,
  type HubPeer,
} from "./lib/hub-presence";
import {
  applyCancelResult,
  EMPTY_PAIRING,
  HUB_PAIRED_LABEL,
  pairedConnIds,
  trackHubPairing,
  type HubPairingSnapshot,
} from "./lib/hub-pairing";
import { displayName, type DeviceState, type Entry, type History } from "./types";

const DEMO = typeof window === "undefined" || !isTauri() || demoRequested();

function HubChip({ state }: { state: HubUiState }) {
  return (
    <span
      className={`net-chip hub-chip hub-${hubStatusTone(state)}`}
      title={hubStatusTitle(state)}
    >
      <Server size={13} aria-hidden />
      <span className="hub-dot" aria-hidden />
      {hubStatusLabel(state)}
    </span>
  );
}

interface EntryDraft {
  mine: boolean;
  text: string;
  state?: Entry["state"];
  filePath?: string;
}

// Detalle de una sesión de navegador del hub: presencia únicamente. Sin
// composer, sin historial, sin vinculación: si aún no existe, no se inventa.
function HubPeerDetail({
  peer,
  lastName,
  paired,
  onBack,
}: {
  peer: HubPeer | null;
  lastName?: string;
  paired?: boolean;
  onBack: () => void;
}) {
  const name = peer ? hubPeerName(peer) : lastName?.trim() || "Navegador";
  return (
    <section className="conv-col" aria-label="Sesión de navegador">
      <header className="conv-head">
        <button
          type="button"
          className="icon-btn is-back"
          aria-label="Volver a dispositivos"
          onClick={onBack}
        >
          <ChevronLeft size={20} />
        </button>
        <Avatar name={name} online={!!peer} size={36} />
        <div className="conv-title">
          <h3>{name}</h3>
          <span className={`conv-status ${peer ? "" : "is-off"}`}>
            {peer ? "En línea" : "Se desconectó del hub"}
          </span>
          {peer && paired && <span className="hub-paired-tag">{HUB_PAIRED_LABEL}</span>}
        </div>
      </header>
      <div className="conv-scroll hub-detail">
        <p className="hub-detail-title">{HUB_PEER_PREVIEW}</p>
        <p className="hub-detail-sub">
          {peer
            ? paired
              ? "Vinculada. Su conversación está en la lista de dispositivos; escribí desde ahí."
              : "Esta sesión aparece por el hub. Vinculala desde el navegador para conversar."
            : "La sesión se desconectó del hub y ya no está disponible."}
        </p>
      </div>
    </section>
  );
}

export default function App() {
  const [devices, setDevices] = useState<DeviceState[]>([]);
  const [history, setHistory] = useState<History>({});
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [scanning, setScanning] = useState(true);
  const [narrow, setNarrow] = useState(false);
  const [downloadFolder, setDownloadFolderState] = useState("");
  const [dragging, setDragging] = useState(false);
  const [pairRequest, setPairRequest] = useState<{ from: string; code: string } | null>(
    null,
  );
  const [pendingPin, setPendingPin] = useState<string | null>(null);
  const [pairingKey, setPairingKey] = useState<string | null>(null);
  const [pinError, setPinError] = useState("");
  const [pinInput, setPinInput] = useState("");
  const [hubState, setHubState] = useState<HubUiState>(() =>
    DEMO ? { kind: "demo" } : { kind: "connecting" },
  );
  const [hubPeers, setHubPeers] = useState<HubPeer[]>([]);
  // Contactos persistidos del hub (`hub:<uuid>` → nombre): vienen con el
  // history_load y crecen cuando un evento revela un contacto recién
  // vinculado. Son la mitad persistente de las conversaciones del hub.
  const [hubContacts, setHubContacts] = useState<Record<string, string>>({});
  // Emparejamiento de navegadores vía hub: snapshot efímero, atado a la
  // sesión actual del hub. Sin DB, sin localStorage: muere con la app.
  const [hubPairingState, setHubPairingState] =
    useState<HubPairingSnapshot>(EMPTY_PAIRING);
  // Último nombre visto por conexión: para que el detalle de un par que se fue
  // siga diciendo quién era en vez de volver a "Navegador". Solo memoria.
  const hubNamesRef = useRef<Map<string, string>>(new Map());

  const devicesRef = useRef<DeviceState[]>([]);
  // Tokens de vigencia del modelo append: epoch global + rev por clave.
  // Se capturan en el momento de cada append/patch; un delete concurrente
  // los invalida y la operación se recarga + reintenta una sola vez.
  const tokensRef = useRef<Record<string, HistToken>>({});
  const epochRef = useRef<number>(0);
  const animateRef = useRef<Map<string, number>>(new Map());
  const scanningRef = useRef(false);
  const selectedKeyRef = useRef<string | null>(null);
  const windowFocusedRef = useRef(true);
  const historyRef = useRef<History>({});
  const hubContactsRef = useRef<Record<string, string>>({});
  const onlineRef = useRef<Map<string, boolean>>(new Map());
  const retryRef = useRef<(key: string, id: string) => void>(() => {});

  useEffect(() => {
    devicesRef.current = devices;
  }, [devices]);

  useEffect(() => {
    selectedKeyRef.current = selectedKey;
  }, [selectedKey]);

  useEffect(() => {
    historyRef.current = history;
  }, [history]);

  useEffect(() => {
    hubContactsRef.current = hubContacts;
  }, [hubContacts]);

  useEffect(() => {
    const mq = window.matchMedia("(max-width: 700px)");
    const apply = () => setNarrow(mq.matches);
    apply();
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  }, []);

  // Enlace con el hub: suscripción primero, consulta inicial después (ver
  // trackHubStatus). Fuera de la app nativa nunca se invoca ni se escucha.
  useEffect(() => {
    if (DEMO) return;
    return trackHubStatus(
      { get: hubStatus, on: onHubState },
      {
        onStatus: (status) => setHubState({ kind: "status", status }),
        onUnavailable: () => setHubState({ kind: "unavailable" }),
      },
    );
  }, []);

  // Presencia del hub: sesiones de navegador efímeras, reemplazo completo del
  // snapshot (uno vacío = se fueron todas). Nunca crean historial ni
  // conversación: solo llenan la lista con la etiqueta correspondiente.
  useEffect(() => {
    if (DEMO) return;
    return trackHubPresence(
      { get: hubPresence, on: onHubPresence },
      {
        onPeers: (peers) => {
          for (const p of peers) {
            if (p.name.trim()) hubNamesRef.current.set(p.conn_id, p.name.trim());
          }
          setHubPeers(peers);
        },
        onUnavailable: () => setHubPeers([]),
      },
    );
  }, []);

  // Emparejamiento vía hub: mismos contratos de ciclo de vida que presencia
  // (suscribir primero, consulta después, snapshot obsoleto descartado).
  // Snapshot vacío = no hay nada vivo: se quitan tarjetas y etiqueta.
  useEffect(() => {
    if (DEMO) return;
    return trackHubPairing(
      { get: hubPairing, on: onHubPairing },
      {
        onSnapshot: setHubPairingState,
        onUnavailable: () => setHubPairingState(EMPTY_PAIRING),
      },
    );
  }, []);

  // Cancel confirmado por el backend: la entrada exacta (conn, req) sale de
  // la vista local; una solicitud nueva para la misma conn nunca es afectada.
  const clearHubPairing = useCallback((connId: string, reqId: string) => {
    setHubPairingState((s) => ({
      ...s,
      pending: applyCancelResult(s.pending, connId, reqId),
    }));
  }, []);

  // Persistencia: en demo no hay backend y todo es memoria. Cada wrapper
  // captura la clave y el token EN EL MOMENTO de la llamada; un error de
  // `stale-history` recarga la conversación desde la DB y reintenta una vez.
  // Cualquier otro error de DB se loguea: la UI sigue funcionando igual.
  const histApi: HistoryApi | null = useMemo(() => (DEMO ? null : makeHistoryApi()), []);

  const refreshKeyToken = useCallback(
    async (key: string): Promise<HistToken> => {
      if (!histApi) return INITIAL_TOKEN;
      const loaded = await histApi.load();
      epochRef.current = loaded.epoch;
      tokensRef.current = tokensFromRevs(loaded.revs, loaded.epoch);
      setHistory((h) => ({
        ...h,
        [key]: mergeLoadedHistory({ [key]: h[key] ?? [] }, loaded.history)[key] ?? [],
      }));
      return tokensRef.current[key] ?? INITIAL_TOKEN;
    },
    [histApi],
  );

  const persistAppend = useCallback(
    (key: string, entry: Entry) => {
      if (!histApi) return;
      const token = tokensRef.current[key] ?? { epoch: epochRef.current, rev: 0 };
      void withHistoryToken(
        token,
        (t) => histApi.append(key, entry.id, entry, t),
        () => refreshKeyToken(key),
      ).catch((e) => console.error("history_append falló:", e));
    },
    [histApi, refreshKeyToken],
  );

  const persistPatch = useCallback(
    (key: string, id: string, state: Entry["state"], read?: boolean) => {
      if (!histApi) return;
      const token = tokensRef.current[key] ?? { epoch: epochRef.current, rev: 0 };
      void withHistoryToken(
        token,
        (t) => histApi.patchState(key, id, state, read ?? false, t),
        () => refreshKeyToken(key),
      ).catch((e) => console.error("history_patch_state falló:", e));
    },
    [histApi, refreshKeyToken],
  );

  const pushEntry = useCallback(
    (
      key: string,
      draft: EntryDraft,
      wireId?: string,
      opts?: { persist?: boolean },
    ): string => {
      const entry: Entry = {
        id: wireId ?? crypto.randomUUID(),
        at: Date.now(),
        state: draft.mine ? "sending" : undefined,
        ...draft,
      };
      setHistory((h) => ({ ...h, [key]: [...(h[key] ?? []), entry] }));
      if (!isHubKey(key) && !isContactKey(key)) {
        // Las filas del hub las manejan contactos + presencia, no el outbox LAN.
        setDevices((prev) =>
          prev.some((d) => d.key === key)
            ? prev
            : [...prev, { key, name: displayName(key), online: false }],
        );
      }
      // Append exactamente una vez por mensaje; la hidratación de mensajes ya
      // persistidos nativamente entra con persist:false y NO re-appendea.
      if (planEntryCommit(opts)) persistAppend(key, entry);
      return entry.id;
    },
    [persistAppend],
  );

  const patchEntry = useCallback(
    (key: string, id: string, state: Entry["state"]) => {
      setHistory((h) => ({
        ...h,
        [key]: (h[key] ?? []).map((e) => (e.id === id ? { ...e, state } : e)),
      }));
      persistPatch(key, id, state);
    },
    [persistPatch],
  );

  // Avisa al otro dispositivo que sus mensajes fueron vistos (palomitas azules).
  const notifyRead = useCallback((key: string, ids: string[]) => {
    if (ids.length === 0) return;
    // Los acuses de lectura son un contrato LAN (IP destino): por hub todavía
    // no existen, y fingirlos con un warning ensuciaría la consola.
    if (isHubKey(key) || isContactKey(key)) return;
    const device = devicesRef.current.find((d) => d.key === key);
    if (!device?.ip) {
      console.warn("notifyRead: no encuentro IP para", key);
      return;
    }
    const payload = JSON.stringify({ kind: "read-ack", ids });
    setHistory((h) => ({
      ...h,
      [key]: (h[key] ?? []).map((e) =>
        !e.mine && ids.includes(e.id) ? { ...e, read: true } : e,
      ),
    }));
    for (const id of ids) persistPatch(key, id, undefined, true);
    console.log("notifyRead →", device.ip, ids.length, "ids");
    void sendAck(device.ip, payload);
  }, [persistPatch]);

  // Las palomitas azules solo cuentan si la ventana está en primer plano:
  // al volver el foco, se marcan como leídos los del chat abierto.
  useEffect(() => {
    const update = () => {
      const focused = document.hasFocus();
      const wasFocused = windowFocusedRef.current;
      windowFocusedRef.current = focused;
      if (focused && !wasFocused) {
        const key = selectedKeyRef.current;
        if (key) {
          const unread = (historyRef.current[key] ?? [])
            .filter((e) => !e.mine && !e.read)
            .map((e) => e.id);
          notifyRead(key, unread);
        }
      }
    };
    window.addEventListener("focus", update);
    window.addEventListener("blur", update);
    return () => {
      window.removeEventListener("focus", update);
      window.removeEventListener("blur", update);
    };
  }, [notifyRead]);

  useEffect(() => {
    if (DEMO) {
      setDevices(DEMO_DEVICES);
      setHistory(demoHistory());
      setScanning(false);
      return runDemoSim(
        (device) =>
          setDevices((prev) =>
            prev.some((d) => d.key === device.key)
              ? prev.map((d) => (d.key === device.key ? { ...d, online: true } : d))
              : [...prev, device],
          ),
        (key, text) => pushEntry(key, { mine: false, text }),
      );
    }
    (async () => {
      try {
        if (!histApi) return;
        const stored = await histApi.load();
        epochRef.current = stored.epoch;
        tokensRef.current = tokensFromRevs(stored.revs, stored.epoch);
        if (stored.contacts) {
          hubContactsRef.current = stored.contacts;
          setHubContacts(stored.contacts);
        }
        // Unión por id (gana la DB): eventos de arranque que llegaron antes
        // de que resolviera la carga no se pierden ni se pisan.
        setHistory((h) => mergeLoadedHistory(h, stored.history));
        // Importación única del snapshot localStorage→DB, decidida por el
        // flag del backend; el snapshot se borra solo si la DB lo aceptó.
        const snapshot = planLegacyImport(stored.legacyImported, readLegacySnapshotOrNull);
        if (snapshot) {
          const imported = await histApi.importLegacy(snapshot);
          if (imported) {
            removeLegacySnapshot();
            const fresh = await histApi.load();
            epochRef.current = fresh.epoch;
            tokensRef.current = tokensFromRevs(fresh.revs, fresh.epoch);
            if (fresh.contacts) {
              hubContactsRef.current = fresh.contacts;
              setHubContacts(fresh.contacts);
            }
            setHistory((h) => mergeLoadedHistory(h, fresh.history));
          }
        }
      } catch (e) {
        console.error("No se pudo cargar el historial:", e);
      }
    })();
  }, [histApi]);

  const applyScan = useCallback((found: RawDevice[]) => {
    const onlineKeys = new Set(found.map((f) => displayName(f.name)));
    const firstScan = onlineRef.current.size === 0;
    const revived: string[] = [];
    for (const key of onlineKeys) {
      if (onlineRef.current.get(key) === false) revived.push(key);
    }
    for (const key of onlineKeys) onlineRef.current.set(key, true);
    for (const [key, was] of onlineRef.current) {
      if (!onlineKeys.has(key) && was) onlineRef.current.set(key, false);
    }

    // Outbox: los mensajes fallidos se reenvían solos cuando el
    // dispositivo vuelve a aparecer en la red.
    const targets = firstScan ? [...onlineKeys] : revived;
    for (const key of targets) {
      const failed = (historyRef.current[key] ?? []).filter(
        (e) => e.state === "failed",
      );
      for (const e of failed) retryRef.current(key, e.id);
    }

    setDevices((prev) => {
      const map = new Map(prev.map((d) => [d.key, { ...d }]));
      const seen = new Set<string>();
      for (const f of found) {
        const key = displayName(f.name);
        seen.add(key);
        map.set(key, { key, name: displayName(f.name), ip: f.ip, online: true });
      }
      for (const [k, d] of map) {
        if (!seen.has(k) && d.online) map.set(k, { ...d, online: false });
      }
      return [...map.values()];
    });
  }, []);

  const runScan = useCallback(async () => {
    if (DEMO || scanningRef.current) return;
    scanningRef.current = true;
    setScanning(true);
    try {
      applyScan(await discover());
    } catch {
      /* red no disponible: la lista queda como está */
    } finally {
      scanningRef.current = false;
      setScanning(false);
    }
  }, [applyScan]);

  useEffect(() => {
    if (DEMO) return;
    let cancelled = false;
    const offs: Array<() => void> = [];
    runScan();
    getDownloadFolder()
      .then(setDownloadFolderState)
      .catch(() => {});
    onMessage((msg) => {
      const id = pushEntry(msg.from, { mine: false, text: msg.text }, msg.id);
      if (selectedKeyRef.current === msg.from && windowFocusedRef.current)
        notifyRead(msg.from, [id]);
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    onFile((f) => {
      const id = pushEntry(
        f.from,
        {
          mine: false,
          text: `📎 ${f.name}`,
          filePath: f.path,
        },
        f.id,
      );
      if (selectedKeyRef.current === f.from && windowFocusedRef.current)
        notifyRead(f.from, [id]);
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    return () => {
      cancelled = true;
      offs.forEach((fn) => fn());
    };
  }, [pushEntry, runScan, notifyRead]);

  // Outbox activo: mientras haya mensajes fallidos, tocar la puerta del
  // dispositivo cada 10s; cuando responde, reenviar todo lo pendiente.
  useEffect(() => {
    if (DEMO) return;
    const t = setInterval(async () => {
      for (const [key, entries] of Object.entries(historyRef.current)) {
        const failed = entries.filter((e) => e.state === "failed");
        if (failed.length === 0) continue;
        const device = devicesRef.current.find((d) => d.key === key);
        if (!device?.ip) continue;
        try {
          const open = await probePort(device.ip);
          if (open) {
            for (const e of failed) retryRef.current(key, e.id);
          }
        } catch {
          /* sin conexión todavía */
        }
      }
    }, 10_000);
    return () => clearInterval(t);
  }, []);

  // Solicitud de vinculación entrante: mostrar el código de sesión.
  useEffect(() => {
    if (DEMO) return;
    let cancelled = false;
    const offs: Array<() => void> = [];
    onPairRequest((r) => {
      if (!cancelled) setPairRequest(r);
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    onPairDone((done) => {
      // El receptor guarda el código como pin del iniciador → simetría.
      setPinFor(done.from, done.code);
      if (!cancelled) setPairRequest(null);
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    return () => {
      cancelled = true;
      offs.forEach((fn) => fn());
    };
  }, []);

  // Palomitas azules: el otro lado vio los mensajes.
  useEffect(() => {
    if (DEMO) return;
    let cancelled = false;
    const offs: Array<() => void> = [];
    onReadAck((a) => {
      console.log("read-ack recibido de", a.from, "con", a.ids.length, "ids");
      const device = devicesRef.current.find((d) => d.ip === a.from);
      if (!device) {
        console.warn("read-ack de IP desconocida:", a.from);
        return;
      }
      setHistory((h) => ({
        ...h,
        [device.key]: (h[device.key] ?? []).map((e) =>
          a.ids.includes(e.id) ? { ...e, state: "read" as const } : e,
        ),
      }));
      for (const id of a.ids) persistPatch(device.key, id, "read");
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    return () => {
      cancelled = true;
      offs.forEach((fn) => fn());
    };
  }, [persistPatch]);

  // Mensajes entrantes del hub: la fila ya fue persistida nativamente ANTES
  // del evento, así que la UI hidrata con el id del wire y NO re-appendea
  // (persist:false evita el insert duplicado). Un contacto recién vinculado
  // aprende su nombre acá; payloads malformados se ignoran a la defensiva.
  useEffect(() => {
    if (DEMO) return;
    let cancelled = false;
    const offs: Array<() => void> = [];
    onHubMessage((raw) => {
      const plan = planHubInbound(raw, hubContactsRef.current);
      if (!plan) return;
      if (plan.upsertName !== null) {
        const next = { ...hubContactsRef.current, [plan.msg.key]: plan.upsertName };
        hubContactsRef.current = next;
        setHubContacts(next);
      }
      pushEntry(plan.msg.key, { mine: false, text: plan.msg.text }, plan.msg.id, {
        persist: false,
      });
      // Sin acuses de lectura por hub todavía: no hay destino LAN adonde
      // mandar el read-ack.
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    // Archivos entrantes del hub: misma regla que los mensajes, la fila ya
    // fue persistida nativamente ANTES del evento (persist:false evita el
    // insert duplicado).
    onHubFile((raw) => {
      const plan = planHubFileInbound(raw, hubContactsRef.current);
      if (!plan) return;
      if (plan.upsertName !== null) {
        const next = { ...hubContactsRef.current, [plan.file.key]: plan.upsertName };
        hubContactsRef.current = next;
        setHubContacts(next);
      }
      pushEntry(
        plan.file.key,
        { mine: false, text: `📎 ${plan.file.name}`, filePath: plan.file.path },
        plan.file.id,
        { persist: false },
      );
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    // Recepción fallida: fila de sistema persistida con causa honesta.
    onHubFileError((err) => {
      if (!isContactKey(err.key)) return;
      pushEntry(err.key, {
        mine: false,
        text: hubFileErrorNotice(err.name, err.reason),
      });
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    return () => {
      cancelled = true;
      offs.forEach((fn) => fn());
    };
  }, [pushEntry]);

  const sorted = useMemo(() => {
    const activity = (k: string) => history[k]?.[history[k].length - 1]?.at ?? 0;
    return [...devices].sort(
      (a, b) =>
        Number(b.online) - Number(a.online) ||
        activity(b.key) - activity(a.key) ||
        a.name.localeCompare(b.name),
    );
  }, [devices, history]);

  const hubPairedIds = useMemo(() => pairedConnIds(hubPairingState), [hubPairingState]);
  // Un grant recién comprometido acuña su contacto nativamente (hub:<uuid>),
  // pero la UI solo lo ve releyendo `contacts`. Al crecer el conjunto de pares
  // recargamos y adoptamos los contactos: la fila de conversación aparece sin
  // esperar un mensaje entrante (si no, la única fila "Vinculado" es la de
  // sesión y su click cae en el detalle de presencia, sin conversación).
  const prevPairedRef = useRef<ReadonlySet<string>>(new Set());
  // Vínculo conn→contacto aprendido del delta de recarga (verdad nativa):
  // el mint nativo crea un `hub:<uuid>` POR sesión del hub, así que los
  // nombres duplicados son el caso común y el match por nombre único falla.
  const learnedBindingsRef = useRef<Map<string, string>>(new Map());
  const [learnedBindings, setLearnedBindings] = useState<Map<string, string>>(
    new Map(),
  );
  useEffect(() => {
    if (DEMO || !histApi) return;
    const fresh = newlyPairedConns(prevPairedRef.current, hubPairedIds);
    prevPairedRef.current = hubPairedIds;
    if (fresh.length === 0) return;
    const before = new Set(Object.keys(hubContactsRef.current));
    histApi
      .load()
      .then((loaded) => {
        if (!loaded.contacts) return;
        const minted = bindMintedContact(fresh, before, loaded.contacts);
        if (minted && learnedBindingsRef.current.get(minted[0]) !== minted[1]) {
          learnedBindingsRef.current.set(minted[0], minted[1]);
          setLearnedBindings(new Map(learnedBindingsRef.current));
        }
        const next = { ...hubContactsRef.current, ...loaded.contacts };
        hubContactsRef.current = next;
        setHubContacts(next);
      })
      .catch((e) => console.error("contacts tras emparejar falló:", e));
  }, [histApi, hubPairedIds]);
  // Contacto por conexión (solo agrupación visual): el vínculo aprendido del
  // mint (delta de recarga) manda; el match por nombre único es el resguardo
  // cuando no hubo delta observable. Ambiguo o anónimo → filas separadas;
  // la autorización real sigue nativa.
  const contactForConn = useMemo(() => {
    const byName = learnContactBindings(hubPeers, hubContacts);
    if (learnedBindings.size === 0) return byName;
    return new Map([...byName, ...learnedBindings]);
  }, [hubPeers, hubContacts, learnedBindings]);
  const hubRows = useMemo(
    () =>
      buildHubRows({
        contacts: hubContacts,
        peers: hubPeers,
        pairedConnIds: hubPairedIds,
        contactForConn,
      }),
    [hubContacts, hubPeers, hubPairedIds, contactForConn],
  );
  const listDevices = useMemo(() => mergeDevices(sorted, hubRows.rows), [sorted, hubRows.rows]);
  // Carriles separados: si hay un modal LAN abierto (PIN o solicitud), las
  // tarjetas del hub se difieren; el estado del backend sigue vivo y vuelven
  // al cerrarse. Nunca dos overlays encima ni bloqueos cruzados.
  const hubPairingDeferred = !!pairRequest || !!pendingPin || !!pairingKey;

  const selectedLan = sorted.find((d) => d.key === selectedKey) ?? null;
  // La selección del hub sobrevive a la desconexión del par: el detalle muestra
  // el estado "se fue" en lugar de fingir presencia o saltar a la bienvenida.
  const selectedHubKey = selectedKey && isHubKey(selectedKey) ? selectedKey : null;
  const selectedHubPeer =
    selectedHubKey ? (hubPeers.find((p) => hubPeerKey(p.conn_id) === selectedHubKey) ?? null) : null;
  // Conversación de contacto del hub: la fila puede faltar un instante (estado
  // de contactos aún vacío); el fallback mantiene la conversación abierta.
  const selectedContactKey = selectedKey && isContactKey(selectedKey) ? selectedKey : null;
  const selectedContact =
    selectedContactKey
      ? (hubRows.rows.find((d) => d.key === selectedContactKey) ?? {
          key: selectedContactKey,
          name: hubContacts[selectedContactKey]?.trim() || "Navegador",
          kind: "hub" as const,
          online: false,
        })
        : null;

  const openChat = useCallback(
    (key: string) => {
      animateRef.current.set(key, Date.now());
      setSelectedKey(key);
      const unread = (historyRef.current[key] ?? [])
        .filter((e) => !e.mine && !e.read)
        .map((e) => e.id);
      notifyRead(key, unread);
    },
    [notifyRead],
  );

  // Dispositivo sin vincular → pedir código al otro y abrir el flujo de vinculación.
  const openConversation = useCallback(
    (key: string) => {
      if (!DEMO && !getPinFor(key)) {
        const device = devicesRef.current.find((d) => d.key === key);
        if (device?.ip) void sendPairRequest(device.ip);
        setPairingKey(key);
        setPinError("");
        setPinInput("");
        return;
      }
      openChat(key);
    },
    [openChat],
  );

  // Sesiones del hub: el contacto vinculado abre su conversación (sin flujo
  // LAN de PIN); la sesión sin vincular queda en detalle de presencia.
  const handleDeviceSelect = useCallback(
    (key: string) => {
      if (isContactKey(key)) {
        openChat(key);
        return;
      }
      if (isHubKey(key)) {
        setSelectedKey(key);
        return;
      }
      openConversation(key);
    },
    [openConversation, openChat],
  );

  const send = useCallback(
    async (key: string, text: string) => {
      const device = devicesRef.current.find((d) => d.key === key);
      const route = routeSend(key, device);
      if (route.kind === "hub") {
        // Contacto del hub: entrada optimista (se persiste como hoy) y
        // envío con acuse acotado. Sin cola: cada error parchea y explica.
        const id = crypto.randomUUID();
        pushEntry(key, { mine: true, text }, id);
        try {
          await hubSendText(key, id, text);
          patchEntry(key, id, "sent");
        } catch (e) {
          patchEntry(key, id, "failed");
          pushEntry(key, {
            mine: false,
            text: hubSendOutcome({ ok: false, error: String(e) }).systemText,
          });
        }
        return;
      }
      const id = crypto.randomUUID();
      pushEntry(key, { mine: true, text }, id);
      try {
        if (device?.ip) {
          const estado = await sendText(device.ip, getPinFor(key), text, id);
          patchEntry(key, id, estado === "delivered" ? "delivered" : "sent");
        } else if (device) {
          await new Promise((r) => setTimeout(r, 400));
          patchEntry(key, id, "sent");
        } else throw new Error("desconocido");
      } catch (e) {
        if (String(e).includes("PIN_REQUERIDO")) setPendingPin(key);
        else console.error("send falló:", e);
        patchEntry(key, id, "failed");
      }
    },
    [pushEntry, patchEntry],
  );

  const retry = useCallback(
    (key: string, id: string) => {
      const entry = (history[key] ?? []).find((e) => e.id === id);
      if (!entry) return;
      patchEntry(key, id, "sending");
      if (isContactKey(key)) {
        // Reintento por hub: archivo con su path o texto, según la entrada.
        void (async () => {
          try {
            if (entry.filePath) await hubSendFile(key, id, entry.filePath);
            else await hubSendText(key, id, entry.text);
            patchEntry(key, id, "sent");
          } catch (e) {
            patchEntry(key, id, "failed");
            pushEntry(key, {
              mine: false,
              text: hubSendOutcome({ ok: false, error: String(e) }).systemText,
            });
          }
        })();
        return;
      }
      const device = devicesRef.current.find((d) => d.key === key);
      const deliver = async () => {
        try {
          if (entry.filePath && device?.ip) {
            const estado = await sendFile(device.ip, getPinFor(key), entry.filePath, id);
            patchEntry(key, id, estado === "delivered" ? "delivered" : "sent");
          } else if (!entry.filePath && device?.ip) {
            const estado = await sendText(device.ip, getPinFor(key), entry.text, id);
            patchEntry(key, id, estado === "delivered" ? "delivered" : "sent");
          } else if (device) {
            await new Promise((r) => setTimeout(r, 400));
            patchEntry(key, id, "sent");
          } else throw new Error("desconocido");
        } catch (e) {
          if (String(e).includes("PIN_REQUERIDO")) setPendingPin(key);
          else console.error("reintento falló:", e);
          patchEntry(key, id, "failed");
        }
      };
      deliver();
    },
    [history, patchEntry, pushEntry],
  );

  useEffect(() => {
    retryRef.current = retry;
  }, [retry]);

  const deleteConversation = useCallback(
    (key: string) => {
      if (histApi) {
        // La DB devuelve el token fresco de la clave; se guarda ANTES de que
        // cualquier append posterior use el rev viejo y rebote.
        histApi
          .deleteConversation(key)
          .then((fresh) => {
            tokensRef.current = { ...tokensRef.current, [key]: fresh };
          })
          .catch((e) => console.error("history_delete_conversation falló:", e));
      }
      setHistory((h) => {
        const next = { ...h };
        delete next[key];
        return next;
      });
    },
    [histApi],
  );

  const deleteAll = useCallback(() => {
    if (histApi) {
      histApi
        .deleteAll()
        .then((epoch) => {
          epochRef.current = epoch;
          tokensRef.current = tokensAfterDeleteAll(tokensRef.current, epoch);
        })
        .catch((e) => console.error("history_delete_all falló:", e));
    }
    setHistory({});
  }, [histApi]);

  const pickDownloadFolder = useCallback(async () => {
    const picked = await openDialog({
      directory: true,
      title: "Carpeta para archivos recibidos",
    });
    if (typeof picked === "string") {
      await persistDownloadFolder(picked);
      setDownloadFolderState(picked);
    }
  }, []);

  const sendFileTo = useCallback(
    async (key: string, path: string) => {
      const device = devicesRef.current.find((d) => d.key === key);
      const route = routeSend(key, device);
      const name = path.split(/[\\/]/).pop() ?? path;
      const id = crypto.randomUUID();
      if (route.kind === "hub") {
        // Contacto del hub: entrada optimista persistida (como LAN) y envío
        // con acuse acotado; cada error parchea y explica con su línea.
        pushEntry(key, { mine: true, text: `📎 ${name}`, filePath: path }, id);
        try {
          await hubSendFile(key, id, path);
          patchEntry(key, id, "sent");
        } catch (e) {
          patchEntry(key, id, "failed");
          pushEntry(key, {
            mine: false,
            text: hubSendOutcome({ ok: false, error: String(e) }).systemText,
          });
        }
        return;
      }
      if (!device?.ip) return;
      pushEntry(key, { mine: true, text: `📎 ${name}`, filePath: path }, id);
      try {
        const estado = await sendFile(device.ip, getPinFor(key), path, id);
        patchEntry(key, id, estado === "delivered" ? "delivered" : "sent");
      } catch (e) {
        if (String(e).includes("PIN_REQUERIDO")) setPendingPin(key);
        else console.error("send_file falló:", e);
        patchEntry(key, id, "failed");
      }
    },
    [patchEntry, pushEntry],
  );

  const attachAndSend = useCallback(
    async (key: string) => {
      const picked = await openDialog({
        multiple: false,
        title: "Elegí un archivo para enviar",
      });
      if (typeof picked === "string") await sendFileTo(key, picked);
    },
    [sendFileTo],
  );

  // Emparejar: guardar el PIN del otro dispositivo, verificarlo y reenviar pendientes.
  const submitPin = useCallback(async () => {
    const key = pendingPin ?? pairingKey;
    const pin = pinInput.trim();
    if (!key || pin.length < 4) return;
    setPinFor(key, pin);
    setPinInput("");

    if (pairingKey) {
      const device = devicesRef.current.find((d) => d.key === key);
      if (!device?.ip) {
        setPinError("No encontré su dirección. Tocá la lupa e intentá de nuevo.");
        return;
      }
      try {
        await pairVerify(device.ip, pin);
        setPairingKey(null);
        setPinError("");
        openChat(key);
      } catch (e) {
        const msg = String(e);
        setPinError(
          msg.includes("PIN incorrecto")
            ? "PIN incorrecto, probá de nuevo."
            : msg,
        );
      }
      return;
    }

    setPendingPin(null);
    const failed = (historyRef.current[key] ?? []).filter(
      (e) => e.state === "failed",
    );
    for (const e of failed) retryRef.current(key, e.id);
  }, [pendingPin, pairingKey, pinInput, openChat]);

  const regeneratePin = useCallback(async () => {
    try {
      await regenerateOwnPin();
    } catch (e) {
      console.error("No se pudo regenerar el PIN:", e);
    }
  }, []);

  // Arrastrar archivos desde el sistema y soltarlos en la app.
  useEffect(() => {
    if (DEMO) return;
    let cancelled = false;
    let un: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") {
          setDragging(true);
        } else if (event.payload.type === "leave") {
          setDragging(false);
        } else if (event.payload.type === "drop") {
          setDragging(false);
          const key = selectedKeyRef.current;
          if (!key) return;
          for (const p of event.payload.paths) void sendFileTo(key, p);
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else un = fn;
      });
    return () => {
      cancelled = true;
      un?.();
    };
  }, [sendFileTo]);

  return (
    <div className={`app ${dragging ? "is-dragging" : ""}`}>
      <header className="topbar">
        <span className="app-mark" aria-hidden>
          <MessageSquare size={15} strokeWidth={2.2} />
        </span>
        <span className="app-name">LAN-Chat</span>
        <span className="topbar-spacer" />
        {DEMO ? (
          <>
            <span className="demo-badge">Demostración</span>
            <HubChip state={{ kind: "demo" }} />
          </>
        ) : (
          <>
            <span className="net-chip">
              <Wifi size={13} aria-hidden />
              Red local
            </span>
            <HubChip state={hubState} />
          </>
        )}
      </header>

      <div
        className={`shell ${narrow && (selectedLan || selectedContact || selectedHubKey) ? "show-conv" : ""}`}
      >
        <DeviceList
          devices={listDevices}
          history={history}
          pairedKeys={hubRows.pairedKeys}
          scanning={scanning}
          selectedKey={selectedKey}
          onSelect={handleDeviceSelect}
          onRescan={runScan}
          onPickFolder={pickDownloadFolder}
          downloadFolder={downloadFolder}
          onRegeneratePin={regeneratePin}
          onDeleteAll={deleteAll}
        />
        {selectedHubKey ? (
          <HubPeerDetail
            peer={selectedHubPeer}
            lastName={
              selectedHubKey ? hubNamesRef.current.get(hubConnIdFromKey(selectedHubKey) ?? "") : undefined
            }
            paired={selectedHubPeer ? hubPairedIds.has(selectedHubPeer.conn_id) : false}
            onBack={() => setSelectedKey(null)}
          />
        ) : selectedContact ? (
          <Conversation
            key={selectedContact.key}
            device={selectedContact}
            entries={history[selectedContact.key] ?? []}
            animateAfter={animateRef.current.get(selectedContact.key) ?? 0}
            onBack={() => setSelectedKey(null)}
            onSend={(text) => send(selectedContact.key, text)}
            onAttach={() => attachAndSend(selectedContact.key)}
            onRetry={(id) => retry(selectedContact.key, id)}
            onDelete={() => deleteConversation(selectedContact.key)}
          />
        ) : selectedLan ? (
          <Conversation
            key={selectedLan.key}
            device={selectedLan}
            entries={history[selectedLan.key] ?? []}
            animateAfter={animateRef.current.get(selectedLan.key) ?? 0}
            onBack={() => setSelectedKey(null)}
            onSend={(text) => send(selectedLan.key, text)}
            onAttach={() => attachAndSend(selectedLan.key)}
            onRetry={(id) => retry(selectedLan.key, id)}
            onDelete={() => deleteConversation(selectedLan.key)}
          />
        ) : (
          <section className="conv-col" aria-label="Bienvenida">
            <div className="welcome">
              <h1>Pasá texto entre tus equipos</h1>
              <p>
                Sin internet, sin cuentas. Los dispositivos de tu misma red aparecen solos en la
                lista; elegí uno y escribí.
              </p>
            </div>
          </section>
        )}
      </div>

      {(pendingPin || pairingKey) && (
        <div className="pin-overlay" role="dialog" aria-label="Emparejar dispositivo">
          <div className="pin-card">
            <h3>
              {pairingKey
                ? `Vincular con ${displayName(pairingKey)}`
                : "Dispositivo protegido con PIN"}
            </h3>
            <p>
              {pinError ||
                (pairingKey
                  ? `En ${displayName(pairingKey)} va a aparecer un código de vinculación. Escribilo acá:`
                  : "Pedile el PIN que aparece en la otra app y escribilo para emparejar.")}
            </p>
            <input
              value={pinInput}
              onChange={(e) =>
                setPinInput(e.target.value.replace(/\D/g, "").slice(0, 6))
              }
              onKeyDown={(e) => {
                if (e.key === "Enter" && pinInput.length >= 4) void submitPin();
              }}
              placeholder="PIN de 6 dígitos"
              inputMode="numeric"
              autoFocus
            />
            <div className="pin-actions">
              <button
                type="button"
                className="pill pill-ghost"
                onClick={() => {
                  setPendingPin(null);
                  setPairingKey(null);
                  setPinError("");
                  setPinInput("");
                }}
              >
                Cancelar
              </button>
              <button
                type="button"
                className="pill"
                disabled={pinInput.length < 4}
                onClick={submitPin}
              >
                Emparejar
              </button>
            </div>
          </div>
        </div>
      )}

      {pairRequest && (
        <div className="pin-overlay" role="dialog" aria-label="Solicitud de vinculación">
          <div className="pin-card">
            <h3>Solicitud de vinculación</h3>
            <p>
              <strong>{pairRequest.from}</strong> quiere vincularse con esta app. En la otra
              máquina escribí este código:
            </p>
            <div className="pair-code">{pairRequest.code}</div>
            <p className="pin-hint">El código vence en 2 minutos.</p>
          </div>
        </div>
      )}

      <HubPairingRequests
        snapshot={hubPairingState}
        deferred={hubPairingDeferred}
        onCancel={hubCancelPairing}
        onCleared={clearHubPairing}
      />
    </div>
  );
}
