import { useCallback, useEffect, useRef, useState } from "react";
import { FolderOpen, Search, X } from "lucide-react";
import type { DeviceState, Entry, History } from "../types";
import { HUB_PAIRED_TAG, canDeleteRow, isContactKey } from "../lib/hub-chat-core";
import { HUB_PEER_PREVIEW } from "../lib/hub-presence";
import { Avatar } from "./Avatar";
import { Menu } from "./Menu";
import { useDismiss } from "./useDismiss";

const DAY = 86_400_000;

function previewTime(at: number): string {
  const d = new Date(at);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  if (sameDay) return d.toLocaleTimeString("es", { hour: "2-digit", minute: "2-digit" });
  if (now.getTime() - at < DAY) return "Ayer";
  return d.toLocaleDateString("es", { day: "2-digit", month: "2-digit" });
}

function Row({
  device,
  entry,
  paired,
  selected,
  entering,
  delay,
  canDelete,
  confirming,
  onSelect,
  onAskDelete,
  onConfirmDelete,
  onCancelDelete,
}: {
  device: DeviceState;
  entry?: Entry;
  paired: boolean;
  selected: boolean;
  entering: boolean;
  delay: number;
  canDelete: boolean;
  confirming: boolean;
  onSelect: () => void;
  onAskDelete: () => void;
  onConfirmDelete: () => void;
  onCancelDelete: () => void;
}) {
  const isHub = device.kind === "hub";
  const isContact = isContactKey(device.key);
  const showsConversation = !isHub || isContact;
  const hubName = isHub ? device.name || "Navegador" : device.name;

  const confirmRef = useRef<HTMLDivElement>(null);
  const confirmBtnRef = useRef<HTMLButtonElement>(null);
  useDismiss(confirmRef, confirming, onCancelDelete);

  useEffect(() => {
    if (confirming) confirmBtnRef.current?.focus();
  }, [confirming]);

  const classes = `device-row ${selected ? "is-selected" : ""} ${entering ? "is-entering" : ""} ${
    device.online ? "" : "is-offline"
  } ${confirming ? "is-confirm" : ""}`;

  return (
    <div
      className={classes}
      style={delay ? { animationDelay: `${delay}ms` } : undefined}
      ref={confirming ? confirmRef : undefined}
    >
      {confirming ? (
        <>
          <span className="device-del-q">¿Borrar?</span>
          <button type="button" ref={confirmBtnRef} className="pill device-del-yes" onClick={onConfirmDelete}>
            Borrar
          </button>
          <button type="button" className="device-del-no" onClick={onCancelDelete}>
            Cancelar
          </button>
        </>
      ) : (
        <>
          <button
            type="button"
            className="device-row-main"
            aria-current={selected ? "true" : undefined}
            onClick={onSelect}
          >
            <Avatar name={hubName} online={device.online} />
            <span className="device-text">
              <span className="device-name">
                {hubName}
                {isHub && !isContact && device.name && <span className="hub-tag">Navegador</span>}
                {paired && <span className="hub-tag">{HUB_PAIRED_TAG}</span>}
              </span>
              <span className="device-preview">
                {showsConversation
                  ? device.online || entry
                    ? entry
                      ? `${entry.mine ? "Vos: " : ""}${entry.text}`
                      : "Sin mensajes todavía"
                    : "Desconectado"
                  : // Presencia del hub: solo avisar, sin prometer conversación.
                    HUB_PEER_PREVIEW}
              </span>
            </span>
            {entry && showsConversation && (
              <span className="device-time">{previewTime(entry.at)}</span>
            )}
          </button>
          {canDelete && (
            <button
              type="button"
              className="device-del"
              aria-label={`Borrar conversación con ${hubName}`}
              onClick={onAskDelete}
            >
              <X size={14} aria-hidden />
            </button>
          )}
        </>
      )}
    </div>
  );
}

export function DeviceList({
  devices,
  history,
  pairedKeys,
  scanning,
  selectedKey,
  onSelect,
  onRescan,
  onPickFolder,
  downloadFolder,
  onRegeneratePin,
  onDeleteAll,
  onDeleteConversation,
}: {
  devices: DeviceState[];
  history: History;
  pairedKeys: Set<string>;
  scanning: boolean;
  selectedKey: string | null;
  onSelect: (key: string) => void;
  onRescan: () => void;
  onPickFolder: () => void;
  downloadFolder: string;
  onRegeneratePin: () => void;
  onDeleteAll: () => void;
  onDeleteConversation: (key: string) => void;
}) {
  const knownRef = useRef<Set<string> | null>(null);
  const enteringRef = useRef<Set<string>>(new Set());
  const delayRef = useRef<Map<string, number>>(new Map());
  const [confirmKey, setConfirmKey] = useState<string | null>(null);

  // Solo una fila confirma a la vez: abrir otra (o elegir la conversación de
  // otra fila) cancela la confirmación pendiente.
  const cancelConfirm = useCallback(() => setConfirmKey(null), []);
  const handleSelect = useCallback(
    (key: string) => {
      setConfirmKey(null);
      onSelect(key);
    },
    [onSelect],
  );
  const askDelete = useCallback((key: string) => setConfirmKey(key), []);
  const confirmDelete = useCallback(
    (key: string) => {
      setConfirmKey(null);
      onDeleteConversation(key);
    },
    [onDeleteConversation],
  );

  if (knownRef.current === null) {
    knownRef.current = new Set();
    devices.forEach((d, i) => {
      knownRef.current!.add(d.key);
      enteringRef.current.add(d.key);
      delayRef.current.set(d.key, Math.min(i * 45, 360));
    });
  } else {
    for (const d of devices) {
      if (!knownRef.current.has(d.key)) {
        knownRef.current.add(d.key);
        enteringRef.current.add(d.key);
        delayRef.current.set(d.key, 0);
      }
    }
  }

  const onlineCount = devices.filter((d) => d.online).length;

  return (
    <section className="list-col" aria-label="Dispositivos en la red">
      <header className="list-head">
        <div className="list-title">
          <h2>Dispositivos</h2>
          {devices.length > 0 && (
            <span className="list-count">
              {onlineCount} en línea · {devices.length}
            </span>
          )}
        </div>
        <button
          type="button"
          className="icon-btn"
          aria-label="Carpeta de descargas"
          title={downloadFolder ? `Guardando en: ${downloadFolder}` : "Carpeta de descargas"}
          onClick={onPickFolder}
        >
          <FolderOpen size={18} />
        </button>
        <button
          type="button"
          className={`icon-btn`}
          aria-label="Buscar dispositivos"
          title="Buscar dispositivos"
          onClick={onRescan}
          disabled={scanning}
        >
          <Search size={18} />
        </button>
        <Menu
          label="Más opciones"
          items={[
            { label: "Regenerar mi PIN", action: onRegeneratePin },
            {
              label: "Borrar todo el historial",
              danger: true,
              action: onDeleteAll,
            },
          ]}
        />
      </header>

      <div className="list-scroll">
        {devices.length === 0 && (
          <div className="list-empty" role="status">
            {scanning ? (
              <>
                <div className="shimmer-row" style={{ width: "72%" }} />
                <div className="shimmer-row" style={{ width: "58%" }} />
                <div className="shimmer-row" style={{ width: "65%" }} />
                <p>Buscando dispositivos…</p>
              </>
            ) : (
              <p>
                Nadie más está en la red todavía. Abrí LAN-Chat en otro equipo de la misma red y
                va a aparecer acá solo.
              </p>
            )}
          </div>
        )}

        {devices.map((d) => {
          const entries = history[d.key] ?? [];
          return (
            <Row
              key={d.key}
              device={d}
              entry={entries[entries.length - 1]}
              paired={pairedKeys.has(d.key)}
              selected={d.key === selectedKey}
              entering={enteringRef.current.has(d.key)}
              delay={delayRef.current.get(d.key) ?? 0}
              canDelete={canDeleteRow(history, d.key)}
              confirming={confirmKey === d.key}
              onSelect={() => handleSelect(d.key)}
              onAskDelete={() => askDelete(d.key)}
              onConfirmDelete={() => confirmDelete(d.key)}
              onCancelDelete={cancelConfirm}
            />
          );
        })}
      </div>
    </section>
  );
}
