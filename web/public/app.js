// LAN-Chat Web — cliente del hub (presencia + relay), UI portada de la desktop.
// Estado efímero en memoria: ver client-state.js. Nada se persiste.
import {
  createSession,
  sendFile,
  sendTextMessage,
  handlePageHide,
  handlePageShow,
} from "./client-state.js";
import { createConnection } from "./connection.js";
import { createPairingAttempts } from "./pairing-attempts.js";

const $ = (id) => document.getElementById(id);

const session = createSession();
let myName = null;
let connecting = false;
const pairedApps = new Set(); // ids de apps emparejadas (solo esta sesión)
let pairingTargetId = null;

const proto = location.protocol === "https:" ? "wss" : "ws";
const MAX_FILE = 25 * 1024 * 1024;
const GROUP_WINDOW = 5 * 60_000;

// Un solo socket activo: los callbacks van atados a su propio socket y los
// eventos de un socket reemplazado se ignoran (ver connection.js).
const conn = createConnection({
  url: `${proto}://${location.host}/hub`,
  onOpen: (socket) =>
    socket.send(JSON.stringify({ type: "hello", name: myName, kind: "web" })),
  onMessage: (socket, ev) => {
    let m;
    try {
      m = JSON.parse(ev.data);
    } catch {
      return;
    }
    try {
      handle(m);
    } catch {
      /* un payload malformado no debe tirar abajo el cliente */
    }
  },
  onClose: () => onConnectionLost(),
});

// Intentos de vinculación salientes: cancelar, timeout o un intento más nuevo
// dejan sin efecto a cualquier pair-ok tardío (ver pairing-attempts.js).
const pairingAttempts = createPairingAttempts({
  timeoutMs: 6000,
  onTimeout: (appId) => {
    if (appId === pairingTargetId) {
      $("pairing-err").textContent = "No respondió. ¿Es el código correcto?";
    }
  },
});

// ── Helpers de la desktop ────────────────────────────────
const PASTELS = ["#E9E3F8", "#F6EAD3", "#DCEEE0", "#F9E3DD", "#DCE9F6", "#EFE9D8"];

function initials(name) {
  const words = name.trim().split(/\s+/);
  if (!words[0]) return "?";
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[words.length - 1][0]).toUpperCase();
}

function tone(name) {
  let hash = 0;
  for (let i = 0; i < name.length; i++) hash = (hash * 31 + name.charCodeAt(i)) | 0;
  return PASTELS[Math.abs(hash) % PASTELS.length];
}

function previewTime(at) {
  const d = new Date(at);
  const now = new Date();
  if (d.toDateString() === now.toDateString())
    return d.toLocaleTimeString("es", { hour: "2-digit", minute: "2-digit" });
  if (now.getTime() - at < 86_400_000) return "Ayer";
  return d.toLocaleDateString("es", { day: "2-digit", month: "2-digit" });
}

function dayLabel(at) {
  const d = new Date(at);
  const now = new Date();
  const days = Math.floor(
    (new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() -
      new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()) /
      86_400_000,
  );
  if (days === 0) return "Hoy";
  if (days === 1) return "Ayer";
  const opts = { day: "numeric", month: "long" };
  if (d.getFullYear() !== now.getFullYear()) opts.year = "numeric";
  return d.toLocaleDateString("es", opts);
}

function clockTime(at) {
  return new Date(at).toLocaleTimeString("es", { hour: "2-digit", minute: "2-digit" });
}

function isPreviewableImage(name) {
  return /\.(jpe?g|png|gif|webp|bmp)$/i.test(name || "");
}

function fmtSize(bytes) {
  if (bytes == null) return "";
  if (bytes > 1048576) return (bytes / 1048576).toFixed(1) + " MB";
  return Math.ceil(bytes / 1024) + " KB";
}

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
}

function avatarEl(name, online, size) {
  const a = el("span", "avatar", initials(name));
  a.style.width = a.style.height = `${size || 40}px`;
  a.style.background = tone(name);
  a.style.fontSize = size === 36 ? "12.5px" : "13.5px";
  if (online !== undefined) {
    a.appendChild(el("span", `avatar-dot ${online ? "is-online" : "is-offline"}`));
  }
  return a;
}

// ── Entrada ──────────────────────────────────────────────
$("gate-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const name = $("name").value.trim();
  if (!name) {
    $("landing-error").textContent = "Poné tu nombre";
    return;
  }
  connect(name);
});

$("name").addEventListener("input", () => ($("landing-error").textContent = ""));

function connect(name) {
  // Un intento en curso (o activo) gana: no abrir sockets duplicados.
  if (connecting || conn.isOpen() || (conn.socket && conn.socket.readyState === 0)) return;
  connecting = true;
  myName = name;
  $("btn-enter").disabled = true;
  conn.connect();
}

function onConnectionLost() {
  connecting = false;
  $("btn-enter").disabled = false;
  if (!session.myId) {
    // Falla de la conexión inicial: restaurar el formulario de entrada.
    $("landing-error").textContent = "No se pudo conectar con el hub. Probá de nuevo.";
    return;
  }
  markDisconnected();
}

function markDisconnected() {
  session.setSocketOpen(false);
  pairingAttempts.cancel();
  pairingTargetId = null;
  $("pairing").classList.add("hidden");
  $("offline").classList.remove("hidden");
  $("conn-chip").lastChild.textContent = "Sin conexión";
  renderDevices();
  renderView();
}

$("btn-reload").onclick = () => location.reload();

// ── Mensajes del hub ─────────────────────────────────────
function handle(m) {
  if (m.type === "welcome") {
    connecting = false;
    $("btn-enter").disabled = false;
    $("landing-error").textContent = "";
    session.setIdentity(m.id, myName);
    session.setSocketOpen(true);
    $("landing").classList.add("hidden");
    $("shell").classList.remove("hidden");
    renderDevices();
    renderView();
    return;
  }
  if (m.type === "peers") {
    const others = (m.list || []).filter((p) => p.id !== session.myId);
    session.applyPeers(others);
    renderDevices();
    renderView();
    return;
  }
  if (m.type === "relay") {
    handleRelay(m);
  }
}

function handleRelay(m) {
  const p = m.payload;
  if (!p || typeof p.type !== "string") return;
  switch (p.type) {
    case "pair-request": {
      // Una app quiere vincularse: grupo confiable, respondemos OK por esta sesión.
      pairedApps.add(m.from_id);
      relayTo(m.from_id, { type: "pair-ok" });
      session.system(m.from_id, `Vinculada la app de escritorio de ${m.from_name}.`);
      renderDevices();
      if (session.selected() === m.from_id) renderView();
      break;
    }
    case "pair-ok": {
      // Solo completa el intento vigente: respuestas tardías o de otra app
      // se ignoran y no navegan la UI.
      if (pairingAttempts.accept(m.from_id)) {
        pairedApps.add(m.from_id);
        pairingTargetId = null;
        $("pairing").classList.add("hidden");
        openConversation(m.from_id);
      }
      break;
    }
    case "chat": {
      if (p.kind === "app" && !pairedApps.has(m.from_id)) return;
      const entry = session.addIncoming(m.from_id, m.from_name, p);
      if (!entry) return;
      renderDevices();
      if (session.selected() === m.from_id) renderView();
      break;
    }
    case "file": {
      if (p.kind === "app" && !pairedApps.has(m.from_id)) return;
      const entry = session.addIncoming(m.from_id, m.from_name, p);
      if (!entry) return;
      renderDevices();
      if (session.selected() === m.from_id) renderView();
      break;
    }
  }
}

function relayTo(peerId, payload) {
  return conn.send({ type: "relay", to: peerId, payload });
}

function relayFile(peerId, { name, bytes, id }) {
  const buf = bytes;
  let binary = "";
  for (let i = 0; i < buf.length; i += 0x8000) {
    binary += String.fromCharCode(...buf.subarray(i, i + 0x8000));
  }
  const ok = conn.send({
    type: "relay",
    to: peerId,
    payload: { type: "file", name, data: btoa(binary), kind: "web", id },
  });
  if (!ok) throw new Error("no-connection");
}

// ── Lista de dispositivos ────────────────────────────────
// knownDevices: ids ya vistos (una sola animación de entrada por id).
// enterDelays: entradas pendientes de pintar; se consumen al crear la fila.
let knownDevices = null;
const enterDelays = new Map();

function sortedConversations() {
  const activity = (c) => c.messages[c.messages.length - 1]?.at ?? 0;
  return session.conversations().sort(
    (a, b) =>
      Number(b.online) - Number(a.online) ||
      activity(b) - activity(a) ||
      (a.name || "").localeCompare(b.name || ""),
  );
}

function renderDevices() {
  const convs = sortedConversations();
  const box = $("device-list");
  const onlineCount = convs.filter((c) => c.online).length;
  $("list-count").textContent =
    convs.length > 0 ? `${onlineCount} en línea · ${convs.length}` : "";

  if (knownDevices === null) {
    knownDevices = new Set();
    convs.forEach((c, i) => {
      knownDevices.add(c.id);
      enterDelays.set(c.id, Math.min(i * 45, 360));
    });
  } else {
    for (const c of convs) {
      if (!knownDevices.has(c.id)) {
        knownDevices.add(c.id);
        enterDelays.set(c.id, 0);
      }
    }
  }

  box.textContent = "";
  if (convs.length === 0) {
    const empty = el("div", "list-empty");
    empty.setAttribute("role", "status");
    empty.appendChild(
      el(
        "p",
        null,
        "Nadie más está conectado a este hub por ahora. Abrí LAN-Chat en otro dispositivo o navegador y va a aparecer acá.",
      ),
    );
    box.appendChild(empty);
    return;
  }

  for (const conv of convs) {
    const enterDelay = enterDelays.get(conv.id);
    const row = el(
      "button",
      `device-row ${conv.id === session.selected() ? "is-selected" : ""} ${
        conv.online ? "" : "is-offline"
      } ${enterDelay !== undefined ? "is-entering" : ""}`,
    );
    row.type = "button";
    if (enterDelay !== undefined) {
      row.style.animationDelay = `${enterDelay}ms`;
      // Consumir el marcador: los re-render del mismo id no re-animan.
      enterDelays.delete(conv.id);
    }
    row.setAttribute("aria-label", `Conversación con ${conv.name || "dispositivo"}`);
    if (conv.id === session.selected()) row.setAttribute("aria-current", "true");

    row.appendChild(avatarEl(conv.name, conv.online));

    const text = el("span", "device-text");
    text.appendChild(el("span", "device-name", conv.name));
    const isUnpairedApp = conv.kind === "app" && !pairedApps.has(conv.id) && conv.online;
    let preview;
    if (isUnpairedApp) {
      preview = "Vinculá con la app para conversar";
    } else if (conv.online || conv.messages.length > 0) {
      const last = conv.messages[conv.messages.length - 1];
      preview = last ? `${last.mine ? "Vos: " : ""}${last.text}` : "Sin mensajes todavía";
    } else {
      preview = "Desconectado";
    }
    text.appendChild(el("span", "device-preview", preview));
    row.appendChild(text);

    const side = el("span", "device-side");
    if (conv.unread > 0 && conv.id !== session.selected()) {
      side.appendChild(el("span", "unread-badge", String(conv.unread)));
    }
    const last = conv.messages[conv.messages.length - 1];
    if (last && !isUnpairedApp) side.appendChild(el("span", "device-time", previewTime(last.at)));
    row.appendChild(side);

    row.onclick = () => {
      if (isUnpairedApp) {
        startPairing(conv);
        return;
      }
      openConversation(conv.id);
    };
    box.appendChild(row);
  }
}

// ── Conversación ─────────────────────────────────────────
function openConversation(id) {
  session.select(id);
  renderDevices();
  renderView();
  const area = $("msg");
  area.value = session.getDraft(id);
  growArea();
  requestAnimationFrame(() => area.focus());
}

function renderView() {
  const id = session.selected();
  const shell = $("shell");
  const hasConv = !!id;
  $("welcome").classList.toggle("hidden", hasConv);
  $("conv").classList.toggle("hidden", !hasConv);
  shell.classList.toggle("show-conv", hasConv);

  if (!hasConv) return;
  const conv = session.conversation(id);

  const headAvatar = $("conv-avatar");
  const fresh = avatarEl(conv.name, conv.online, 36);
  headAvatar.replaceWith(fresh);
  fresh.id = "conv-avatar";

  $("conv-name").textContent = conv.name;
  const status = $("conv-status");
  status.textContent = conv.online ? "En línea" : "Desconectado";
  status.classList.toggle("is-off", !conv.online);

  renderThread();
  updateComposer();
}

function updateComposer() {
  const id = session.selected();
  if (!id) return;
  const can = session.canSend(id);
  const area = $("msg");
  area.disabled = !can;
  $("btn-send").disabled = !can;
  $("btn-attach").disabled = !can;
  area.placeholder = !can
    ? conn.isOpen()
      ? "Esta sesión se desconectó: no se puede enviar"
      : "Sin conexión con el hub"
    : "Escribe un mensaje";
}

function renderThread() {
  const id = session.selected();
  const thread = $("thread");
  thread.textContent = "";
  if (!id) return;
  const conv = session.conversation(id);
  const entries = conv.messages;

  if (entries.length === 0) {
    const empty = el("div", "conv-empty");
    empty.appendChild(el("p", "conv-empty-title", `Todavía no hay mensajes con ${conv.name}`));
    empty.appendChild(
      el(
        "p",
        "conv-empty-sub",
        "Escribí abajo y va a salir en vivo hacia la otra sesión. Nada queda guardado.",
      ),
    );
    thread.appendChild(empty);
    return;
  }

  const wrap = el("div", "conv-thread");
  entries.forEach((e, i) => {
    const prev = entries[i - 1];
    const newDay = !prev || dayLabel(prev.at) !== dayLabel(e.at);
    if (newDay) wrap.appendChild(el("div", "day-chip", dayLabel(e.at)));
    const grouped =
      !!prev && !newDay && prev.kind !== "system" && e.kind !== "system" && prev.mine === e.mine && e.at - prev.at < GROUP_WINDOW;
    wrap.appendChild(renderEntry(e, grouped));
  });
  thread.appendChild(wrap);
  thread.scrollTop = thread.scrollHeight;
}

function renderEntry(e, grouped) {
  const row = el(
    "div",
    `bubble-row ${e.kind === "system" ? "system" : e.mine ? "is-mine" : "is-theirs"} ${
      grouped ? "is-grouped" : ""
    }`,
  );
  if (e.kind === "system") return el("div", "system-row", e.text);

  const bubble = el("div", "bubble");
  if (e.kind === "text") {
    bubble.classList.add("is-copyable");
    bubble.title = "Tocar para copiar";
    bubble.appendChild(el("span", "bubble-text", e.text));
  } else {
    if (isPreviewableImage(e.name) && e.url) {
      const img = el("img", "bubble-img");
      img.src = e.url;
      img.alt = e.name;
      bubble.appendChild(img);
    }
    const card = el("a", "file-card");
    card.href = e.url;
    card.download = e.name;
    card.innerHTML =
      '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>';
    card.appendChild(el("span", null, e.name));
    if (e.size != null) card.appendChild(el("span", "file-size", `· ${fmtSize(e.size)}`));
    bubble.appendChild(card);
  }

  const meta = el("span", "bubble-meta", clockTime(e.at));
  bubble.appendChild(meta);

  if (e.kind === "text") {
    bubble.onclick = async () => {
      try {
        await navigator.clipboard.writeText(e.text);
        meta.textContent = "Copiado";
        setTimeout(() => (meta.textContent = clockTime(e.at)), 1200);
      } catch {
        /* portapapeles no disponible (contexto inseguro) */
      }
    };
  }

  row.appendChild(bubble);
  return row;
}

$("btn-back").onclick = () => {
  session.select(null);
  renderDevices();
  renderView();
};

// ── Enviar texto ─────────────────────────────────────────
function sendText() {
  const id = session.selected();
  const text = $("msg").value.trim();
  if (!id || !text || !session.canSend(id)) return;
  try {
    sendTextMessage(session, id, text, { relay: relayTo });
  } catch (err) {
    // Falla de encolado local, no acuse de entrega: el borrador queda intacto.
    reportTextSendFailure(id, err);
    return;
  }
  $("msg").value = "";
  session.setDraft(id, "");
  growArea();
  renderDevices();
  renderView();
}

function reportTextSendFailure(peerId, err) {
  const conv = session.conversations().find((c) => c.id === peerId);
  if (!conv) return;
  if (String(err?.message) === "peer-unavailable") {
    session.system(peerId, `${conv.name} se desconectó: el mensaje no se envió.`);
  } else {
    session.system(
      peerId,
      "No se pudo enviar el mensaje: falló la conexión al encolar. Probá de nuevo.",
    );
  }
  renderDevices();
  if (session.selected() === peerId) renderView();
}

$("btn-send").onclick = sendText;
$("msg").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    sendText();
  }
});
$("msg").addEventListener("input", () => {
  const id = session.selected();
  if (id) session.setDraft(id, $("msg").value);
  growArea();
  const len = $("msg").value.length;
  const count = $("msg-count");
  if (len > 460) {
    count.hidden = false;
    count.textContent = String(500 - len);
  } else {
    count.hidden = true;
  }
});

function growArea() {
  const area = $("msg");
  area.style.height = "0px";
  area.style.height = `${Math.min(area.scrollHeight, 120)}px`;
}

// ── Archivos ─────────────────────────────────────────────
// El destinatario se captura ANTES de la preparación async del archivo:
// cambiar de conversación mientras prepara no redirige el envío.
async function sendFileTo(peerId, file) {
  if (!peerId) return;
  if (file.size > MAX_FILE) {
    session.system(peerId, "El archivo supera el máximo de 25 MB y no se envió.");
    if (session.selected() === peerId) renderThread();
    return;
  }
  try {
    await sendFile(
      session,
      peerId,
      {
        name: file.name,
        size: file.size,
        prepare: () => file.arrayBuffer().then((b) => new Uint8Array(b)),
      },
      { relay: relayFile },
    );
    renderDevices();
    if (session.selected() === peerId) renderView();
  } catch (err) {
    // Estado de encolado local, no acuse de entrega.
    const conv = session.conversations().find((c) => c.id === peerId);
    if (!conv) return;
    if (String(err?.message) === "peer-unavailable") {
      session.system(peerId, `${conv.name} se desconectó: el archivo no se envió.`);
    } else {
      session.system(
        peerId,
        `No se pudo enviar ${file.name}: falló la conexión al encolar. Probá de nuevo.`,
      );
    }
    renderDevices();
    if (session.selected() === peerId) renderView();
  }
}

$("btn-attach").onclick = () => {
  if (!session.canSend(session.selected())) return;
  $("file-input").click();
};

$("file-input").onchange = () => {
  const f = $("file-input").files[0];
  const peerId = session.selected(); // capturado antes del await
  if (f && peerId) sendFileTo(peerId, f);
  $("file-input").value = "";
};

let dragDepth = 0;
window.addEventListener("dragenter", () => {
  dragDepth++;
  document.body.classList.add("is-dragging");
});
window.addEventListener("dragleave", () => {
  dragDepth--;
  if (dragDepth <= 0) {
    dragDepth = 0;
    document.body.classList.remove("is-dragging");
  }
});
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  document.body.classList.remove("is-dragging");
  const peerId = session.selected(); // capturado antes del await
  if (!peerId || !session.canSend(peerId)) return;
  for (const f of e.dataTransfer.files) sendFileTo(peerId, f);
});

// ── Vinculación con apps ─────────────────────────────────
function startPairing(conv) {
  // Un intento nuevo reemplaza al anterior: su timeout queda sin efecto.
  pairingAttempts.cancel();
  pairingTargetId = conv.id;
  relayTo(conv.id, { type: "pair-request" });
  $("pair-title").textContent = `Vincular con ${conv.name}`;
  $("pair-hint").textContent = `En ${conv.name} va a aparecer un código de vinculación. Escribilo acá:`;
  $("pair-code").value = "";
  $("pairing-err").textContent = "";
  $("pairing").classList.remove("hidden");
  $("pair-code").focus();
}

function confirmPairing() {
  const code = $("pair-code").value.trim();
  const target = pairingTargetId;
  if (code.length < 4 || !target) return;
  if (pairingAttempts.pending() === target) return; // ya esperando respuesta
  relayTo(target, { type: "pair-verify", code });
  pairingAttempts.start(target);
}

$("btn-pair").onclick = confirmPairing;
$("pair-code").addEventListener("keydown", (e) => {
  if (e.key === "Enter") confirmPairing();
});
$("btn-pair-cancel").onclick = () => {
  pairingAttempts.cancel();
  pairingTargetId = null;
  $("pairing").classList.add("hidden");
};

// ── Limpieza y ciclo de vida ─────────────────────────────
// pagehide final: revocar object URLs. Con BFCache (persisted) la página puede
// volver, así que los links de descarga siguen vivos; al restaurar, si el
// socket murió congelado, se muestra el estado de recuperación. Nunca se
// reenvían mensajes automáticamente.
window.addEventListener("pagehide", (e) => handlePageHide(e, session));
window.addEventListener("pageshow", (e) =>
  handlePageShow(e, { session, connection: conn, onDisconnected: markDisconnected }),
);
