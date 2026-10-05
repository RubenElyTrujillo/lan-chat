# Desktop ↔ Web Integration via Hub — Design

Desktop becomes a first-class peer of the existing hub at `wss://lan-chat.nerdalab.tech/hub`: it connects and reconnects automatically, exchanges presence with browsers, pairs with a correlated request-id code flow, and relays chat and files (≤ 25 MiB) in both directions. The desktop persists **all** conversation history (including web-origin) in SQLite until the user deletes it; the browser stays fully ephemeral.

**Status:** approved scope, revised after the 2026-10-04 audit (browser identity lifetime, receiver-side dedup, delete-ordering, pairing concurrency, module decomposition), and re-revised the same date after the persistence audit, which eliminated six conflicts: (1) the frontend snapshot `save_history` model (DELETE-ALL + reinsert) is retired — history writes become transactional per-message appends plus guarded state patches, because a snapshot replay wipes natively-arrived inbound rows and can regress delivery/read states; (2) deletes gate on a **global `hist_epoch`** (bumped by `delete_all_history`) plus per-key `hist_rev` (bumped by `delete_conversation`), closing the empty-key resurrection hole that a per-key-only gate leaves after a whole-history delete; (3) inbound acceptance persists **before** any UI event and **never** `INSERT OR REPLACE`s rows — same id+content is a duplicate with no second side effect, same id with different content or under a different conversation is a conflict reject; (4) deletion is **DB-only**: SQLite rows and revision metadata only, sent/received files and folders are never removed; (5) durable contact keys are **desktop-native grant UUIDs** (`hub:<native-uuid>` minted per successful pairing) — the browser-presented sid is untrusted and never a durable key; (6) migration is idempotent behind a `history_legacy_imported` flag, and the first history slice ships as an **inert DB-only module** (`mod history;`, no commands, no UI) with the frontend+backend cutover happening atomically later — no mixed-API release. Approved scope is unchanged: desktop WSS/public-hub presence, session-correlated auth, text/files 25 MiB, SQLite desktop (including web-origin), web ephemeral, LAN untouched, **no app-app hub fallback**.

## Quick path

1. Review the decision table below — every seam that could lie about functionality is called out honestly.
2. Review the identity, acceptance, and pairing rules (security-relevant).
3. Read `docs/superpowers/plans/2026-10-04-desktop-web-integration-plan.md` for the slice breakdown; only **Slices 1a–1c (foundation)** are implemented after this design is approved.

## Current state (honest baseline)

| Fact today | Consequence this design must address |
|---|---|
| No desktop hub client exists (`src-tauri/src/lib.rs`, 1130 lines, only does mDNS + TCP 8787 + local axum 8789) | New focused `src-tauri/src/hub/` modules; `lib.rs` only gains wiring — no second 1000-line god module |
| Desktop UI fakes `sent` for peers without an IP (`src/App.tsx` `send()`) | Hub-path sends get honest states; legacy `web:*` fake path is explicitly left untouched (out of scope) |
| Hub peer ids are per-connection random (`crypto.randomBytes(6)` in `web/server.js`) | Conversations must key on a browser-presented **session id**, never on hub conn id or display name |
| Web pairing today: `pair-request` from an app is auto-accepted web↔web (`web/public/app.js:206`); legacy bridge pairing reuses the LAN PIN | New hub pairing is code-based and independent; LAN PIN is never read or written by hub code |
| `save_history` does `DELETE FROM messages` + full reinsert from the frontend snapshot (`lib.rs:315`) | A snapshot replay wipes rows that arrived natively between snapshot and save, can regress delivery/read states to older values, and resurrects deleted conversations → the snapshot model is retired, not hardened: transactional per-message append + guarded state patch + DB-only gated deletes (see History model) |
| Browser is ephemeral by product decision | No persistent browser identity, no delivery/read receipts over the hub; desktop-side states cap at `sent` |

## Decisions

| Topic | Decision |
|---|---|
| Transport | `tokio-tungstenite = "0.30"` with feature `rustls-tls-webpki-roots` — TLS is always validated against webpki roots; there is no code path that disables validation. `futures-util` (already a dependency, `Cargo.toml:30`) drives the split sink/stream |
| Hub URL | Default `wss://lan-chat.nerdalab.tech/hub`. `LANCHAT_HUB_URL` env var overrides for local tests (e.g. `ws://localhost:8788/hub npm start` in `web/`). Plain `ws://` is reachable only via explicit env override |
| Desktop identity | **Stable `device_id`** (UUIDv4-format hex from `getrandom`) persisted once in the SQLite `settings` table, separate from the **live hub session id** the hub assigns per connection (`welcome.id`). Both appear on the wire; only `device_id` survives restarts |
| Browser identity | **Per-page in-memory `sid`** (`crypto.randomUUID()` at page scope — no `sessionStorage`, no `localStorage`). Page reload and tab duplication mint a **new identity**; only a socket reconnect within the same page keeps it. The browser-presented `sid` is **untrusted input**: it may be displayed, but it is never persisted as a durable key. Desktop keys hub-origin conversations **`hub:<native-uuid>`** — a UUIDv4 the desktop mints itself when a pairing grant is created (one per successful pairing; a re-pair mints a new key, the browser appears as a new contact, and the old conversation stays in history, offline, with its name metadata). Names are persisted in history; two browsers with the same name never merge |
| Authorization lifetime | Pairing grants are bound to **(current hub conn id, `reqId`, capability `hub-v1`)**, held only in memory. When a peer disappears from the `peers` list (tab closed **or** page reconnected with a new conn id) its grant is dropped — the same sid returning on a new conn id must pair again. Chat/file relays from an unauthorized or sid-less peer are dropped. There is no grant that survives a conn change |
| Presence | Hub IP-group `peers` lists (existing) drive mutual presence. Desktop mirrors them to the UI via a `hub-presence` event. Inherited constraint: desktop and browser must share a public IP to see each other — this is the deployed hub's grouping model |
| Pairing | Browser-initiated only (the minimum for the approved scope): browser sends `pair-request {reqId}`; desktop answers with an 8-digit CSPRNG code (`getrandom::fill`), TTL 120 s, max 3 verification attempts per code. **Bounded concurrency:** up to **2 pending codes**, each bound to `(peer conn id, reqId)` — two browsers can pair at once; a third request gets `pair-error {reason:"busy"}`. Rate limit is **per peer** (≤ 10 code generations / rolling 10 min). Codes **never persist** and hub pairing **never reads or writes** the LAN `own_pin` |
| Desktop initiation | There is no desktop-initiated pairing. If the desktop user clicks an unpaired `hub:` contact and sends, the send fails with `pair-required` and the UI shows guidance to pair from the browser — **never** a fake success, auto-auth, or a desktop-sent `pair-request` |
| Files | Base64 relay both ways, hard cap 25 MiB per file (≈ 34 MB base64 frame, under the hub's 40 MB `maxPayload`). Filenames sanitized to their final path component; on-disk collisions get ` (n)` suffixes instead of overwrites. Inbound acceptance is receiver-verified (dedup + staging), not `INSERT OR REPLACE`-and-hope — see "Inbound acceptance" |
| History | One transactional write path — **append per message** at send/receive time and **guarded state patches** (delivery/read columns only) — there is no bulk snapshot save. Hub-origin inbound rows persist **before** any UI event via `accept_text` (`INSERT` only, never `REPLACE`): same id + same content → duplicate, no second side effect; same id with different content or under a different conversation → conflict reject. Deletes are **DB-only**: rows plus revision metadata (`hist_epoch` global, `hist_rev:<key>` per key); sent/received files are never removed from disk. Retries patch state, they never re-append. Permissions/pairing are **never** written to history |
| Delivery states | Hub send resolves `sent` only when the frame is written to the socket; never `delivered`/`read`. LAN acks (`delivered`/`sent`/`read-ack`) are untouched |
| Reconnect | Exponential backoff with full jitter, 1 s → 30 s cap, reset on successful welcome. Every desktop reconnect starts a fresh hub session: prior pairing and presence are invalidated. **No replay**: failed hub sends stay `failed` until the user explicitly retries |
| Untouched | mDNS + TCP 8787 LAN protocol, the legacy 8789 axum bridge (and its `send_web`/`send_web_file` commands), LAN PIN flows, `web/trust-proxy.js` |

## Wire protocol

Everything rides the hub's existing `relay` envelope; only `hello`/`peers` gain optional fields (backward compatible). `kind` stays `"app"`/`"web"` exactly as today — these changes are additive for old clients.

### Hello / presence

```jsonc
// Desktop → hub (on connect)
{ "type": "hello", "name": "<device_name>", "kind": "app", "sid": "<device_id>", "caps": ["hub-v1"] }
// Browser → hub (compatible change)
{ "type": "hello", "name": "<name>", "kind": "web", "sid": "<per-page sid>", "caps": ["web-v1"] }

// Hub → all (peers entries gain optional fields)
{ "type": "peers", "list": [{ "id": "<conn id>", "name": "...", "kind": "web", "sid": "...", "caps": ["web-v1"] }] }

// Hub → relay envelope gains one field
{ "type": "relay", "from_id": "...", "from_name": "...", "from_sid": "...", "payload": { ... } }
```

A peer without `sid` is **legacy/incompatible** for the desktop: presence may render, but chat/file from it is rejected (drop + log).

### Pairing (browser initiates, existing UX preserved)

```jsonc
// 1. Browser → desktop (relay payload)
{ "type": "pair-request", "reqId": "<uuid>" }
// 2. Desktop: CSPRNG 8-digit code, TTL 120 s, bound to (peer conn id, reqId) → UI overlay shows code
// 3. Browser → desktop (user typed the code shown on the desktop)
{ "type": "pair-verify", "code": "12345678", "reqId": "<same uuid>" }
// 4a. Desktop → browser on success
{ "type": "pair-ok", "reqId": "<same uuid>" }
// 4b. Desktop → browser on wrong code / exhausted attempts / busy / rate-limited
{ "type": "pair-error", "reqId": "<same uuid>", "reason": "code" | "busy" | "rate" }
```

Rules desktop-side: a repeat `pair-request` from the **same peer** with the **same `reqId`** while its code is live **reuses** the code (no regeneration spam); a new `reqId` from that peer replaces its pending entry; requests beyond the 2-slot cap get `busy`; ≥ 10 generations in 10 min mutes **that peer** until the window clears. A `pair-ok` is sent **only** to the requesting peer and only for a matching, live `reqId` — stale/mismatched verifies are ignored. The browser's `pairing-attempts.js` keys attempts by `reqId` so a late `pair-ok` cannot complete a superseded attempt.

### Chat / files

```jsonc
// Desktop → browser
{ "type": "relay", "to": "<conn id>", "payload": { "type": "chat", "text": "...", "kind": "app", "id": "<uuid>" } }
{ "type": "relay", "to": "<conn id>", "payload": { "type": "file", "name": "a.pdf", "data": "<base64>", "size": 123, "kind": "app", "id": "<uuid>" } }

// Browser → desktop (same payloads with "kind": "web"); desktop requires sid + authorization
```

## Inbound acceptance (receiver-verified, not blind upsert)

`INSERT OR REPLACE` alone cannot dedup: it cannot prevent duplicate UI events, duplicate file writes, or a wrong payload winning an id collision. Every inbound chat/file passes this pipeline before any user-visible effect:

1. **Authorize** — `from_id` is live in `peers`, `from_sid` present, grant exists for `(from_id)` with capability `hub-v1`. Otherwise drop + log.
2. **Scope uniqueness** — durable key is `(device_key = "hub:<native-uuid>", id)` where the device key is the desktop-minted grant UUID; enforced by a `UNIQUE(device_key, id)` index (additive; the global `id` PK stays for compatibility, which means the same `id` resurfacing under a **different** conversation key can never insert and is treated as a conflict, not a silent remap).
3. **Conflict check** — if a row already exists for that id: under a different key → **conflict**: reject + log. Under the same key: compare `content_hash` (SHA-256 of the payload bytes; legacy rows without a hash fall back to `text` equality). Same content → **duplicate**: skip every side effect (no second UI event, no file rewrite) and treat as success. Different content → **conflict**: reject + log, no write. Inbound rows are **never** `INSERT OR REPLACE`d.
4. **Atomic accept + persist before any UI event** — a new message is one SQLite transaction that inserts the row; **the row itself is the acceptance record**. There is no tombstone or seen-marker written before the persist (a crash between marker and persist would lose the message forever), and the UI event is emitted only after the transaction commits.
5. **File staging** — decoded bytes are written to `<download_dir>/.hub-stage/<id>` *before* the insert; the row's `file_path` initially points at the staging file, and after commit the file is renamed to its collision-safe final path and the row updated. Failure at any step cleans up the staging file; a crash leaves either an unreferenced staging file (swept at startup: stage files with no referencing row are deleted) or a row that still points at a valid staged file. Disk-write failure yields no file row; a `hub-file-error` event lets the UI append a persisted system notice.

The hub is a dumb relay and the browser has no retry queue, so duplicates are rare (desktop retry paths only) — the pipeline exists to make them harmless, not to optimize a hot path. Row-level accept logic (`accept_text`) lives in the DB-only `history` module so it is testable without a hub session; `hub/inbox.rs` contributes authorization and hashing around it. The **history-deletion path never touches the filesystem**: the only `fs::remove_file` in the whole design is the `.hub-stage` crash sweep, which removes solely *unreferenced* staging leftovers (a Slice 5a staging concern) and is never invoked by `delete_conversation`/`delete_all_history`.

## History model: transactional append, guarded patches, DB-only deletes

The legacy frontend snapshot save (`DELETE FROM messages` + full reinsert, `lib.rs:315`) is retired, not hardened. A snapshot replay destroys rows that arrived natively between snapshot and save, can regress `sent`/`read` states to older values, and — after a delete — resurrects conversations, including keys that no longer have rows. The replacement model is three primitives plus a two-level gate, all inside one DB-only module (`src-tauri/src/history.rs`, inert until the atomic cutover):

- **Per-message append.** Every desktop-generated message is inserted in its own transaction at send time (`append_message`). Retries patch the existing row's state; they never re-append, so a retried send cannot duplicate a row.
- **Guarded state patch.** Delivery/read progress goes through `patch_message_state(key, id, state, read, token)`: an `UPDATE` that touches only the `state` and `read` columns — never content, never a reinsert. A stale or out-of-order patch (caller's token behind the current one) is rejected with `stale-history`; a patch for a nonexistent row returns an honest `Ok(false)`, never fakes success.
- **Two-level staleness gate.** `settings` holds a global `hist_epoch` (bumped by `delete_all_history`) and per-key `hist_rev:<key>` (bumped by `delete_conversation(key)`; absent = 0). `load_history` returns entries **plus** the current token set (`epoch` + `revs`); every append/patch echoes its token and is rejected with `stale-history` on mismatch. The global epoch is what closes the empty-key hole: after `delete_all`, a per-key gate alone cannot protect a key whose rows *and* rev metadata are gone — the epoch mismatch still rejects the stale caller. Rev metadata is never removed, so stale callers are rejected even for keys that no longer exist in `messages`.
- **Inbound is never gated.** `accept_text` inserts with the current epoch/rev context unconditionally — a genuinely new message is not a stale snapshot, so new hub traffic after any delete is accepted; and because snapshots no longer exist, an old snapshot can never recreate anything.
- **Honest versioning.** Delete commands return the fresh token/epoch they produced; a rejected caller gets `stale-history` and must reload that conversation from the DB (documented, tested behavior) — the version is never faked or guessed.
- **Deletion is DB-only.** `delete_conversation` removes one key's rows and bumps `hist_rev:<key>` in one transaction; `delete_all_history` removes all rows and bumps `hist_epoch` in one transaction. Neither removes, renames, or rewrites sent/received files or folders — UI history disappears, downloaded bytes stay on disk (verified by a real-tempfile byte-identical test).
- **Idempotent, guarded migration.** `ensure_history_schema` runs once behind the `history_legacy_imported` settings flag: `ALTER TABLE messages ADD COLUMN content_hash TEXT` (a "duplicate column" error means already-migrated) + the `UNIQUE(device_key, id)` index. Legacy snapshot rows stay valid with `content_hash` NULL and text-equality dedup fallback; the flag guarantees the one-time legacy handling can never run twice (a re-run after deletes would mislabel rows).
- **Cutover is atomic.** This module ships first with `mod history;` registered and **no commands exposed and no frontend change**. In a later slice the commands are exposed and `history.ts`/`App.tsx` switch to them, retiring the legacy `save_history` command **in the same change** — no release ever runs the old snapshot API and the new append API together. The LAN wire protocol is untouched by this; only its frontend persistence route changes at cutover.

## Delivery semantics

| Path | Success ceiling | Failure | Auto-retry |
|---|---|---|---|
| LAN TCP (existing) | `delivered` + `read` via socket acks / `read-ack` | `failed` (`PIN_REQUERIDO` → prompt) | Existing outbox probe, LAN only |
| Hub (new) | `sent` (frame written to socket) | `failed` with honest cause (`hub-unavailable`, `peer-offline`, `pair-required`, `file-too-large`) | **Never** — explicit user retry only |

The browser has no ack channel (ephemeral), so showing `delivered`/`read` for hub traffic would be a lie; the desktop never emits those states for `hub:*` keys.

## Desktop module layout (no god module)

`lib.rs` is already 1130 lines; the hub client must not become a second one. New code lives in focused files, each with one responsibility and a ≤ ~400-line budget:

| File | Responsibility |
|---|---|
| `src-tauri/src/history.rs` | **DB-only history layer (inert first).** Guarded idempotent migration (`history_legacy_imported`), token minting (`hist_epoch`/`hist_rev:<key>`), `append_message`, guarded `patch_message_state`, `accept_text` (dedup/conflict, never `REPLACE`, persist-before-emit ordering), gated `delete_conversation`/`delete_all_history` (never touch files), `load_history` (entries + tokens). Registered as `mod history;` with **no commands and no UI** — cutover is atomic, later |
| `src-tauri/src/hub/mod.rs` | Session loop, welcome handshake, frame dispatch, `HubShared`/`HubRuntime`/`HubStatus`, Tauri wiring surface |
| `src-tauri/src/hub/identity.rs` | `resolve_hub_url`, UUIDv4 minting, persisted `device_id` |
| `src-tauri/src/hub/backoff.rs` | Full-jitter reconnect backoff (pure, injectable rng) |
| `src-tauri/src/hub/protocol.rs` | Inbound frame types/parsing, outbound payload builders (chat/file/pair frames) |
| `src-tauri/src/hub/pairing.rs` | Pending-code state machine (cap 2, TTL, attempts, per-peer rate ring), grants; on acceptance mints the durable contact key `hub:<native-uuid>`, recorded in the runtime's grant-scoped `contact_keys` map (cleared together with grants on disconnect) |
| `src-tauri/src/hub/inbox.rs` | Acceptance pipeline shell: authorization lookups, hashing, file staging, sweeps — row writes delegate to `history::accept_text`/file twin |

`lib.rs` gains only: `mod hub;`, `mod history;` (inert in the first history slice), `AppState.hub`, spawn, new commands, and — at the atomic cutover slice — the history commands replacing `save_history`.

## Out of scope

P2P and large-file transfer, native fallback, app-app fallback via the hub, removing the legacy 8789 bridge or `send_web*` commands, LAN PIN CSPRNG hardening, hub-side auth/persistence, web↔desktop read receipts, any replay queue, desktop-initiated hub pairing, making the browser sid survive reloads (deliberately *not* wanted), exposing history commands or switching the frontend before the atomic cutover (the DB-only slice stays inert), and any deletion path that removes sent/received files or folders.

## Acceptance checklist

- [ ] Desktop auto-connects to the configured hub URL; TLS validation is structural (no bypass exists)
- [ ] Kill the hub: desktop backs off with jitter, reconnects when it returns; pairing does not survive the reconnect
- [ ] Browser reload mints a new `sid`; the desktop shows a new contact and the old conversation stays offline — no `sessionStorage`/`localStorage` key carries the sid
- [ ] Two browsers pair concurrently (2 pending codes); a third gets `busy`; wrong code 3× disables that code; desktop LAN PIN is byte-identical before/after
- [ ] Browser reconnect (same page, new conn id) keeps its conversation but must pair again before sending
- [ ] Text and ≤ 25 MiB files flow both ways; a > 25 MiB file is refused honestly on both sides
- [ ] Same inbound id twice with the same content: no duplicate UI event and no second file on disk; same id with different content, or the same id under a different conversation key, is rejected and logged — inbound rows are never `INSERT OR REPLACE`d
- [ ] Desktop restart: grant-keyed conversations and names persist, marked offline
- [ ] Desktop quit mid-receive does not lose an already-emitted inbound message (persist-before-emit); no orphan `.hub-stage` file survives a restart sweep unreferenced
- [ ] Deleting a conversation or all history removes only SQLite rows/metadata: sent/received files and folders are byte-identical afterwards (real-tempfile test); a stale append/patch token afterwards is rejected with `stale-history` — including for keys with no remaining rows (epoch gate) — and a new inbound message still persists under the current epoch
- [ ] History writes are transactional appends and guarded state patches: the legacy snapshot `save_history` no longer exists after the atomic cutover (no mixed-API release); stale/out-of-order patches are rejected and a patch never rewrites content
- [ ] The DB-only history slice ships inert (no commands, no UI); commands + frontend switch to the new API in one atomic cutover
- [ ] Clicking an unpaired hub contact and sending yields honest guidance to pair from the browser — no fake success, no auto-auth
- [ ] `cargo test` (src-tauri) and `npm test` (web) pass; existing 49 web tests stay green
