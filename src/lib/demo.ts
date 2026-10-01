import type { DeviceState, Entry, History } from "../types";

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

export const DEMO_DEVICES: DeviceState[] = [
  { key: "demo:valeria", name: "MacBook de Valeria", online: true },
  { key: "demo:bruno", name: "Pixel de Bruno", online: true },
  { key: "demo:estudio", name: "Mac mini Estudio", online: false },
];

export function demoHistory(): History {
  const now = Date.now();
  const today = (h: number, m: number) => {
    const d = new Date(now);
    d.setHours(h, m, 0, 0);
    return d.getTime();
  };
  const ago = (ms: number) => now - ms;

  const entry = (mine: boolean, text: string, at: number, state?: Entry["state"]): Entry => ({
    id: crypto.randomUUID(),
    mine,
    text,
    at,
    state,
  });

  return {
    "demo:valeria": [
      entry(false, "¿Me pasás el resumen que armamos ayer?", today(9, 3)),
      entry(true, "Sí, te lo mando en un toque", today(9, 4), "sent"),
      entry(false, "Buen día! ¿arrancamos a las 10?", today(9, 31)),
      entry(true, "Dale, entro a las 10 y comparto pantalla", today(9, 32), "sent"),
    ],
    "demo:bruno": [
      entry(false, "Che, dejame saber cuando llegue la página nueva", today(8, 12)),
      entry(false, "Quedó guardada en la carpeta compartida", today(8, 14)),
    ],
    "demo:estudio": [
      entry(true, "Cierro el equipo, dejo el respaldo corriendo", ago(2 * DAY + 5 * HOUR), "sent"),
      entry(false, "Dale, yo apago cuando termine", ago(2 * DAY + 3 * HOUR)),
      entry(false, "Listo, respaldo terminó bien", ago(2 * DAY)),
    ],
  };
}

export function runDemoSim(
  onDevice: (device: DeviceState) => void,
  onMessage: (key: string, text: string) => void,
): () => void {
  const timers: ReturnType<typeof setTimeout>[] = [];

  timers.push(
    setTimeout(() => {
      onDevice({ key: "demo:camila", name: "iPad de Camila", online: true });
    }, 9_000),
  );

  timers.push(
    setTimeout(() => {
      onMessage("demo:bruno", "¿Lo viste? Lo dejé en la misma carpeta");
    }, 16_000),
  );

  timers.push(
    setTimeout(() => {
      onMessage("demo:camila", "Hola! Ya estoy en la red");
    }, 26_000),
  );

  return () => timers.forEach(clearTimeout);
}
