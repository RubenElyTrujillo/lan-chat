// LAN-Chat Web — cliente del hub: presencia por IP pública + relay de datos.
const $ = (id) => document.getElementById(id);

// randomUUID solo existe en contextos seguros (HTTPS); fallback para HTTP.
const uid = () =>
  crypto.randomUUID
    ? crypto.randomUUID()
    : Array.from(crypto.getRandomValues(new Uint32Array(4))).join("-");

const state = {
  ws: null,
  myId: null,
  name: null,
  peers: [], // [{id, name, kind}]
  target: null, // id del par elegido (null = nadie)
  pairedApps: new Set(), // ids de apps emparejadas (por sesión)
};

const PAIR_PENDING = {}; // appId -> resolve cuando llega el verify

const proto = location.protocol === "https:" ? "wss" : "ws";

// ── Landing ──────────────────────────────────────────────
$("btn-enter").onclick = () => {
  const name = $("name").value.trim();
  if (!name) return showError("Poné tu nombre");
  state.name = name;
  connect();
};

function showError(msg) {
  $("landing-error").textContent = msg;
}

function connect() {
  const ws = new WebSocket(`${proto}://${location.host}/hub`);
  state.ws = ws;

  ws.onopen = () =>
    ws.send(JSON.stringify({ type: "hello", name: state.name, kind: "web" }));

  ws.onmessage = (e) => handle(JSON.parse(e.data));

  ws.onclose = () => {
    if (state.myId) {
      document.body.innerHTML =
        '<p style="font-family:sans-serif;padding:30px">Conexión perdida. Recargá la página.</p>';
    }
  };
}

// ── Mensajes del hub ─────────────────────────────────────
function handle(m) {
  if (m.type === "welcome") {
    state.myId = m.id;
    $("landing").classList.add("hidden");
    $("peers-screen").classList.remove("hidden");
    return;
  }
  if (m.type === "peers") {
    state.peers = m.list;
    renderPeers();
    return;
  }
  if (m.type === "relay") {
    const p = m.payload;
    switch (p.type) {
      case "pair-request": {
        // Una app quiere vincularse conmigo: no pido código (grupo confiable),
        // respondo OK para que quede emparejada en esta sesión.
        state.pairedApps.add(m.from_id);
        state.ws.send(
          JSON.stringify({
            type: "relay",
            to: m.from_id,
            payload: { type: "pair-ok" },
          }),
        );
        addSystem(`🖥️ ${m.from_name} (app) se vinculó con vos`);
        renderPeers();
        break;
      }
      case "pair-ok": {
        const pend = PAIR_PENDING[m.from_id];
        if (pend) {
          state.pairedApps.add(m.from_id);
          pend();
          delete PAIR_PENDING[m.from_id];
        }
        break;
      }
      case "chat":
        if (p.kind === "app" && !state.pairedApps.has(m.from_id)) return;
        addChat(m.from_name, p.text);
        break;
      case "file": {
        if (p.kind === "app" && !state.pairedApps.has(m.from_id)) return;
        const bytes = Uint8Array.from(atob(p.data), (c) => c.charCodeAt(0));
        addFile(m.from_name, p.name, bytes);
        break;
      }
    }
  }
}

// ── Pantalla de pares ────────────────────────────────────
function renderPeers() {
  const box = $("peer-list");
  const others = state.peers.filter((p) => p.id !== state.myId);
  $("peer-count").textContent = `${others.length} dispositivo${s(others.length)} en tu red`;
  box.innerHTML = "";
  for (const p of others) {
    const isApp = p.kind === "app";
    const paired = state.pairedApps.has(p.id);
    const el = document.createElement("button");
    el.className = "device";
    el.innerHTML = `<span class="dot ${paired || !isApp ? "" : "dim"}"></span>
      <span>${isApp ? "🖥️" : "🌐"} ${esc(p.name)}</span>
      ${isApp ? '<span class="tag">app</span>' : '<span class="tag web">web</span>'}
      <span class="arrow">${paired || !isApp ? "Abrir chat →" : "Vincular →"}</span>`;
    el.onclick = () => selectPeer(p);
    box.appendChild(el);
  }
  if (others.length === 0) {
    box.innerHTML =
      '<div class="hint">Nadie más está conectado desde tu red todavía. Abrí LAN-Chat en otro dispositivo de tu casa y va a aparecer acá.</div>';
  }
}

const s = (n) => (n === 1 ? "" : "s");

function selectPeer(p) {
  const isApp = p.kind === "app";
  const paired = state.pairedApps.has(p.id);

  if (isApp && !paired) {
    // Pedir vinculación: la app muestra su código en pantalla.
    state.target = p.id;
    state.targetName = p.name;
    send(p, { type: "pair-request" });
    $("peers-screen").classList.add("hidden");
    $("pairing").classList.remove("hidden");
    $("pair-code").value = "";
    $("pairing-err").textContent = "";
    return;
  }
  openChatWith(p);
}

function confirmPairing() {
  const code = $("pair-code").value.trim();
  if (code.length < 4) return;
  state.ws.send(
    JSON.stringify({
      type: "relay",
      to: state.target,
      payload: { type: "pair-verify", code },
    }),
  );
  PAIR_PENDING[state.target] = () => {
    $("pairing").classList.add("hidden");
    openChatWith({ id: state.target, name: state.targetName, kind: "app" });
  };
  setTimeout(() => {
    if (PAIR_PENDING[state.target]) {
      $("pairing-err").textContent = "No respondió. ¿Es el código correcto?";
      delete PAIR_PENDING[state.target];
    }
  }, 6000);
}
$("btn-pair").onclick = confirmPairing;
$("pair-code").addEventListener("keydown", (e) => {
  if (e.key === "Enter") confirmPairing();
});
$("btn-pair-cancel").onclick = () => {
  $("pairing").classList.add("hidden");
  $("peers-screen").classList.remove("hidden");
};

// ── Chat ─────────────────────────────────────────────────
let chatPeer = null; // {id, name}

function openChatWith(peer) {
  chatPeer = peer;
  state.target = peer.id;
  $("peers-screen").classList.add("hidden");
  $("chat").classList.remove("hidden");
  $("chat-title").textContent = peer.name;
  $("chat-sub").textContent = peer.kind === "app" ? "app de escritorio" : "navegador";
}

$("btn-back").onclick = () => {
  chatPeer = null;
  state.target = null;
  $("chat").classList.add("hidden");
  $("peers-screen").classList.remove("hidden");
};

function relay(peerId, payload) {
  state.ws.send(JSON.stringify({ type: "relay", to: peerId, payload }));
}

function sendChat() {
  const v = $("msg").value.trim();
  if (!v || !chatPeer) return;
  relay(chatPeer.id, {
    type: "chat",
    text: v,
    kind: "web",
    id: uid(),
  });
  addChat(chatPeer.name, v);
  $("msg").value = "";
}
$("btn-send").onclick = sendChat;
$("msg").addEventListener("keydown", (e) => {
  if (e.key === "Enter") sendChat();
});

// ── Render ───────────────────────────────────────────────
function esc(x) {
  const d = document.createElement("div");
  d.textContent = x;
  return d.innerHTML;
}

function addChat(from, text) {
  const row = document.createElement("div");
  row.className = `bubble-row ${from === state.name ? "is-mine" : ""}`;
  const meta = new Date().toLocaleTimeString("es", { hour: "2-digit", minute: "2-digit" });
  const b = document.createElement("div");
  b.className = "bubble";
  b.innerHTML = `<span class="bubble-from">${esc(from)}</span><span>${esc(text)}</span><span class="bubble-meta">${meta}</span>`;
  b.onclick = async () => {
    try {
      await navigator.clipboard.writeText(text);
      b.querySelector(".bubble-meta").textContent = "copiado ✓";
      setTimeout(() => (b.querySelector(".bubble-meta").textContent = meta), 1200);
    } catch {}
  };
  row.appendChild(b);
  $("thread").appendChild(row);
  $("thread").scrollTop = $("thread").scrollHeight;
}

function addSystem(text) {
  const row = document.createElement("div");
  row.className = "bubble-row system";
  row.textContent = text;
  $("thread").appendChild(row);
  $("thread").scrollTop = $("thread").scrollHeight;
}

function addFile(from, name, bytes, id) {
  const row = document.createElement("div");
  row.className = `bubble-row ${from === state.name ? "is-mine" : ""}`;
  const blob = new Blob([bytes]);
  const url = URL.createObjectURL(blob);
  const kb = bytes.length > 1048576 ? (bytes.length / 1048576).toFixed(1) + " MB" : Math.ceil(bytes.length / 1024) + " KB";
  const b = document.createElement("div");
  b.className = "bubble";
  b.innerHTML = `<span class="bubble-from">${esc(from)}</span>
    <a class="file-card" href="${url}" download="${esc(name)}">📎 ${esc(name)} · ${kb} — tocá para descargar</a>`;
  row.appendChild(b);
  $("thread").appendChild(row);
  $("thread").scrollTop = $("thread").scrollHeight;
}

// ── Archivos ─────────────────────────────────────────────
$("btn-attach").onclick = () => $("file-input").click();
$("file-input").onchange = () => {
  const f = $("file-input").files[0];
  if (f) uploadFile(f);
  $("file-input").value = "";
};

async function uploadFile(file) {
  if (file.size > 25 * 1024 * 1024) return addSystem("⚠️ Máximo 25 MB");
  if (!chatPeer) return addSystem("⚠️ Elegí un destinatario");
  addSystem(`⏳ Enviando ${file.name}…`);
  const buf = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let i = 0; i < buf.length; i += 0x8000) {
    binary += String.fromCharCode(...buf.subarray(i, i + 0x8000));
  }
  const data = btoa(binary);
  relay(chatPeer.id, {
    type: "file",
    name: file.name,
    data,
    kind: "web",
    id: uid(),
  });
  addFile(chatPeer.name, file.name, buf, chatPeer.id);
}

// Drag & drop
let depth = 0;
window.addEventListener("dragenter", () => {
  depth++;
  document.body.classList.add("is-dragging");
});
window.addEventListener("dragleave", () => {
  depth--;
  if (depth <= 0) {
    depth = 0;
    document.body.classList.remove("is-dragging");
  }
});
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  depth = 0;
  document.body.classList.remove("is-dragging");
  if (!chatPeer) return;
  for (const f of e.dataTransfer.files) uploadFile(f);
});
