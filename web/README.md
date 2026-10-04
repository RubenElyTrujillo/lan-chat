# LAN-Chat Web — cliente efímero del hub

Versión web de LAN-Chat: presencia por IP pública + relay de texto y archivos.
**Nada se persiste**: conversaciones, archivos, borradores y estado de lectura
viven solo en la memoria de la página. Al recargar, se empieza de cero.

## Local

```bash
cd web
npm install
npm start          # http://localhost:8788
npm test           # tests del estado del cliente (node --test)
```

## VPS con Docker

```bash
docker build -t lan-chat-web .
docker run -d --name lan-chat-web --restart unless-stopped -p 8788:8788 lan-chat-web
```

Tras un reverse proxy con HTTPS mejor (habilita copiar al portapapeles);
directo por `http://IP:8788` funciona todo excepto copiar.

## Cómo funciona

- `server.js` — hub WebSocket: agrupa clientes por IP pública y retransmite
  (`relay`) los mensajes entre pares del mismo grupo. Sirve `public/` como
  sitio estático.
- `public/client-state.js` — estado efímero del cliente, sin DOM: conversaciones
  por ID de par (nunca por nombre), borradores, no leídos, ciclo de vida de
  object URLs y envío de archivos con destinatario capturado antes de la
  preparación async. Cubierto por tests.
- `public/app.js` + `index.html` + `style.css` — UI portada del diseño de la
  app de escritorio (`DESIGN.md`): superficies claras, burbujas lavanda
  (entrante) y crema (saliente), botón pill negro, Inter variable bundeada en
  `public/fonts/` (sin requests a Google Fonts).

## Notas

- Los mensajes pasan por el hub (relay); la web no es P2P ni promete misma red.
- Las apps de escritorio deben vincularse por código (`pair-request` /
  `pair-verify` / `pair-ok`); las sesiones web del mismo grupo son de confianza.
- Máximo 25 MiB por archivo saliente.
- Sin ACKs: no hay indicadores de entrega ni lectura.
