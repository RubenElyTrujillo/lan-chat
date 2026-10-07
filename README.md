# LAN-Chat

LAN-Chat is a local-network messenger: devices on the same network discover each other automatically and exchange text and files as a chat — no accounts, no cloud, no internet required. A shared-clipboard mode syncs your clipboard across your devices with a global shortcut, and every conversation keeps a persistent local history you control.

<!-- TODO: add a real screenshot of the chat list + conversation view before launch. Do not fabricate. -->

## Features

| Feature | What it does |
| --- | --- |
| Chat | Text messages between devices on the same LAN, with read receipts |
| Files | Send files of any type; received files land in your download folder |
| Persistent history | Every conversation keeps its history in a local SQLite database; delete one chat or all chats — deleting history never deletes received files |
| Shared clipboard | Copy on one device, paste on another, via a global shortcut |
| PIN pairing | Pair devices once with a PIN; web sessions pair with 8-digit codes |
| Optional hub relay | Connect to a self-hosted hub (`LANCHAT_HUB_URL`) to bridge browsers and remote networks — the app is fully functional without it |
| Read receipts | See when your message reached the other device |

## Requirements

- Rust toolchain (`rustup`)
- Node.js 20+ and npm
- macOS, Windows, or Linux (desktop)

## Quick start (development)

```bash
npm install
npm run tauri dev
```

That opens the desktop app. Launch a second instance (another machine on the same network, or `DEVICE_NAME=Other npm run tauri dev` on the same machine) and they will find each other automatically.

## Building installers

```bash
npm run tauri build
```

Installers must be built **on each target OS** — cross-compilation is not supported:

| OS | Artifacts |
| --- | --- |
| macOS | `.dmg`, `.app` |
| Windows | `.msi`, `.exe` (NSIS) |
| Linux | `.AppImage`, `.deb` |

Output lands in `src-tauri/target/release/bundle/`. The binaries are unsigned, so the first run may trigger an OS warning (Gatekeeper, SmartScreen, etc.) — this is normal for unsigned local builds.

How to open the app on each OS:

- **macOS**: after installing, macOS (Sequoia in particular) may report the app as "damaged". The app is fine — Gatekeeper just quarantines unsigned downloads. Clear the flag and open normally:

  ```bash
  xattr -cr /Applications/lan-chat.app
  ```

- **Windows**: SmartScreen shows "Windows protected your PC" — click **More info** → **Run anyway**.
- **Linux**: there are no signature checks; if the AppImage doesn't start, make it executable with `chmod +x`.

## Optional: Hub mode

By default LAN-Chat is a pure LAN application — no hub is configured and nothing leaves your network. To bridge browsers and other networks, point the app at a hub relay with the `LANCHAT_HUB_URL` environment variable:

```bash
LANCHAT_HUB_URL=wss://your-hub.example.com/hub npm run tauri dev
```

The hub server itself is not part of this repository; you deploy your own on infrastructure you control.

**Trust note:** traffic relayed through a hub is readable by the hub operator. Only connect to hubs you run yourself or explicitly trust.

## Security model

- **PIN pairing per device**: devices pair once via a PIN exchange; unpaired peers cannot message you.
- **Pairing codes for web sessions**: browser sessions join through short-lived 8-digit codes.
- **No accounts, no telemetry**: there is nothing to sign up for and nothing phones home.
- **Local history**: conversations are stored in a local SQLite database on your machine. Deleting history never deletes received files.

## Guía rápida en español

LAN-Chat es un mensajero de red local: chat, archivos, historial por conversación y portapapeles compartido entre tus dispositivos, sin cuentas ni internet.

```bash
npm install
npm run tauri dev
```

Para compilar el instalador: `npm run tauri build` (`.dmg` en macOS, `.msi`/`.exe` en Windows, `.AppImage`/`.deb` en Linux — hay que compilar en cada sistema operativo; los binarios sin firmar muestran un aviso del sistema la primera vez, es normal).

El hub es opcional: sin configurar nada, la app funciona solo en tu red local. Para conectar un hub propio: `LANCHAT_HUB_URL=wss://tu-hub.ejemplo.com/hub npm run tauri dev`. El tráfico que pasa por un hub puede ser leído por quien lo opera — usá hubs de confianza o auto-alojados.

## Contributing

Issues and pull requests are welcome. Keep changes focused; run the test suites before submitting:

```bash
cargo test --locked   # from src-tauri/
node --test tests/*.test.ts
npm run build
```

## License

[MIT](LICENSE) — Copyright (c) 2026 Ruben Ely Trujillo
