# LAN-Chat Web — salas efímeras

Versión web de LAN-Chat: salas por código, chat en vivo y archivos hasta 25 MB.
**Nada se persiste** — salas, mensajes y archivos viven en RAM y expiran
(archivos 30 min, salas vacías 6 h).

## Local

```bash
cd web
npm install
npm start          # http://localhost:8788
```

## VPS con Docker

```bash
docker build -t lan-chat-web .
docker run -d --name lan-chat-web --restart unless-stopped -p 8788:8788 lan-chat-web
```

Tras un reverse proxy con HTTPS mejor (habilita copiar al portapapeles);
directo por `http://IP:8788` funciona todo excepto copiar.

## Notas

- El código de sala ES la credencial: quien lo tiene, entra.
- Máximo 12 personas por sala, 25 MB por archivo.
- Sin cuentas, sin historial, sin base de datos.
