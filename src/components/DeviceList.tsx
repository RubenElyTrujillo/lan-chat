import { useRef } from "react";
import { Search } from "lucide-react";
import type { DeviceState, Entry, History } from "../types";
import { Avatar } from "./Avatar";
import { Menu } from "./Menu";

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
  selected,
  entering,
  delay,
  onSelect,
}: {
  device: DeviceState;
  entry?: Entry;
  selected: boolean;
  entering: boolean;
  delay: number;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      className={`device-row ${selected ? "is-selected" : ""} ${entering ? "is-entering" : ""} ${
        device.online ? "" : "is-offline"
      }`}
      style={delay ? { animationDelay: `${delay}ms` } : undefined}
      aria-current={selected ? "true" : undefined}
      onClick={onSelect}
    >
      <Avatar name={device.name} online={device.online} />
      <span className="device-text">
        <span className="device-name">{device.name}</span>
        <span className="device-preview">
          {device.online || entry
            ? entry
              ? `${entry.mine ? "Vos: " : ""}${entry.text}`
              : "Sin mensajes todavía"
            : "Desconectado"}
        </span>
      </span>
      {entry && <span className="device-time">{previewTime(entry.at)}</span>}
    </button>
  );
}

export function DeviceList({
  devices,
  history,
  scanning,
  selectedKey,
  onSelect,
  onRescan,
  onDeleteAll,
}: {
  devices: DeviceState[];
  history: History;
  scanning: boolean;
  selectedKey: string | null;
  onSelect: (key: string) => void;
  onRescan: () => void;
  onDeleteAll: () => void;
}) {
  const knownRef = useRef<Set<string> | null>(null);
  const enteringRef = useRef<Set<string>>(new Set());
  const delayRef = useRef<Map<string, number>>(new Map());

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
          className={`icon-btn ${scanning ? "is-spinning" : ""}`}
          aria-label="Buscar dispositivos"
          title="Buscar dispositivos"
          onClick={onRescan}
          disabled={scanning}
        >
          <Search size={18} />
        </button>
        <Menu
          label="Más opciones"
          items={[{ label: "Borrar todo el historial", danger: true, action: onDeleteAll }]}
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
              selected={d.key === selectedKey}
              entering={enteringRef.current.has(d.key)}
              delay={delayRef.current.get(d.key) ?? 0}
              onSelect={() => onSelect(d.key)}
            />
          );
        })}
      </div>
    </section>
  );
}
