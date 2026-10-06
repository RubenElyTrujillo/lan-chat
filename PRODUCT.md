# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

(Tauri 2 desktop app: web UI in a native window. A future companion **web app** is planned as the second half of the product; this project is the desktop app.)

## Users

Anyone who wants to move text — and eventually files — between **their own devices** on the same local network. The immediate build target is the desktop app; a web app will let other devices join from a browser later.

## Product Purpose

LAN-Chat lets devices on the same LAN discover each other and exchange content as a chat, with zero internet, zero accounts, zero cloud. Success: two devices find each other instantly and the content arrives — private by construction.

## Positioning

Two apps, one protocol: a desktop app and a web app that speak the same protocol, so the receiving side needs no install (web) or no browser (desktop). By default traffic stays on the LAN; an optional hub relay is opt-in via configuration. Neighboring products (AirDrop, SendAnywhere, cloud clipboard syncs) cannot truthfully claim "no accounts, works cross-platform via browser."

## Operating Context

- Devices share a LAN/wifi; no internet connectivity required at all.
- Each desktop instance announces itself over mDNS (`_lanchat._tcp.local.`) and listens on TCP port 8787.
- Optional hub mode: setting `LANCHAT_HUB_URL` connects the desktop app to a hub relay (private deployments only; the public default is LAN-only, hub disabled).
- macOS dev environment today; bundle targets "all" desktop platforms.
- `DEVICE_NAME` env var overrides the announced name (used to test multiple instances on one machine).

## Capabilities and Constraints

**Shipped (v0.1.0 prototype):**
- mDNS discovery of other LAN-Chat instances (hides self, filters to `_lanchat` service).
- TCP text transfer: newline-delimited JSON `{from, text}` on port 8787; receive is event-driven into the UI.

**Confirmed direction (user):**
- Chat history is persisted per conversation; the user decides when to delete — a single chat's history or all chats.
- Interaction model reference: messaging apps — the user explicitly cited WhatsApp as the mental model (chat list + conversation). This describes product shape, not a visual spec; visual treatment is decided in design work, not here.
- Files are part of the eventual scope ("ya sea un archivo, texto o lo que sea").
- A web app companion is planned; the desktop app is the current build.
- UI copy is Spanish (user decision, shaping phase).

**Technical constraints:**
- Stack: Tauri 2 + React 19 + TypeScript + Vite frontend, Rust backend. Not a greenfield stack decision — existing codebase.
- Port 8787 and the `_lanchat._tcp.local.` service type are the wire contract between apps.

## Brand Commitments

- **Privacy / local-first** is a binding product promise: by default content never leaves the LAN — no accounts, no cloud, no telemetry. The optional hub is strictly opt-in via `LANCHAT_HUB_URL`; the app is fully functional without it.
- Name: LAN-Chat (identifier `com.rubenely.lanchat`). No logo or visual identity assets exist yet (icons are Tauri template defaults).
- **Craft bar (user, standing):** WhatsApp / Telegram Desktop — the UI must sit alongside mass-market chat apps at their finish level. Pinned visual direction: modern soft messenger — light surfaces, pastel bubbles, large radii, black pill primary actions, clean grotesque.

## Evidence on Hand

- Working prototype in this repo (mDNS discovery + TCP text transfer, 2 commits).
- No logo, brand assets, testimonials, screenshots, or user research exist. Future work must not fabricate any of these.

## Product Principles

1. **Local-first, always.** The default experience is LAN-only: no accounts, no cloud, no telemetry. The hub is an opt-in relay (operator-trusted), never a requirement.
2. **Discovery should feel like magic.** Finding the other device must be automatic and instant — the user never configures IPs or ports.
3. **The user owns the history.** Chat history persists until the user deletes it, one chat or all of them, and deletion is real.
4. **Two doors, one protocol.** Desktop and web apps are equal citizens of the same wire contract; never break one to favor the other.
5. **Content moves, chat is the vehicle.** The job is transferring content between devices; the chat experience serves that job.
