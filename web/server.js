// LAN-Chat Hub — punto de encuentro por IP pública. Sin salas, sin storage:
// presenta a los pares que comparten IP pública y les hace de relay.
import { WebSocketServer } from "ws";
import http from "http";
import crypto from "crypto";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import { parseTrustedProxies, resolveClientIp } from "./trust-proxy.js";
import { parsePresence } from "./presence.js";

const PORT = process.env.PORT || 8788;
const MAX_MSG = 40 * 1024 * 1024; // 40 MB (archivos base64 por relay)
// Allowlist exacta de IPs de proxy (separadas por coma). Vacía/ausente = off:
// se ignora X-Forwarded-For y se agrupa por la IP del socket.
const trustedProxies = parseTrustedProxies(process.env.TRUST_PROXY);

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PUB = path.join(__dirname, "public");
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".svg": "image/svg+xml",
};

const peers = new Map(); // ws -> { id, name, kind, ip, sid?, caps? }
const groups = new Map(); // ip -> Set<ws>

const publicIp = (req) =>
  resolveClientIp({
    socketIp: req.socket.remoteAddress,
    forwardedFor: req.headers["x-forwarded-for"],
    trustedProxies,
  });

const send = (res, status, body, type = "application/json") => {
  res.writeHead(status, { "Content-Type": type, "Cache-Control": "no-store" });
  res.end(body);
};

const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://x");
  if (url.pathname === "/api/health") {
    return send(res, 200, JSON.stringify({ ok: true, peers: peers.size }));
  }
  let file = url.pathname === "/" ? "/index.html" : url.pathname;
  file = path.normalize(file).replace(/^(\.\.[/\\])+/, "");
  const full = path.join(PUB, file);
  if (!full.startsWith(PUB)) return send(res, 403, "forbidden", "text/plain");
  fs.readFile(full, (err, data) => {
    if (err) return send(res, 404, "no encontrado", "text/plain");
    send(res, 200, data, MIME[path.extname(full)] || "application/octet-stream");
  });
});

const wss = new WebSocketServer({ server, maxPayload: MAX_MSG });

function broadcastPeers(ip) {
  const group = groups.get(ip);
  if (!group) return;
  // Presence metadata (sid/caps) rides along only when the peer registered a
  // valid one; legacy peers are broadcast exactly as before.
  const list = [...group].map((p) => ({
    id: p.id,
    name: p.name,
    kind: p.kind,
    ...(p.sid ? { sid: p.sid } : {}),
    ...(p.caps ? { caps: p.caps } : {}),
  }));
  const data = JSON.stringify({ type: "peers", list });
  for (const p of group) if (p.readyState === 1) p.send(data);
}

wss.on("connection", (ws, req) => {
  ws.ip = publicIp(req);
  ws.id = crypto.randomBytes(6).toString("hex");
  ws.name = null;
  ws.kind = "web";

  ws.on("message", (raw) => {
    let m;
    try {
      m = JSON.parse(raw);
    } catch {
      return;
    }

    if (m.type === "hello") {
      ws.name = String(m.name || "anónimo").slice(0, 24);
      ws.kind = m.kind === "app" ? "app" : "web";
      // Valid presence metadata only; anything malformed is simply absent.
      Object.assign(ws, parsePresence(m));
      let group = groups.get(ws.ip);
      if (!group) {
        group = new Set();
        groups.set(ws.ip, group);
      }
      group.add(ws);
      ws.send(
        JSON.stringify({
          type: "welcome",
          id: ws.id,
          ip: ws.ip,
          peersInGroup: group.size,
        }),
      );
      return broadcastPeers(ws.ip);
    }

    // Relay: entregar el payload al par destino del mismo grupo (misma IP pública).
    // from_sid se toma de la conexión REGISTRADA que envía: el payload jamás
    // puede atribuirse una identidad que no anunció en su hello.
    if (m.type === "relay" && m.to) {
      const group = groups.get(ws.ip) || [];
      for (const peer of group) {
        if (peer.id === m.to) {
          return peer.send(
            JSON.stringify({
              type: "relay",
              from_id: ws.id,
              from_name: ws.name,
              ...(ws.sid ? { from_sid: ws.sid } : {}),
              payload: m.payload,
            }),
          );
        }
      }
    }
  });

  ws.on("close", () => {
    const group = groups.get(ws.ip);
    if (!group) return;
    group.delete(ws);
    if (group.size === 0) groups.delete(ws.ip);
    else broadcastPeers(ws.ip);
  });
});

server.listen(PORT, () => {
  console.log(`LAN-Chat Hub escuchando en http://0.0.0.0:${PORT}`);
});

// Exported for integration tests (ephemeral PORT=0) to discover the bound port.
export { server };
