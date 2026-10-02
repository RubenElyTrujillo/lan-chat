// LAN-Chat Web — servidor de salas efímeras. Todo vive en RAM, nada se persiste.
import { WebSocketServer } from "ws";
import http from "http";
import crypto from "crypto";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

const PORT = process.env.PORT || 8788;
const MAX_FILE = 25 * 1024 * 1024; // 25 MB
const FILE_TTL = 30 * 60 * 1000; // 30 min
const ROOM_TTL = 6 * 60 * 60 * 1000; // 6 h vacía

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PUB = path.join(__dirname, "public");
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".svg": "image/svg+xml",
};

const rooms = new Map(); // code -> { members: Map<ws, name>, at }
const files = new Map(); // id -> { room, name, from, buf, at }

const code6 = () =>
  Array.from(
    { length: 6 },
    () => "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"[crypto.randomInt(32)],
  ).join("");

const send = (res, status, body, type = "application/json") => {
  res.writeHead(status, {
    "Content-Type": type,
    "Cache-Control": "no-store",
  });
  res.end(body);
};

const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://x");

  if (req.method === "GET" && url.pathname === "/api/health") {
    return send(res, 200, JSON.stringify({ ok: true, rooms: rooms.size }));
  }

  // Subida de archivo: cuerpo crudo, query ?room=&name=&from=
  if (req.method === "POST" && url.pathname === "/api/upload") {
    const room = url.searchParams.get("room") || "";
    const name = (url.searchParams.get("name") || "archivo").slice(0, 120);
    const from = (url.searchParams.get("from") || "anónimo").slice(0, 24);
    if (!rooms.has(room)) return send(res, 404, '{"error":"sala inexistente"}');

    const chunks = [];
    let size = 0;
    let aborted = false;
    req.on("data", (c) => {
      size += c.length;
      if (size > MAX_FILE) {
        aborted = true;
        return send(res, 413, '{"error":"archivo demasiado grande"}');
      }
      chunks.push(c);
    });
    req.on("end", () => {
      if (aborted) return;
      const id = crypto.randomBytes(8).toString("hex");
      files.set(id, {
        room,
        name,
        from,
        buf: Buffer.concat(chunks),
        at: Date.now(),
      });
      const payload = JSON.stringify({
        type: "file",
        from,
        name,
        url: `/f/${id}`,
        size,
        at: Date.now(),
      });
      broadcast(room, payload);
      send(res, 200, JSON.stringify({ ok: true, url: `/f/${id}` }));
    });
    return;
  }

  // Descarga de archivo compartido.
  if (req.method === "GET" && url.pathname.startsWith("/f/")) {
    const f = files.get(url.pathname.slice(3));
    if (!f) return send(res, 404, "archivo expirado", "text/plain");
    res.writeHead(200, {
      "Content-Type": "application/octet-stream",
      "Content-Disposition": `attachment; filename="${f.name.replace(/"/g, "")}"`,
      "Content-Length": f.buf.length,
    });
    return res.end(f.buf);
  }

  // Estáticos.
  let file = url.pathname === "/" ? "/index.html" : url.pathname;
  file = path.normalize(file).replace(/^(\.\.[/\\])+/, "");
  const full = path.join(PUB, file);
  if (!full.startsWith(PUB)) return send(res, 403, "forbidden", "text/plain");
  fs.readFile(full, (err, data) => {
    if (err) return send(res, 404, "no encontrado", "text/plain");
    send(res, 200, data, MIME[path.extname(full)] || "application/octet-stream");
  });
});

const wss = new WebSocketServer({ server });

wss.on("connection", (ws) => {
  ws.room = null;
  ws.code = null;
  ws.name = null;

  ws.on("message", (raw) => {
    let m;
    try {
      m = JSON.parse(raw);
    } catch {
      return;
    }

    if (m.type === "join") {
      let code = String(m.code || "").toUpperCase().trim();
      if (m.create) {
        do {
          code = code6();
        } while (rooms.has(code));
        rooms.set(code, { members: new Map(), at: Date.now() });
      }
      const room = rooms.get(code);
      if (!room)
        return ws.send(JSON.stringify({ type: "error", message: "La sala no existe" }));
      if (room.members.size >= 12)
        return ws.send(JSON.stringify({ type: "error", message: "La sala está llena" }));

      ws.room = room;
      ws.code = code;
      ws.name = String(m.name || "anónimo").slice(0, 24);
      room.members.set(ws, ws.name);
      ws.send(JSON.stringify({ type: "joined", code, name: ws.name }));
      broadcast(code, {
        type: "system",
        text: `${ws.name} se unió`,
        at: Date.now(),
      });
      return sendMembers(code);
    }

    if (!ws.room) return;

    if (m.type === "chat") {
      const text = String(m.text || "").trim().slice(0, 2000);
      if (!text) return;
      return broadcast(ws.code, {
        type: "chat",
        from: ws.name,
        text,
        at: Date.now(),
      });
    }
  });

  ws.on("close", () => {
    if (!ws.room) return;
    ws.room.members.delete(ws);
    broadcast(ws.code, { type: "system", text: `${ws.name} salió`, at: Date.now() });
    sendMembers(ws.code);
    if (ws.room.members.size === 0) ws.room.at = Date.now();
  });
});

function broadcast(code, payload) {
  const room = rooms.get(code);
  if (!room) return;
  const data = JSON.stringify(payload);
  for (const [client] of room.members) {
    if (client.readyState === 1) client.send(data);
  }
}

function sendMembers(code) {
  const room = rooms.get(code);
  if (!room) return;
  broadcast(code, { type: "members", list: [...room.members.values()] });
}

// Barrendero: salas vacías viejas y archivos expirados.
setInterval(() => {
  const now = Date.now();
  for (const [code, room] of rooms) {
    if (room.members.size === 0 && now - room.at > ROOM_TTL) rooms.delete(code);
  }
  for (const [id, f] of files) {
    if (now - f.at > FILE_TTL) files.delete(id);
  }
}, 60_000);

server.listen(PORT, () => {
  console.log(`LAN-Chat Web escuchando en http://0.0.0.0:${PORT}`);
});
