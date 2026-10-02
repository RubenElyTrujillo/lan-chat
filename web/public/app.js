// LAN-Chat Web — cliente de sala efímera.
const $ = (id) => document.getElementById(id);

const state = {
  ws: null,
  code: null,
  name: null,
};

// ── Landing ──────────────────────────────────────────────
$("btn-create").onclick = () => join({ create: true });
$("btn-join").onclick = () => join({ create: false });
$("code-join").addEventListener("keydown", (e) => {
  if (e.key === "Enter") join({ create: false });
});
$("code-join").addEventListener("input", (e) => {
  e.target.value = e.target.value.toUpperCase().replace(/[^A-Z0-9]/g, "");
});

function join({ create }) {
  const name = ($("name-create").value || $("name-join").value || "").trim();
  const code = $("code-join").value.trim();
  if (!name) return showError("Poné tu nombre");
  if (!create && code.length !== 6) return showError("El código tiene 6 caracteres");

  const proto = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${proto}://${location.host}`);
  state.ws = ws;
  state.name = name;

  ws.onopen = () => ws.send(JSON.stringify({ type: "join", create, code, name }));
  ws.onmessage = (e) => handle(JSON.parse(e.data));
  ws.onclose = () => {
    if (state.code) {
      addSystem("Te desconectaste. Recargá para volver a entrar.");
    }
  };
}

function showError(msg) {
  $("landing-error").textContent = msg;
}

// ── Mensajes del servidor ────────────────────────────────
function handle(m) {
  switch (m.type) {
    case "joined":
      state.code = m.code;
      $("landing").classList.add("hidden");
      $("chat").classList.remove("hidden");
      $("room-code").textContent = m.code;
      addSystem(`Sala ${m.code} lista. Compartí el código para invitar.`);
      break;
    case "error":
      if (!state.code) showError(m.message);
      else addSystem(`⚠️ ${m.message}`);
      break;
    case "members":
      $("members").innerHTML = m.list
        .map((n) => `<li>${esc(n)}</li>`)
        .join("");
      break;
    case "system":
      addSystem(m.text);
      break;
    case "chat":
      addChat(m.from, m.text, m.at);
      break;
    case "file":
      addFile(m.from, m.name, m.url, m.size, m.at);
      break;
  }
}

// ── Render ───────────────────────────────────────────────
function esc(s) {
  const d = document.createElement("div");
  d.textContent = s;
  return d.innerHTML;
}

function addChat(from, text, at) {
  const row = document.createElement("div");
  row.className = `bubble-row ${from === state.name ? "is-mine" : ""}`;
  const b = document.createElement("button");
  b.className = "bubble";
  b.title = "Tocar para copiar";
  const meta = new Date(at || Date.now()).toLocaleTimeString("es", {
    hour: "2-digit",
    minute: "2-digit",
  });
  b.innerHTML = `<span class="bubble-from">${esc(from)}</span><span>${esc(text)}</span><span class="bubble-meta">${meta}</span>`;
  b.onclick = async () => {
    try {
      await navigator.clipboard.writeText(text);
      b.querySelector(".bubble-meta").textContent = "copiado ✓";
      setTimeout(() => {
        b.querySelector(".bubble-meta").textContent = meta;
      }, 1200);
    } catch {
      /* sin https el portapapeles puede no estar disponible */
    }
  };
  row.appendChild(b);
  $("thread").appendChild(row);
  scroll();
}

function addSystem(text) {
  const row = document.createElement("div");
  row.className = "bubble-row system";
  row.textContent = text;
  $("thread").appendChild(row);
  scroll();
}

function addFile(from, name, url, size, at) {
  const row = document.createElement("div");
  row.className = `bubble-row ${from === state.name ? "is-mine" : ""}`;
  const meta = new Date(at || Date.now()).toLocaleTimeString("es", {
    hour: "2-digit",
    minute: "2-digit",
  });
  const kb = size > 1024 * 1024 ? `${(size / 1048576).toFixed(1)} MB` : `${Math.ceil(size / 1024)} KB`;
  const b = document.createElement("div");
  b.className = "bubble";
  b.innerHTML = `<span class="bubble-from">${esc(from)}</span>
    <a class="file-card" href="${esc(url)}" download="${esc(name)}">📎 <span>${esc(name)}</span><span style="opacity:.55;font-size:11px">${kb}</span></a>
    <span class="bubble-meta">${meta}</span>`;
  row.appendChild(b);
  $("thread").appendChild(row);
  scroll();
}

function scroll() {
  const t = $("thread");
  t.scrollTop = t.scrollHeight;
}

// ── Enviar ───────────────────────────────────────────────
function sendChat() {
  const input = $("msg-input");
  const text = input.value.trim();
  if (!text || !state.ws) return;
  state.ws.send(JSON.stringify({ type: "chat", text }));
  input.value = "";
}
$("btn-send").onclick = sendChat;
$("msg-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") sendChat();
});

$("btn-leave").onclick = () => location.reload();
$("room-code").onclick = async () => {
  try {
    await navigator.clipboard.writeText(state.code);
    $("room-code").textContent = "¡copiado!";
    setTimeout(() => ($("room-code").textContent = state.code), 1200);
  } catch {
    /* sin https puede fallar */
  }
};

// ── Archivos ─────────────────────────────────────────────
$("btn-attach").onclick = () => $("file-input").click();
$("file-input").onchange = () => {
  const f = $("file-input").files[0];
  if (f) upload(f);
  $("file-input").value = "";
};

async function upload(file) {
  if (file.size > 25 * 1024 * 1024) {
    addSystem("⚠️ Máximo 25 MB por archivo");
    return;
  }
  addSystem(`⏳ Subiendo ${file.name}…`);
  try {
    const q = new URLSearchParams({
      room: state.code,
      name: file.name,
      from: state.name,
    });
    const res = await fetch(`/api/upload?${q}`, {
      method: "POST",
      body: file,
    });
    if (!res.ok) throw new Error("fallo");
  } catch {
    addSystem("⚠️ No se pudo subir el archivo");
  }
}

// Drag & drop
let dragDepth = 0;
window.addEventListener("dragenter", () => {
  dragDepth++;
  document.querySelector(".app").classList.add("is-dragging");
});
window.addEventListener("dragleave", () => {
  dragDepth--;
  if (dragDepth <= 0) {
    dragDepth = 0;
    document.querySelector(".app").classList.remove("is-dragging");
  }
});
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  document.querySelector(".app").classList.remove("is-dragging");
  if (!state.code) return;
  for (const f of e.dataTransfer.files) upload(f);
});
