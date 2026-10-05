import { useEffect, useMemo, useRef, useState } from "react";
import {
  formatCountdown,
  isExpired,
  receiptDeadline,
  remainingMs,
  type HubPairingSnapshot,
  type HubPendingPairing,
} from "../lib/hub-pairing";

// Solicitudes de vinculación de navegadores vía hub: tarjetas compactas con
// el código de 8 dígitos. Carril propio, separado de la vinculación LAN
// (PIN): sin cooldown, sin input, sin superponerse al modal LAN — cuando hay
// un modal LAN abierto se difieren (el estado sigue vivo en el backend).
// El código vive SOLO acá: no se copia solo, no se registra, no viaja.
function entryKey(p: { conn_id: string; req_id: string }): string {
  return `${p.conn_id}|${p.req_id}`;
}

function peerDisplayName(name: string): string {
  const trimmed = name.trim();
  return trimmed.length > 0 ? trimmed : "Navegador";
}

export function HubPairingRequests({
  snapshot,
  deferred,
  onCancel,
  onCleared,
}: {
  snapshot: HubPairingSnapshot;
  deferred: boolean;
  onCancel: (connId: string, reqId: string) => Promise<boolean>;
  onCleared: (connId: string, reqId: string) => void;
}) {
  const pending = snapshot.pending;
  // Vencimiento anclado al momento local de recepción del snapshot
  // (monótono en la práctica: cada entrada fija su deadline una sola vez).
  const deadlinesRef = useRef<Map<string, number>>(new Map());
  for (const p of pending) {
    const key = entryKey(p);
    if (!deadlinesRef.current.has(key)) {
      deadlinesRef.current.set(key, receiptDeadline(p.expires_in_ms, Date.now()));
    }
  }

  const [now, setNow] = useState(() => Date.now());
  const [canceling, setCanceling] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Set<string>>(new Set());

  const liveKeys = useMemo(
    () => pending.map(entryKey),
    [pending],
  );

  // Un tick por segundo SOLO mientras haya solicitudes vivas: sin polling
  // eterno. El intervalo se limpia al desmontar y cuando la lista se vacía.
  useEffect(() => {
    if (pending.length === 0) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [pending.length]);

  // Purga de deadlines de entradas que ya no están (vencidas, canceladas o
  // reemplazadas): el mapa queda acotado a lo visible.
  useEffect(() => {
    const live = new Set(liveKeys);
    for (const key of deadlinesRef.current.keys()) {
      if (!live.has(key)) deadlinesRef.current.delete(key);
    }
  }, [liveKeys]);

  const cancel = (p: HubPendingPairing) => {
    // Captura al click: la respuesta tardía sólo puede afectar ESTA entrada;
    // una solicitud nueva (misma conn, otro req) nunca la reemplaza.
    const { conn_id, req_id } = p;
    const key = entryKey(p);
    setCanceling((s) => new Set(s).add(key));
    setErrors((s) => {
      const next = new Set(s);
      next.delete(key);
      return next;
    });
    void onCancel(conn_id, req_id)
      .catch(() => undefined)
      .then((matched?: boolean) => {
        if (matched) {
          // Confirmado por el backend: se va de la vista local; el snapshot
          // emitido por el backend llega después y confirma lo mismo.
          setCanceling((s) => {
            const next = new Set(s);
            next.delete(key);
            return next;
          });
          onCleared(conn_id, req_id);
          return;
        }
        setCanceling((s) => {
          const next = new Set(s);
          next.delete(key);
          return next;
        });
        setErrors((s) => new Set(s).add(key));
      });
  };

  if (deferred || pending.length === 0) return null;

  return (
    <div className="hub-pair" role="dialog" aria-label="Solicitudes de vinculación de navegadores">
      <p className="hub-pair-heading">Vinculación de navegador</p>
      {pending.map((p) => {
        const key = entryKey(p);
        const deadline = deadlinesRef.current.get(key) ?? now;
        const expired = isExpired(deadline, now);
        return (
          <article className="hub-pair-card" key={key}>
            <div className="hub-pair-info">
              <span className="hub-pair-name">{peerDisplayName(p.name)}</span>
              <span className="hub-pair-why">quiere vincularse con esta app</span>
            </div>
            {expired ? (
              <span className="hub-pair-expired">Código vencido</span>
            ) : (
              <>
                <span className="hub-pair-code" aria-label="Código de vinculación">
                  {p.code}
                </span>
                <span className="hub-pair-timer">
                  Vence en {formatCountdown(remainingMs(deadline, now))}
                </span>
              </>
            )}
            <button
              type="button"
              className="pill pill-ghost hub-pair-cancel"
              disabled={canceling.has(key) || expired}
              onClick={() => cancel(p)}
            >
              {canceling.has(key) ? "Cancelando…" : "Cancelar"}
            </button>
            {errors.has(key) && (
              <p className="hub-pair-error" role="alert">
                No se pudo cancelar esta solicitud. Intentá de nuevo.
              </p>
            )}
          </article>
        );
      })}
    </div>
  );
}
