import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MessageSquare, Wifi } from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { DeviceList } from "./components/DeviceList";
import { Conversation } from "./components/Conversation";
import {
  demoRequested,
  discover,
  getDownloadFolder,
  isTauri,
  onFile,
  onMessage,
  onReadAck,
  sendAck,
  sendFile,
  sendText,
  setDownloadFolder as persistDownloadFolder,
  type RawDevice,
} from "./lib/backend";
import { DEMO_DEVICES, demoHistory, runDemoSim } from "./lib/demo";
import { loadHistory, saveHistory } from "./lib/history";
import { displayName, type DeviceState, type Entry, type History } from "./types";

const DEMO = typeof window === "undefined" || !isTauri() || demoRequested();

interface EntryDraft {
  mine: boolean;
  text: string;
  state?: Entry["state"];
  filePath?: string;
}

export default function App() {
  const [devices, setDevices] = useState<DeviceState[]>([]);
  const [history, setHistory] = useState<History>({});
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [scanning, setScanning] = useState(true);
  const [narrow, setNarrow] = useState(false);
  const [downloadFolder, setDownloadFolderState] = useState("");
  const [dragging, setDragging] = useState(false);

  const devicesRef = useRef<DeviceState[]>([]);
  const loadedRef = useRef(false);
  const animateRef = useRef<Map<string, number>>(new Map());
  const scanningRef = useRef(false);
  const selectedKeyRef = useRef<string | null>(null);

  useEffect(() => {
    devicesRef.current = devices;
  }, [devices]);

  useEffect(() => {
    selectedKeyRef.current = selectedKey;
  }, [selectedKey]);

  useEffect(() => {
    const mq = window.matchMedia("(max-width: 700px)");
    const apply = () => setNarrow(mq.matches);
    apply();
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  }, []);

  const pushEntry = useCallback(
    (key: string, draft: EntryDraft, wireId?: string): string => {
      const entry: Entry = {
        id: wireId ?? crypto.randomUUID(),
        at: Date.now(),
        state: draft.mine ? "sending" : undefined,
        ...draft,
      };
      setHistory((h) => ({ ...h, [key]: [...(h[key] ?? []), entry] }));
      setDevices((prev) =>
        prev.some((d) => d.key === key)
          ? prev
          : [...prev, { key, name: displayName(key), online: false }],
      );
      return entry.id;
    },
    [],
  );

  const patchEntry = useCallback((key: string, id: string, state: Entry["state"]) => {
    setHistory((h) => ({
      ...h,
      [key]: (h[key] ?? []).map((e) => (e.id === id ? { ...e, state } : e)),
    }));
  }, []);

  // Avisa al otro dispositivo que sus mensajes fueron vistos (palomitas azules).
  const notifyRead = useCallback((key: string, ids: string[]) => {
    if (ids.length === 0) return;
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
    console.log("notifyRead →", device.ip, ids.length, "ids");
    void sendAck(device.ip, payload);
  }, []);

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
    setHistory(loadHistory());
    loadedRef.current = true;
  }, [pushEntry]);

  useEffect(() => {
    if (DEMO || !loadedRef.current) return;
    saveHistory(history);
  }, [history]);

  const applyScan = useCallback((found: RawDevice[]) => {
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
      if (selectedKeyRef.current === msg.from) notifyRead(msg.from, [id]);
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
      if (selectedKeyRef.current === f.from) notifyRead(f.from, [id]);
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    return () => {
      cancelled = true;
      offs.forEach((fn) => fn());
    };
  }, [pushEntry, runScan, notifyRead]);

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
    }).then((fn) => {
      if (cancelled) fn();
      else offs.push(fn);
    });
    return () => {
      cancelled = true;
      offs.forEach((fn) => fn());
    };
  }, []);

  const sorted = useMemo(() => {
    const activity = (k: string) => history[k]?.[history[k].length - 1]?.at ?? 0;
    return [...devices].sort(
      (a, b) =>
        Number(b.online) - Number(a.online) ||
        activity(b.key) - activity(a.key) ||
        a.name.localeCompare(b.name),
    );
  }, [devices, history]);

  const selected = sorted.find((d) => d.key === selectedKey) ?? null;

  const openConversation = useCallback(
    (key: string) => {
      animateRef.current.set(key, Date.now());
      setSelectedKey(key);
      const unread = (history[key] ?? [])
        .filter((e) => !e.mine && !e.read)
        .map((e) => e.id);
      notifyRead(key, unread);
    },
    [history, notifyRead],
  );

  const send = useCallback(
    async (key: string, text: string) => {
      const device = devicesRef.current.find((d) => d.key === key);
      const id = crypto.randomUUID();
      setHistory((h) => ({
        ...h,
        [key]: [
          ...(h[key] ?? []),
          { id, mine: true, text, at: Date.now(), state: "sending" },
        ],
      }));
      try {
        if (device?.ip) {
          const estado = await sendText(device.ip, text, id);
          patchEntry(key, id, estado === "delivered" ? "delivered" : "sent");
        } else if (device) {
          await new Promise((r) => setTimeout(r, 400));
          patchEntry(key, id, "sent");
        } else throw new Error("desconocido");
      } catch (e) {
        console.error("send falló:", e);
        patchEntry(key, id, "failed");
      }
    },
    [patchEntry],
  );

  const retry = useCallback(
    (key: string, id: string) => {
      const entry = (history[key] ?? []).find((e) => e.id === id);
      if (!entry) return;
      patchEntry(key, id, "sending");
      const device = devicesRef.current.find((d) => d.key === key);
      const deliver = async () => {
        try {
          if (entry.filePath && device?.ip) {
            const estado = await sendFile(device.ip, entry.filePath, id);
            patchEntry(key, id, estado === "delivered" ? "delivered" : "sent");
          } else if (!entry.filePath && device?.ip) {
            const estado = await sendText(device.ip, entry.text, id);
            patchEntry(key, id, estado === "delivered" ? "delivered" : "sent");
          } else if (device) {
            await new Promise((r) => setTimeout(r, 400));
            patchEntry(key, id, "sent");
          } else throw new Error("desconocido");
        } catch (e) {
          console.error("reintento falló:", e);
          patchEntry(key, id, "failed");
        }
      };
      deliver();
    },
    [history, patchEntry],
  );

  const deleteConversation = useCallback((key: string) => {
    setHistory((h) => {
      const next = { ...h };
      delete next[key];
      return next;
    });
  }, []);

  const deleteAll = useCallback(() => setHistory({}), []);

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
      if (!device?.ip) return;
      const name = path.split(/[\\/]/).pop() ?? path;
      const id = crypto.randomUUID();
      setHistory((h) => ({
        ...h,
        [key]: [
          ...(h[key] ?? []),
          {
            id,
            mine: true,
            text: `📎 ${name}`,
            at: Date.now(),
            state: "sending",
            filePath: path,
          },
        ],
      }));
      try {
        const estado = await sendFile(device.ip, path, id);
        patchEntry(key, id, estado === "delivered" ? "delivered" : "sent");
      } catch (e) {
        console.error("send_file falló:", e);
        patchEntry(key, id, "failed");
      }
    },
    [patchEntry],
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
          <span className="demo-badge">Demostración</span>
        ) : (
          <span className="net-chip">
            <Wifi size={13} aria-hidden />
            Red local
          </span>
        )}
      </header>

      <div className={`shell ${narrow && selected ? "show-conv" : ""}`}>
        <DeviceList
          devices={sorted}
          history={history}
          scanning={scanning}
          selectedKey={selectedKey}
          onSelect={openConversation}
          onRescan={runScan}
          onPickFolder={pickDownloadFolder}
          downloadFolder={downloadFolder}
          onDeleteAll={deleteAll}
        />
        {selected ? (
          <Conversation
            key={selected.key}
            device={selected}
            entries={history[selected.key] ?? []}
            animateAfter={animateRef.current.get(selected.key) ?? 0}
            onBack={() => setSelectedKey(null)}
            onSend={(text) => send(selected.key, text)}
            onAttach={() => attachAndSend(selected.key)}
            onRetry={(id) => retry(selected.key, id)}
            onDelete={() => deleteConversation(selected.key)}
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
    </div>
  );
}
