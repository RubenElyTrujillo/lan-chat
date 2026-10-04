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

### Variable de entorno `TRUST_PROXY`

Por defecto el hub agrupa a los pares por la IP del socket TCP, y **ignora por
completo** la cabecera `X-Forwarded-For` (si alguien la enviara, podría
suplantar otra IP). Si el hub corre detrás de un proxy de borde confiable
(por ejemplo Caddy en el host VPS, que por defecto sobrescribe la cabecera con
la IP real del cliente), el tráfico llega al contenedor con la IP del puente
Docker (`172.17.0.1`) y todos los clientes caerían en un mismo grupo.

Para ese caso, lista **exacta** de IPs de proxy confiables (separadas por
coma):

```bash
docker run -d --name lan-chat-web --restart unless-stopped \
  -p 8788:8788 -e TRUST_PROXY=172.17.0.1 lan-chat-web
```

Reglas (implementadas en `trust-proxy.js`, cubiertas por tests):

- Off por defecto: sin `TRUST_PROXY` no se lee `X-Forwarded-For` jamás.
- Solo coincidencia exacta de IP normalizada (se acepta forma `::ffff:`).
  Valores no-IP (`true`, `*`, CIDR) se descartan: nunca hay "confiar en todo".
- Solo si el socket proviene de una IP de la lista se toma la IP de
  `X-Forwarded-For`, y únicamente la entrada más a la derecha (la que agregó
  el proxy de borde al final).
- Cabecera ausente, vacía o malformada → se usa la IP del socket; nunca
  rompe ni permite suplantación.

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
