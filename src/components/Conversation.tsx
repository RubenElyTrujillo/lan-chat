import { useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Check, CheckCheck, ChevronLeft, RotateCcw } from "lucide-react";
import { openPath } from "@tauri-apps/plugin-opener";
import { isPreviewableImage, type DeviceState, type Entry } from "../types";
import { Avatar } from "./Avatar";
import { Menu } from "./Menu";

const GROUP_WINDOW = 5 * 60_000;

function dayLabel(at: number): string {
  const d = new Date(at);
  const now = new Date();
  const days = Math.floor(
    (new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() -
      new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()) /
      86_400_000,
  );
  if (days === 0) return "Hoy";
  if (days === 1) return "Ayer";
  const opts: Intl.DateTimeFormatOptions = { day: "numeric", month: "long" };
  if (d.getFullYear() !== now.getFullYear()) opts.year = "numeric";
  return d.toLocaleDateString("es", opts);
}

function Bubble({
  entry,
  grouped,
  animate,
  onRetry,
}: {
  entry: Entry;
  grouped: boolean;
  animate: boolean;
  onRetry: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const time = new Date(entry.at).toLocaleTimeString("es", {
    hour: "2-digit",
    minute: "2-digit",
  });
  const failed = entry.state === "failed";
  const hasFile = !!entry.filePath;
  const isImage = hasFile && isPreviewableImage(entry.text);

  const copyText = async () => {
    try {
      await navigator.clipboard.writeText(entry.text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {
      /* portapapeles no disponible */
    }
  };

  return (
    <div
      className={`bubble-row ${entry.mine ? "is-mine" : "is-theirs"} ${
        grouped ? "is-grouped" : ""
      } ${animate ? "is-new" : ""}`}
    >
      <button
        type="button"
        className={`bubble ${failed ? "is-failed" : ""}`}
        onClick={
          failed
            ? onRetry
            : hasFile
              ? () => openPath(entry.filePath!)
              : copyText
        }
        title={
          failed
            ? "Tocá para reintentar"
            : hasFile
              ? "Abrir archivo"
              : "Tocar para copiar"
        }
      >
        {isImage && entry.filePath && (
          <img
            src={convertFileSrc(entry.filePath)}
            className="bubble-img"
            alt={entry.text.replace("📎 ", "")}
          />
        )}
        <span className="bubble-text">
          {isImage ? entry.text.replace("📎 ", "") : entry.text}
        </span>
        <span className="bubble-meta">
          {copied && <span className="copied-hint">Copiado</span>}
          {entry.mine && entry.state === "sent" && <Check size={12} aria-hidden />}
          {entry.mine && entry.state === "delivered" && (
            <CheckCheck size={12} aria-hidden />
          )}
          {entry.mine && entry.state === "read" && (
            <CheckCheck size={12} className="is-read" aria-hidden />
          )}
          {time}
        </span>
      </button>
      {failed && (
        <span className="bubble-fail">
          <RotateCcw size={11} aria-hidden />
          Se reenviará cuando vuelva a conectar · Tocá para forzar
        </span>
      )}
    </div>
  );
}

export function Conversation({
  device,
  entries,
  animateAfter,
  onBack,
  onSend,
  onAttach,
  onRetry,
  onDelete,
}: {
  device: DeviceState;
  entries: Entry[];
  animateAfter: number;
  onBack: () => void;
  onSend: (text: string) => void;
  onAttach: () => void;
  onRetry: (id: string) => void;
  onDelete: () => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const prevKeyRef = useRef(device.key);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const switched = prevKeyRef.current !== device.key;
    prevKeyRef.current = device.key;
    el.scrollTo({ top: el.scrollHeight, behavior: switched ? "auto" : "smooth" });
  }, [entries.length, device.key]);

  return (
    <section className="conv-col" aria-label={`Conversación con ${device.name}`}>
      <header className="conv-head">
        <button type="button" className="icon-btn is-back" aria-label="Volver a dispositivos" onClick={onBack}>
          <ChevronLeft size={20} />
        </button>
        <Avatar name={device.name} online={device.online} size={36} />
        <div className="conv-title">
          <h3>{device.name}</h3>
          <span className={`conv-status ${device.online ? "" : "is-off"}`}>
            {device.online ? "En línea" : "Desconectado"}
          </span>
        </div>
        <Menu
          label="Más opciones"
          items={[{ label: "Borrar conversación", danger: true, action: onDelete }]}
        />
      </header>

      <div className="conv-scroll" ref={scrollRef} role="log" aria-live="polite">
        {entries.length === 0 ? (
          <div className="conv-empty">
            <p className="conv-empty-title">Todavía no hay mensajes con {device.name}</p>
            <p className="conv-empty-sub">
              Escribí abajo y va a salir directo por la red local.
            </p>
          </div>
        ) : (
          <div className="conv-thread">
            {entries.map((e, i) => {
              const prev = entries[i - 1];
              const newDay = !prev || dayLabel(prev.at) !== dayLabel(e.at);
              const grouped =
                !!prev &&
                !newDay &&
                prev.mine === e.mine &&
                e.at - prev.at < GROUP_WINDOW;
              return (
                <div key={e.id} className="thread-unit">
                  {newDay && (
                    <div className="day-chip" aria-hidden>
                      {dayLabel(e.at)}
                    </div>
                  )}
                  <Bubble
                    entry={e}
                    grouped={grouped}
                    animate={e.at > animateAfter}
                    onRetry={() => onRetry(e.id)}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>

      <Composer onSend={onSend} onAttach={onAttach} />
    </section>
  );
}

function Composer({
  onSend,
  onAttach,
}: {
  onSend: (text: string) => void;
  onAttach: () => void;
}) {
  const [text, setText] = useState("");
  const areaRef = useRef<HTMLTextAreaElement>(null);

  const grow = () => {
    const el = areaRef.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight, 120)}px`;
  };

  const send = () => {
    const trimmed = text.trim();
    if (!trimmed) return;
    onSend(trimmed);
    setText("");
    requestAnimationFrame(grow);
  };

  return (
    <footer className="composer">
      <button
        type="button"
        className="icon-btn"
        aria-label="Adjuntar archivo"
        title="Adjuntar archivo"
        onClick={onAttach}
      >
        <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden>
          <path d="M9 3.5v11M3.5 9h11" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
        </svg>
      </button>
      <div className="composer-field">
        <textarea
          ref={areaRef}
          rows={1}
          value={text}
          maxLength={500}
          placeholder="Escribe un mensaje"
          aria-label="Escribe un mensaje"
          onChange={(e) => {
            setText(e.target.value);
            grow();
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
        />
        {text.length > 460 && <span className="composer-count">{500 - text.length}</span>}
      </div>
      <button type="button" className="pill" onClick={send} disabled={!text.trim()}>
        Enviar
      </button>
    </footer>
  );
}
