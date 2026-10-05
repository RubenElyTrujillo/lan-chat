# Desktop ↔ Web Hub Integration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Desktop joins the existing hub (`wss://lan-chat.nerdalab.tech/hub`) as a first-class peer with auto-reconnect, mutual presence with browsers, correlated code pairing, bidirectional chat/files ≤ 25 MiB, and full SQLite persistence of all desktop-side history while the browser stays ephemeral (per-page identity).

**Architecture:** New focused modules under `src-tauri/src/hub/` (`mod.rs` session loop, `identity.rs`, `backoff.rs`, `protocol.rs`, `pairing.rs`, `inbox.rs`) plus a **DB-only `src-tauri/src/history.rs`** that ships inert first (no commands, no UI). History persistence is transactional per-message append + guarded state patches (delivery/read only) gated by a global `hist_epoch` + per-key `hist_rev` — the legacy snapshot `save_history` is retired at an **atomic frontend+backend cutover (Slice 4b)**; no mixed-API release. Inbound acceptance persists before any UI event and never `REPLACE`s rows (duplicate/conflict semantics); durable contact keys are desktop-minted grant UUIDs (`hub:<native-uuid>`, browser sid untrusted); deletion is DB-only and never removes sent/received files. Web-side changes are minimal and additive: per-page `sid`/`caps` in `hello`, `from_sid` in relay envelopes, `reqId` passthrough in pairing.

**Tech Stack:** Rust (tauri 2, tokio, axum, rusqlite) + new `tokio-tungstenite 0.30` (`rustls-tls-webpki-roots`), `getrandom 0.4`, `sha2 0.10`; Node hub (`ws`); vanilla JS web client (`node --test`); React/TS desktop frontend (`npm run build` = `tsc && vite build`).

**Spec:** `docs/superpowers/specs/2026-10-04-desktop-web-integration-design.md`

---

## Scope discipline (read first)

- **Authorization is code-only.** The executor does **not** run `git commit`, `git push`, does not deploy, does not bump versions, and does not create PRs. Every task ends with "report to orchestrator" with the working tree left dirty for review. See **Release gate** at the bottom — only the orchestrator may authorize a commit/release.
- **Only Slices 1a–1c are implemented after orchestrator review of this plan.** Later slices are specified with binding contracts and test examples; each expands to full TDD steps (failing test → run → implement → run) at its own slice start, after the previous slice is reviewed. **Slice 4a is fully expanded here** because its correctness is the persistence audit's core: it is an inert DB-only slice (no commands, no UI), ≤ 400 impl lines with tests separate, and the later commands+frontend cutover ships atomically in Slice 4b — no mixed-API release.
- LAN mDNS + TCP 8787, the legacy 8789 axum bridge, `send_web*` commands, LAN PIN flows, and `web/trust-proxy.js` are **never modified**. Hub pairing code must not read or write `own_pin`/`session_pin`.
- App-app fallback via hub, P2P, large files, replay queues, desktop-initiated pairing: out of scope.
- **Honesty rule:** the desktop never fakes success on the hub path. Legacy `web:*` fake-success in `App.tsx` `send()` predates this plan and is intentionally untouched.
- UI copy stays Spanish; code, comments, and docs are English.

## File structure (who owns what)

| File | Responsibility after this plan |
|---|---|
| `src-tauri/src/hub/mod.rs` | **New.** Session loop, welcome handshake, frame dispatch, `HubShared`/`HubRuntime`/`HubStatus`, outbound channel with write-ack |
| `src-tauri/src/hub/identity.rs` | **New.** Hub URL resolution, UUIDv4 minting, persisted `device_id` |
| `src-tauri/src/hub/backoff.rs` | **New.** Full-jitter reconnect backoff (pure, injectable rng) |
| `src-tauri/src/hub/protocol.rs` | **New.** Inbound frame parsing (`peers`, `relay`), outbound payload builders (chat/file/pair) |
| `src-tauri/src/hub/pairing.rs` | **New.** Pending-code state machine (cap 2, TTL, 3 attempts, per-peer rate ring), pure + injectable clock |
| `src-tauri/src/hub/inbox.rs` | **New.** Acceptance pipeline shell: authorization, hashing, file staging + sweep; row writes delegate to `history::accept_text` and its file twin |
| `src-tauri/src/history.rs` | **New.** DB-only history layer: guarded idempotent migration (`history_legacy_imported`), tokens (`hist_epoch` + `hist_rev:<key>`), `append_message`, guarded `patch_message_state`, `accept_text` (INSERT-only, duplicate/conflict), gated DB-only deletes, `load_history` with tokens. **Inert in Slice 4a** (no commands, no UI); cutover in 4b |
| `src-tauri/src/lib.rs` | Wiring only: `mod hub;`, `mod history;` (inert in Slice 4a), `AppState.hub`, spawn; at the Slice 4b atomic cutover: history commands (`load_history`/`append_message`/`patch_message_state`/`delete_conversation`/`delete_all_history`) replacing the legacy snapshot `save_history` in the same change |
| `src-tauri/Cargo.toml` | Adds `tokio-tungstenite`, `getrandom`, `sha2` |
| `src/lib/backend.ts` | Thin bindings for all new commands/events |
| `src/lib/history.ts` | Cutover bindings (Slice 4b): `loadHistory` (entries + epoch + revs), `appendMessage`, `patchMessageState`, `deleteConversation`, `deleteAllHistory` — no snapshot save |
| `src/App.tsx` | Transport routing by key prefix (`hub:` → hub), hub chip, presence merge, pairing overlay + `pair-required` guidance, append-at-send/receive + `patchMessageState` for state changes, explicit delete invokes. No new business logic |
| `web/server.js` | Compatible: `hello` stores optional `sid` (sanitized ≤64 chars `[A-Za-z0-9_-]`) + `caps` (≤8 strings); peers entries and relay envelopes carry them. Nothing else |
| `web/public/app.js` | Per-page `sid` (`crypto.randomUUID()` at module scope — **no** `sessionStorage`); `reqId` passthrough in pairing |
| `web/public/pairing-attempts.js` | Attempts keyed by `reqId` (was hub app id) |
| `web/test/*.test.js` | New cases per slice; existing 49 tests must stay green |

## Line-count forecast (honest)

Every slice below is a **real deliverable review unit ≤ 400 lines** (code + tests), not a review label on a bigger blob.

| Slice | Unit | Forecast |
|---|---|---|
| 1a | Foundation — deps, identity, backoff (pure modules, no I/O) | ~220 |
| 1b | Foundation — session loop, wiring, `hub_status` (Rust side) | ~300 |
| 1c | Foundation — desktop status UI (bindings + chip) | ~80 |
| 2 | Presence — server sid/caps, browser per-page sid, desktop presence map + UI | ~270 |
| 3a | Pairing — desktop engine (`pairing.rs` state machine + dispatch) | ~260 |
| 3b | Pairing — browser `reqId` + desktop overlay + `pair-required` guidance | ~180 |
| 4a | History — inert DB-only module (guarded migration, token gate, append, guarded patch, gated deletes, accept_text) | ~250 |
| 4b | Chat — atomic cutover (commands + frontend switch, `save_history` retirement) + authorized inbound + honest outbound | ~330 |
| 5a | Files — inbound staging pipeline, dedup, sweep | ~260 |
| 5b | Files — outbound send + UI routing | ~180 |
| 6 | E2E manual verification + docs | ~80 |
| | **Total** | **~2410** |

Integration churn across slices is real; treat the total as **~2400 ±10%**. If a slice overruns 400 at expansion time, it is split before implementation, not reviewed in name only.

---

## Slice 1a — Foundation: identity & backoff (implement next)

Delivers: dependencies, hub URL resolution, persisted `device_id`, backoff calculator. Pure modules, no I/O beyond SQLite in tests. **No session loop, no presence, no pairing, no messages yet** — this slice must not pretend otherwise.

### Task 1: Dependencies and identity module (tests first)

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/hub/mod.rs` (module root), `src-tauri/src/hub/identity.rs`
- Modify: `src-tauri/src/lib.rs` (module registration only)

- [ ] **Step 1: Add dependencies**

In `src-tauri/Cargo.toml`, under `[dependencies]` (after `futures-util`, line 30), add:

```toml
tokio-tungstenite = { version = "0.30", features = ["rustls-tls-webpki-roots"] }
getrandom = "0.4"
sha2 = "0.10"
```

`sha2` is only used from Slice 4a onward; adding it here avoids a second dependency-edit commit unit later.

- [ ] **Step 2: Register the module**

In `src-tauri/src/lib.rs`, line 1 area, add:

```rust
pub mod hub;
```

Create `src-tauri/src/hub/mod.rs`:

```rust
// Hub client: WSS link to the public relay (presence, pairing, chat, files).
// TLS is always validated: tokio-tungstenite is compiled with
// `rustls-tls-webpki-roots` and no bypass exists in this module.

pub mod backoff;
pub mod identity;
```

Create `src-tauri/src/hub/identity.rs` with **only the tests** (implementation comes in Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hub_url_is_production_wss() {
        assert_eq!(resolve_hub_url(None), "wss://lan-chat.nerdalab.tech/hub");
    }

    #[test]
    fn env_override_wins_and_trims() {
        assert_eq!(
            resolve_hub_url(Some("  ws://localhost:8788/hub ")),
            "ws://localhost:8788/hub"
        );
        assert_eq!(resolve_hub_url(Some("   ")), "wss://lan-chat.nerdalab.tech/hub");
    }

    #[test]
    fn uuid_format_is_v4() {
        let u = uuid_from_random([0u8; 16]);
        assert_eq!(u, "00000000-0000-4000-8000-000000000000");
        let u2 = new_uuid().unwrap();
        assert_eq!(u2.len(), 36);
        assert_eq!(u2.as_bytes()[14], b'4');
    }

    #[test]
    fn device_id_is_created_once_and_stable() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        let a = read_or_create_device_id(&conn).unwrap();
        let b = read_or_create_device_id(&conn).unwrap();
        assert_eq!(a, b, "device_id must not rotate between calls");
        assert_eq!(a.len(), 36);
        let stored: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'device_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, a);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml hub::`
Expected: **FAIL** — `resolve_hub_url`, `uuid_from_random`, `new_uuid`, `read_or_create_device_id` not defined. (First run also compiles the new crates; expect a long build.)

- [ ] **Step 4: Implement `identity.rs`** (above the tests module)

```rust
use rusqlite::Connection;

/// Resolves the hub URL. `LANCHAT_HUB_URL` overrides the production default
/// (local test servers use plain ws:// through this exact override).
pub fn resolve_hub_url(env_value: Option<&str>) -> String {
    match env_value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => v.to_string(),
        None => "wss://lan-chat.nerdalab.tech/hub".to_string(),
    }
}

/// Formats 16 random bytes as canonical UUIDv4 text (no external uuid crate).
pub fn uuid_from_random(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// Cryptographically secure UUIDv4.
pub fn new_uuid() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| format!("entropy unavailable: {e}"))?;
    Ok(uuid_from_random(b))
}

/// Stable desktop identity for the hub. Persisted once in `settings`;
/// never rotated. Separate from the per-connection hub session id.
pub fn read_or_create_device_id(conn: &Connection) -> Result<String, String> {
    let existing: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'device_id'", [], |r| {
            r.get(0)
        })
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other.to_string()),
        })?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = new_uuid()?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('device_id', ?1)",
        rusqlite::params![id],
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}
```

(The `settings` table DDL matches `open_history_db` at `lib.rs:358`; in tests it is created inline.)

- [ ] **Step 5: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml hub::`
Expected: 4 PASS.

- [ ] **Step 6: Report to orchestrator** (no commit — see Release gate)

### Task 2: Reconnect backoff with full jitter (tests first)

**Files:**
- Create: `src-tauri/src/hub/backoff.rs`
- Modify: `src-tauri/src/hub/mod.rs` (add `pub mod backoff;` — already present from Task 1)

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/hub/backoff.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn exponential_sequence_is_exact_and_capped() {
        assert_eq!(ReconnectBackoff::exponential_ms(0), 1_000);
        assert_eq!(ReconnectBackoff::exponential_ms(1), 2_000);
        assert_eq!(ReconnectBackoff::exponential_ms(2), 4_000);
        assert_eq!(ReconnectBackoff::exponential_ms(5), 30_000); // capped
        assert_eq!(ReconnectBackoff::exponential_ms(50), 30_000);
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let mut bo = ReconnectBackoff::new();
        let d = bo.next_delay_with(&mut |_| 0u64);
        assert_eq!(d, Duration::from_millis(1)); // clamped to the 1 ms floor
        let mut bo = ReconnectBackoff::new();
        let d = bo.next_delay_with(&mut |_| u64::MAX);
        assert!(d >= Duration::from_millis(1));
        assert!(d <= Duration::from_millis(1_000));
    }

    #[test]
    fn attempts_advance_up_to_cap_then_reset() {
        let mut bo = ReconnectBackoff::new();
        let mut seq: u64 = 0;
        let mut rng = move || {
            seq += 1;
            seq
        };
        for attempt in 0..8u32 {
            let d = bo.next_delay_with(&mut rng);
            let upper = ReconnectBackoff::exponential_ms(attempt);
            assert!(d >= Duration::from_millis(1));
            assert!(d <= Duration::from_millis(upper));
        }
        // Beyond the cap the upper bound stays 30 s.
        let d = bo.next_delay_with(&mut rng);
        assert!(d <= std::time::Duration::from_secs(30));
        bo.reset();
        let d = bo.next_delay_with(&mut rng);
        assert!(d <= std::time::Duration::from_millis(1_000));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml backoff`
Expected: FAIL — `ReconnectBackoff` not defined.

- [ ] **Step 3: Implement** (above the tests module)

```rust
use std::time::Duration;

/// Exponential backoff with full jitter: 1s, 2s, 4s ... capped at 30s.
/// `reset()` on a successful session. The rng closure is injectable so
/// tests are deterministic.
pub struct ReconnectBackoff {
    attempt: u32,
}

const BASE_MS: u64 = 1_000;
const CAP_MS: u64 = 30_000;

impl ReconnectBackoff {
    pub fn new() -> Self {
        Self { attempt: 0 }
    }

    /// Pure exponential component: 1000, 2000, 4000 ... capped at 30_000 ms.
    pub fn exponential_ms(attempt: u32) -> u64 {
        BASE_MS.saturating_mul(1u64 << attempt.min(31)).min(CAP_MS)
    }

    /// Full jitter: uniform in [1, exponential_ms(attempt)] ms.
    pub fn next_delay_with(&mut self, rng: &mut impl FnMut() -> u64) -> Duration {
        let upper = Self::exponential_ms(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        let roll = rng() % (upper + 1);
        Duration::from_millis(roll.max(1).min(upper))
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml hub::`
Expected: 7 PASS (4 identity + 3 backoff).

- [ ] **Step 5: Report to orchestrator**

**Slice 1a exit criteria:** `cargo test` green (7 hub tests: 4 identity + 3 backoff); `cargo check` clean; no runtime code outside `hub/identity.rs` and `hub/backoff.rs`.

---

## Slice 1b — Foundation: session loop & wiring (Rust)

Delivers: WSS connect/hello/welcome loop with TLS validation, reconnect with backoff, `HubShared`/`HubRuntime`, `hub_status` command, app wiring.

### Task 3: Session runtime and connect loop (tests first)

**Files:**
- Modify: `src-tauri/src/hub/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to `src-tauri/src/hub/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A new hub session invalidates prior pairing and presence.
    #[test]
    fn disconnect_clears_peers_and_grants() {
        let mut rt = HubRuntime {
            connected: true,
            ..Default::default()
        };
        rt.peers.insert(
            "c1".into(),
            HubPeer { id: "c1".into(), sid: Some("s1".into()), name: "Ana".into() },
        );
        rt.authorized.insert("c1".into(), PairGrant { req_id: "r1".into() });
        apply_disconnect(&mut rt, "socket closed");
        assert!(!rt.connected);
        assert!(rt.peers.is_empty());
        assert!(rt.authorized.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml hub::`
Expected: FAIL — `HubRuntime`, `HubPeer`, `PairGrant`, `apply_disconnect` not defined.

- [ ] **Step 3: Implement the runtime, channel, and loop** (above the tests module)

```rust
use crate::hub::backoff::ReconnectBackoff;
use serde::Serialize;
use tokio::sync::mpsc;

/// Volatile hub session state. Cleared on every disconnect: a new hub
/// session invalidates prior pairing and presence. Never persisted.
#[derive(Default)]
pub struct HubRuntime {
    pub connected: bool,
    /// hub conn id -> peer (browser) info, from `peers` lists
    pub peers: std::collections::HashMap<String, HubPeer>,
    /// hub conn id -> pairing grant for the CURRENT connection only.
    /// A grant implies the `hub-v1` capability (chat + files).
    pub authorized: std::collections::HashMap<String, PairGrant>,
}

#[derive(Clone, Serialize)]
pub struct HubPeer {
    pub id: String,
    pub sid: Option<String>,
    pub name: String,
}

/// Proof of a completed pairing for one live connection.
pub struct PairGrant {
    pub req_id: String,
}

/// Frontend-facing status (Tauri command payload).
#[derive(Serialize)]
pub struct HubStatus {
    pub url: String,
    pub connected: bool,
    pub peers: usize,
}

/// One outbound frame plus its optional write-ack. `sent` states are only
/// reported after the sink write succeeds (honest success ceiling).
pub struct Outbound {
    pub text: String,
    pub ack: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}

/// Shared handle for commands. The current session's sender lives behind a
/// mutex and is swapped by `hub_loop` on every connect (a fresh session owns
/// a fresh channel). Sends before the first connect fail with a dead channel.
pub struct HubShared {
    pub url: String,
    pub runtime: std::sync::Mutex<HubRuntime>,
    pub outbound: std::sync::Mutex<mpsc::UnboundedSender<Outbound>>,
}

pub enum HubEvent {
    Connected,
    Disconnected { reason: String },
}

pub fn apply_disconnect(rt: &mut HubRuntime, reason: &str) -> HubEvent {
    rt.connected = false;
    rt.peers.clear();
    rt.authorized.clear();
    HubEvent::Disconnected { reason: reason.to_string() }
}

fn set_connected(
    shared: &HubShared,
    handle: &tauri::AppHandle,
    connected: bool,
    reason: &str,
) {
    {
        let mut rt = shared.runtime.lock().unwrap();
        rt.connected = connected;
        if !connected {
            rt.peers.clear();
            rt.authorized.clear();
        }
    }
    use tauri::Emitter;
    let _ = handle.emit(
        "hub-state",
        serde_json::json!({ "connected": connected, "reason": reason }),
    );
}

/// One hub session: connect, hello, welcome, pump until closed.
/// Returns `Ok(())` only when the outbound channel closes (app shutdown).
/// `on_up` runs after a valid welcome (backoff reset + connected state).
async fn run_session(
    url: &str,
    device_id: &str,
    name: &str,
    outbound_rx: &mut mpsc::UnboundedReceiver<Outbound>,
    on_up: &mut impl FnMut(),
) -> Result<(), String> {
    let (ws, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("hub connect failed: {e}"))?;
    let (mut sink, mut stream) = ws.split();

    let hello = serde_json::json!({
        "type": "hello",
        "name": name,
        "kind": "app",
        "sid": device_id,
        "caps": ["hub-v1"],
    });
    sink.send(tokio_tungstenite::tungstenite::protocol::Message::text(
        hello.to_string(),
    ))
    .await
    .map_err(|e| format!("hub hello failed: {e}"))?;

    // Welcome is mandatory before the session counts as up.
    match stream.next().await {
        Some(Ok(tokio_tungstenite::tungstenite::protocol::Message::Text(t))) => {
            let v: serde_json::Value = serde_json::from_str(t.as_str())
                .map_err(|e| format!("bad welcome: {e}"))?;
            if v["type"] != "welcome" {
                return Err("expected welcome".into());
            }
        }
        _ => return Err("closed before welcome".into()),
    }
    on_up();

    loop {
        tokio::select! {
            out = outbound_rx.recv() => {
                match out {
                    Some(Outbound { text, ack }) => {
                        let result = sink
                            .send(tokio_tungstenite::tungstenite::protocol::Message::text(text))
                            .await
                            .map_err(|e| format!("hub send failed: {e}"));
                        if let Some(ack) = ack {
                            let _ = ack.send(
                                result.as_ref().map(|_| ()).map_err(|e| e.clone()),
                            );
                        }
                        if result.is_err() {
                            return Err("hub send failed".into());
                        }
                    }
                    None => return Ok(()), // channel closed: app shutting down
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(tokio_tungstenite::tungstenite::protocol::Message::Text(t))) => {
                        // Slices 2-5 dispatch peers/relay frames here.
                        // Slice 1 parses defensively and ignores unknown frames.
                        match serde_json::from_str::<serde_json::Value>(t.as_str()) {
                            Ok(v) => println!("[hub] frame type: {}", v["type"]),
                            Err(e) => println!("[hub] dropped malformed frame: {e}"),
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::protocol::Message::Ping(p))) => {
                        let _ = sink
                            .send(tokio_tungstenite::tungstenite::protocol::Message::Pong(p))
                            .await;
                    }
                    _ => return Err("socket closed".into()),
                }
            }
        }
    }
}

/// Outer loop: connect, on failure emit disconnect + wait with jitter, retry.
/// Runs until process exit; app shutdown drops the runtime and closes channels.
pub async fn hub_loop(
    shared: std::sync::Arc<HubShared>,
    device_id: String,
    name: String,
    handle: tauri::AppHandle,
) {
    let mut backoff = ReconnectBackoff::new();
    let mut rng = move || {
        let mut b = [0u8; 8];
        let _ = getrandom::fill(&mut b);
        u64::from_le_bytes(b)
    };
    loop {
        let (tx, mut rx) = mpsc::unbounded_channel::<Outbound>();
        *shared.outbound.lock().unwrap() = tx;
        let shared_up = shared.clone();
        let handle_up = handle.clone();
        let mut on_up = move || {
            backoff.reset();
            set_connected(&shared_up, &handle_up, true, "");
        };
        let outcome = run_session(&shared.url, &device_id, &name, &mut rx, &mut on_up).await;
        let reason = match &outcome {
            Ok(()) => "shutdown".to_string(),
            Err(r) => r.clone(),
        };
        let event = {
            let mut rt = shared.runtime.lock().unwrap();
            apply_disconnect(&mut rt, &reason)
        };
        if let HubEvent::Disconnected { reason } = event {
            use tauri::Emitter;
            let _ = handle.emit(
                "hub-state",
                serde_json::json!({ "connected": false, "reason": reason }),
            );
        }
        if outcome.is_ok() {
            return; // graceful shutdown
        }
        let delay = backoff.next_delay_with(&mut rng);
        tokio::time::sleep(delay).await;
    }
}
```

Add `use futures_util::{SinkExt, StreamExt};` at the top of the module. Notes:
- `t.as_str()` keeps the snippet valid for both `String` and `Utf8Bytes` text payloads across tungstenite versions.
- `tauri::Emitter` must be imported for `emit` in tauri 2 (lib.rs already does this; hub/mod.rs imports it locally in each fn that emits).

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml hub:: && cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 8 PASS; check clean.

- [ ] **Step 5: Report to orchestrator**

### Task 4: Wire into the app (`hub_status`)

**Files:**
- Modify: `src-tauri/src/lib.rs` (`AppState` ~line 55, setup block ~lines 823-861, `invoke_handler` ~line 1111)
- [ ] **Step 1: Extend `AppState` and construct `HubShared` before `app.manage`**

```rust
struct AppState {
    // ...existing fields unchanged...
    hub: std::sync::Arc<hub::HubShared>,
}
```

In the setup block, construct the hub shared state **before** `AppState` is built (adjust local names to the actual code at `lib.rs:823-861` — the `Connection` from `open_history_db` is reused for `device_id`):

```rust
let device_id = hub::identity::read_or_create_device_id(&conn)
    .unwrap_or_else(|_| hub::identity::new_uuid().expect("entropy unavailable"));
let (dead_tx, _dead_rx) = tokio::sync::mpsc::unbounded_channel::<hub::Outbound>();
let hub_shared = std::sync::Arc::new(hub::HubShared {
    url: hub::identity::resolve_hub_url(std::env::var("LANCHAT_HUB_URL").ok().as_deref()),
    runtime: std::sync::Mutex::new(hub::HubRuntime::default()),
    outbound: std::sync::Mutex::new(dead_tx),
});
// AppState { ..., hub: hub_shared.clone() } — real from the first manage().
let hub_for_loop = hub_shared.clone();
let handle_for_loop = app.handle().clone();
let name = device_name();
let device_for_loop = device_id.clone();
tauri::async_runtime::spawn(async move {
    hub::hub_loop(hub_for_loop, device_for_loop, name, handle_for_loop).await;
});
```

- [ ] **Step 2: Add the status command and register it**

```rust
#[tauri::command]
fn hub_status(state: tauri::State<std::sync::Arc<AppState>>) -> hub::HubStatus {
    let rt = state.hub.runtime.lock().unwrap();
    hub::HubStatus {
        url: state.hub.url.clone(),
        connected: rt.connected,
        peers: rt.peers.len(),
    }
}
```

Add `hub_status` to `generate_handler![]` (line ~1111).

- [ ] **Step 3: Verify**

Run: `cargo check --manifest-path src-tauri/Cargo.toml && cargo test --manifest-path src-tauri/Cargo.toml`
Expected: clean check, all tests PASS.

Manual smoke (optional, no deploy): `LANCHAT_HUB_URL=ws://localhost:8788/hub npm start` in `web/`, then `npm run tauri dev` — server log shows one more peer; killing the hub shows reconnect backoff in app logs.

- [ ] **Step 4: Report to orchestrator**

**Slice 1b exit criteria:** `cargo test` green (8 hub tests + full suite); desktop reconnects to the hub with backoff; `hub-state` events emitted; no presence/pairing/messages — explicitly a foundation.

---

## Slice 1c — Foundation: desktop status UI

**Files:**
- Modify: `src/lib/backend.ts`, `src/App.tsx` (topbar chip, ~line 613), `src/App.css`

- [ ] **Step 1: Bindings** (append to `src/lib/backend.ts`; `invoke`/`listen` imports already exist there, lines 1-2)

```ts
export interface HubStatus {
  url: string;
  connected: boolean;
  peers: number;
}

export function hubStatus(): Promise<HubStatus> {
  return invoke<HubStatus>("hub_status");
}

export function onHubState(
  handler: (s: { connected: boolean; reason?: string }) => void,
): Promise<() => void> {
  return listen("hub-state", (event) => handler(event.payload));
}
```

- [ ] **Step 2: UI (Spanish copy preserved)**

In `App.tsx` add `const [hub, setHub] = useState<HubStatus | null>(null);`, load on mount next to `getDownloadFolder()` (line ~255), registering the unlisten in `offs` like the existing listeners:

```ts
hubStatus().then(setHub).catch(() => {});
onHubState((s) => setHub((h) => (h ? { ...h, connected: s.connected } : h))).then(
  (fn) => offs.push(fn),
);
```

Replace the static chip at line ~613:

```tsx
<span className="net-chip">
  <Wifi size={13} aria-hidden />
  Red local
  {hub && (
    <span
      className={`hub-dot ${hub.connected ? "is-online" : "is-offline"}`}
      title={hub.connected ? "Nube conectada" : "Nube sin conexión"}
    />
  )}
</span>
```

Style in `src/App.css` (dot only, no layout change):

```css
.hub-dot { width: 7px; height: 7px; border-radius: 999px; display: inline-block; }
.hub-dot.is-online { background: #2E7D46; }
.hub-dot.is-offline { background: #A6AAB1; }
```

- [ ] **Step 3: Verify**

Run: `npm run build`
Expected: tsc + vite build clean.

- [ ] **Step 4: Report to orchestrator**

**Slice 1c exit criteria:** chip reflects live `hub-state`; `npm run build` green.

---

## Slices 2–6 (specified, executed after Slices 1a–1c review)

Each slice starts by expanding its contracts into full TDD steps (failing test → run → implement → run → report) following the Slice 1a pattern, and each stays a ≤ 400-line review unit. Contracts below are binding; they prevent drift between slices.

### Slice 2 — Presence (~270)

**Files:** `web/server.js`, `web/public/app.js`, `web/test/hub-identity.test.js` (new), `src-tauri/src/hub/protocol.rs`, `src-tauri/src/hub/mod.rs`, `src/lib/backend.ts`, `src/App.tsx`

Contracts:
- Hub `hello` stores `sid` (sanitized `String(m.sid || "").replace(/[^A-Za-z0-9_-]/g, "").slice(0, 64) || null`) and `caps` (first 8 strings). Peers entries and relay envelopes add `sid` / `from_sid`. Peers without sid stay `null` (legacy browsers keep working web↔web).
- Browser `app.js`: **per-page identity, no storage** — at module scope: `const sid = crypto.randomUUID();` and hello gains `sid`, `caps: ["web-v1"]`. Reload and tab duplication mint a new identity automatically (fresh module evaluation). No `sessionStorage`/`localStorage` anywhere in this flow.
- Desktop `protocol.rs`: parse `peers` frames into `Vec<HubPeer>`; pure reconciliation `fn reconcile_peers(rt: &mut HubRuntime, list: Vec<HubPeer>, own_device_id: &str)` — filters `sid == own_device_id`, upserts by conn id, **drops peers and their `authorized` grants and `contact_keys` entries whose conn id vanished** (tab closed or same page back on a new conn id → re-pair required), emits `hub-presence` `{ online: [{ key: "<grant key if a grant exists for this peer, else null>", sid, name }] }` (empty list on disconnect). Presence is display/matching info only: the durable conversation key is the desktop-minted `hub:<native-uuid>` stored with the grant (Slice 3a) — the browser sid is never persisted as a key.
- `App.tsx`: merge presence into `devices` for `hub:` keys (name from presence, `online` flag only while connected). `pushEntry` fallback for `hub:` keys renders `device.name || "Contacto web"` until presence names it.

Test examples:

```js
// web/test/hub-identity.test.js (node:test, ws client)
test("hello sid surfaces in peers list and relay envelope", async () => { /* two sockets, hello with sid a/b, peers entries carry sid; relay a→b carries from_sid === "a" */ });
test("hello without sid keeps legacy null sid", async () => { /* peers entry sid === null */ });
test("reconnect keeps sid only if the page does — storage plays no role", async () => { /* same-page reconnect reuses in-memory sid; a fresh page object generates a different one */ });
```

```rust
// protocol.rs tests: reconcile_peers drops grants for vanished conn ids,
// keeps live ones, filters own device_id.
```

### Slice 3a — Pairing desktop engine (~260)

**Files:** `src-tauri/src/hub/pairing.rs`, `src-tauri/src/hub/mod.rs` (dispatch wiring)

Contracts (all pure in `pairing.rs`, injectable `Instant` — no I/O, fully unit-testable):
- `PairCode { code: String, peer: String, req_id: String, attempts: u8, expires: Instant }`; state = `Vec<PairCode>` capped at **2** concurrent entries (volatile, never persisted).
- `fn new_pair_code(rng: &mut impl FnMut() -> u8) -> String` — 8 digits from 5 CSPRNG bytes: `format!("{:08}", (u32::from_le_bytes(b) as u64 % 100_000_000))`.
- `enum PairDecision { Reused(PairCode), Issued(PairCode), Busy, RateLimited }` and `fn on_pair_request(&mut self, peer: &str, req_id: &str, now: Instant) -> PairDecision`:
  - same peer + same live `req_id` → `Reused` (no regeneration spam);
  - same peer + new `req_id` → replaces that peer's entry (`Issued`);
  - 2 slots occupied by other peers → `Busy`;
  - per-peer ring: ≥ 10 generations in rolling 10 min → `RateLimited` (that peer only).
- `enum VerifyOutcome { Accepted, WrongCode, Unknown }` and `fn on_pair_verify(&mut self, peer: &str, req_id: &str, code: &str, now: Instant) -> VerifyOutcome` — requires live code, matching peer **and** `req_id`; wrong/expired → `attempts += 1`, `WrongCode`; `attempts >= 3` drops the entry; match → returns `Accepted`, caller inserts `PairGrant { req_id }` into `HubRuntime.authorized[peer]`, records the minted contact key in `HubRuntime.contact_keys[peer]` (conn id → `hub:<native-uuid>`, minted via `identity::new_uuid()`), and clears the entry.
- Dispatch in `mod.rs`: sends `pair-ok`/`pair-error {reqId, reason}` **only** to the requesting peer; on `Accepted` the caller **mints the durable contact key `hub:<native-uuid>`** (`identity::new_uuid()`), records it in `HubRuntime.contact_keys[peer]`, and emits `hub-pair-request { code }` and `hub-pair-done { key, name }` to the frontend — the key is desktop-generated per successful pairing (a re-pair mints a new key; the old conversation stays offline in history), never derived from the untrusted browser sid.
- LAN PIN isolation: no code path in `hub/` reads or writes `own_pin`/`session_pin`.

Test examples:

```rust
#[test] fn pair_code_is_8_digits() { /* format over 1000 codes */ }
#[test] fn verify_rejects_wrong_peer_reqid_and_third_strike() { /* 3 wrong codes invalidate; correct code after invalidation fails */ }
#[test] fn two_peers_pair_concurrently_third_gets_busy() { /* cap 2 enforced per peer, not globally mutated */ }
#[test] fn generation_ring_limits_spam_per_peer() { /* 11th generation inside window is refused for that peer only */ }
#[test] fn pairing_never_touches_own_pin() { /* own_pin identical before/after a full pair-ok */ }
```

### Slice 3b — Pairing browser side + UI (~180)

**Files:** `web/public/app.js`, `web/public/pairing-attempts.js`, `web/test/pairing-attempts.test.js`, `src/lib/backend.ts`, `src/App.tsx`

Contracts:
- Browser `app.js`: `startPairing()` sends `{ type: "pair-request", reqId: newId() }`; `confirmPairing()` sends `{ type: "pair-verify", code, reqId }`; `pair-ok`/`pair-error` handlers match on `reqId`.
- `pairing-attempts.js`: keyed by `reqId` (`start(reqId)`, `accept(reqId)` — both must match).
- Desktop UI: overlay mirroring the existing LAN `pairRequest` card (code + "El código vence en 2 minutos."); cancel does nothing server-side (code expires by TTL).
- **Desktop initiation guidance (no fake success):** clicking an unpaired `hub:` contact and sending surfaces the `pair-required` failure as a Spanish guidance entry: "Vinculá desde el navegador: abrí LAN-Chat en el navegador y tocá «Vincular» en el contacto de esta computadora." No auto-auth, no desktop-sent `pair-request`.

Test examples:

```js
// web/test/pairing-attempts.test.js additions
test("late pair-ok with stale reqId is ignored", () => { /* start("r1"); start("r2"); accept("r1") → false */ });
```

### Slice 4a — History: inert DB-only module (~250)

**Files:**
- Create: `src-tauri/src/history.rs`
- Modify: `src-tauri/src/lib.rs` (one line: `mod history;` — registered but **inert**)

**This slice ships NO Tauri commands, NO frontend changes, NO UI cutover.** It cannot change app behavior by construction: `history` is a DB-only module exercised only by its own tests. The commands and the frontend switch (retiring the legacy snapshot `save_history`, `lib.rs:314`) ship **atomically in Slice 4b** — no release runs the snapshot API and the append API together (mixed API is a spec violation). Implementation budget: **≤ 400 lines excluding the `#[cfg(test)]` module**; tests live in the same file under `#[cfg(test)]`, separate from impl. All tests open **real SQLite files in the OS temp dir** — never `open_in_memory` — so on-disk migration and file-preservation behavior is actually exercised. The staging/contact breadth of Slices 3/5 is **not** a prerequisite here: no staging, no grants, no hub session — pure SQLite.

Delivers: guarded idempotent migration (`history_legacy_imported` flag), two-level staleness tokens (`hist_epoch` global, `hist_rev:<key>` per key), transactional `append_message`, guarded `patch_message_state` (state/read columns only, never content), ungated inbound `accept_text` (`INSERT` only — never `REPLACE`; duplicate/conflict semantics), DB-only gated deletes, and the invariant that **deletion never touches the filesystem** (no `fs::remove_file` in non-test code; sent/received files are byte-identical after one-key and all-history deletes).

Existing schema (never altered destructively): `messages(id TEXT PRIMARY KEY, device_key TEXT NOT NULL, mine INTEGER NOT NULL, text TEXT NOT NULL, at INTEGER NOT NULL, state TEXT, file_path TEXT, read INTEGER NOT NULL DEFAULT 0)` (`lib.rs:349`), `settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)` (`lib.rs:363`).

#### Task 5: Inert module, guarded migration, token gate, append, patch (tests first)

**Files:**
- Create: `src-tauri/src/history.rs`
- Modify: `src-tauri/src/lib.rs` (module registration only)

- [ ] **Step 1: Register the inert module**

In `src-tauri/src/lib.rs`, next to the other module declarations, add exactly one line (do **not** touch `generate_handler!`):

```rust
mod history;
```

- [ ] **Step 2: Write the failing tests**

Create `src-tauri/src/history.rs` containing only the test scaffolding:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Real SQLite file in the OS temp dir (never in-memory), unique per call.
    fn temp_db(label: &str) -> Connection {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "lan-chat-hist-{label}-{}-{n}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path); // test hygiene only
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "CREATE TABLE messages (
                id TEXT PRIMARY KEY,
                device_key TEXT NOT NULL,
                mine INTEGER NOT NULL,
                text TEXT NOT NULL,
                at INTEGER NOT NULL,
                state TEXT,
                file_path TEXT,
                read INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn
    }

    fn entry(id: &str, text: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            mine: true,
            text: text.into(),
            at: 1,
            state: None,
            file_path: None,
            read: false,
        }
    }

    fn token(conn: &Connection, key: &str) -> HistToken {
        HistToken {
            epoch: hist_epoch(conn).unwrap(),
            rev: hist_rev(conn, key).unwrap(),
        }
    }

    fn count_rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn migration_is_idempotent_and_legacy_rows_survive() {
        let mut conn = temp_db("migr");
        conn.execute(
            "INSERT INTO messages (id, device_key, mine, text, at, state, file_path, read)
             VALUES ('l1', 'k', 1, 'legacy', 1, 'sent', NULL, 0)",
            [],
        )
        .unwrap();
        ensure_history_schema(&conn).unwrap();
        ensure_history_schema(&conn).unwrap(); // second run: no-op, no error
        let (text, hash): (String, Option<String>) = conn
            .query_row(
                "SELECT text, content_hash FROM messages WHERE id = 'l1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(text, "legacy");
        assert!(hash.is_none(), "legacy rows keep NULL content_hash");
        assert!(has_flag(&conn, LEGACY_FLAG).unwrap());
    }

    #[test]
    fn append_is_gated_and_delete_bumps_rev() {
        let mut conn = temp_db("gate");
        ensure_history_schema(&conn).unwrap();
        let t0 = token(&conn, "k");
        assert_eq!(t0, HistToken { epoch: 0, rev: 0 });
        append_message(&mut conn, &entry("m1", "hola"), "k", t0).unwrap();
        let fresh = delete_conversation(&mut conn, "k").unwrap();
        assert_eq!(fresh, HistToken { epoch: 0, rev: 1 });
        // Stale token (pre-delete) is rejected; nothing resurrects.
        assert_eq!(
            append_message(&mut conn, &entry("m1", "hola"), "k", t0).unwrap_err(),
            "stale-history"
        );
        assert_eq!(count_rows(&conn), 0);
        // Fresh token (post-delete) is accepted.
        append_message(&mut conn, &entry("m2", "nuevo"), "k", fresh).unwrap();
        assert_eq!(count_rows(&conn), 1);
    }

    #[test]
    fn patch_touches_only_state_and_stale_patch_is_rejected() {
        let mut conn = temp_db("patch");
        ensure_history_schema(&conn).unwrap();
        let t0 = token(&conn, "k");
        append_message(&mut conn, &entry("m1", "hola"), "k", t0).unwrap();
        assert!(patch_message_state(&mut conn, "k", "m1", Some("sent"), false, t0).unwrap());
        let (text, state): (String, Option<String>) = conn
            .query_row(
                "SELECT text, state FROM messages WHERE id = 'm1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(text, "hola"); // a patch never rewrites content
        assert_eq!(state.as_deref(), Some("sent"));
        delete_conversation(&mut conn, "k").unwrap();
        // Out-of-order: stale token rejected even though the row is gone.
        assert_eq!(
            patch_message_state(&mut conn, "k", "m1", Some("read"), true, t0).unwrap_err(),
            "stale-history"
        );
        // Honest no-op for a nonexistent row under a fresh token.
        let fresh = token(&conn, "k");
        assert!(!patch_message_state(&mut conn, "k", "ghost", Some("sent"), false, fresh).unwrap());
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml history::`
Expected: **FAIL** — `HistoryEntry`, `HistToken`, `hist_epoch`, `hist_rev`, `ensure_history_schema`, `append_message`, `patch_message_state`, `delete_conversation`, `has_flag`, `LEGACY_FLAG` not defined.

- [ ] **Step 4: Implement** (above the tests module)

```rust
// DB-only history layer: transactional per-message append, guarded state
// patches, two-level stale gate (global epoch + per-key rev), inbound
// accept (INSERT only, never REPLACE), DB-only deletes. Inert in Slice 4a:
// no Tauri commands, no UI — the atomic cutover ships in Slice 4b.
// DELETION IS DB-ONLY: no fs mutation exists in non-test code here.

use rusqlite::Connection;
use std::collections::HashMap;

pub const EPOCH_KEY: &str = "hist_epoch";
pub const REV_PREFIX: &str = "hist_rev:";
pub const LEGACY_FLAG: &str = "history_legacy_imported";

/// Staleness token carried by every append/patch: global epoch + per-key rev.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistToken {
    pub epoch: i64,
    pub rev: i64,
}

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub id: String,
    pub mine: bool,
    pub text: String,
    pub at: i64,
    pub state: Option<String>,
    pub file_path: Option<String>,
    pub read: bool,
}

pub enum Accept {
    New,
    Duplicate,
}

pub fn has_flag(conn: &Connection, key: &str) -> Result<bool, String> {
    match conn.query_row("SELECT 1 FROM settings WHERE key = ?1", [key], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

fn get_num(conn: &Connection, key: &str) -> Result<i64, String> {
    match conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        [key],
        |r| r.get::<_, String>(0),
    ) {
        Ok(v) => v.parse::<i64>().map_err(|e| e.to_string()),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0), // absent = 0
        Err(e) => Err(e.to_string()),
    }
}

fn set_num(conn: &Connection, key: &str, value: i64) -> Result<(), String> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value.to_string()],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

pub fn hist_epoch(conn: &Connection) -> Result<i64, String> {
    get_num(conn, EPOCH_KEY)
}

pub fn hist_rev(conn: &Connection, key: &str) -> Result<i64, String> {
    get_num(conn, &format!("{REV_PREFIX}{key}"))
}

fn check_token(conn: &Connection, key: &str, t: HistToken) -> Result<(), String> {
    if hist_epoch(conn)? != t.epoch || hist_rev(conn, key)? != t.rev {
        return Err("stale-history".into());
    }
    Ok(())
}

/// Idempotent migration, run exactly once behind `history_legacy_imported`:
/// additive `content_hash` column + scoped uniqueness index. Legacy snapshot
/// rows stay valid (`content_hash` NULL → dedup falls back to `text`
/// equality). The flag guarantees the one-time legacy handling can never run
/// twice — a re-run after deletes would mislabel rows.
pub fn ensure_history_schema(conn: &Connection) -> Result<(), String> {
    if has_flag(conn, LEGACY_FLAG)? {
        return Ok(());
    }
    conn.execute("ALTER TABLE messages ADD COLUMN content_hash TEXT", [])
        .or_else(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ColumnExists => Ok(0),
            other => Err(other),
        })
        .map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_key_id
         ON messages(device_key, id)",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, '1')",
        [LEGACY_FLAG],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Appends one desktop-generated message in its own transaction, gated by
/// the caller's token. The only write path for `mine = 1` rows; there is no
/// bulk snapshot save in this model, so a retried send patches instead of
/// re-appending.
pub fn append_message(
    conn: &mut Connection,
    entry: &HistoryEntry,
    device_key: &str,
    token: HistToken,
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    check_token(&tx, device_key, token)?;
    tx.execute(
        "INSERT INTO messages
         (id, device_key, mine, text, at, state, file_path, read, content_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
        rusqlite::params![
            entry.id,
            device_key,
            entry.mine,
            entry.text,
            entry.at,
            entry.state,
            entry.file_path,
            entry.read
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Patches ONLY the delivery/read columns, gated by the token; content is
/// never touched and nothing is reinserted. A stale/out-of-order token is
/// rejected. Returns Ok(false) for a nonexistent row — an honest no-op,
/// never faked success.
pub fn patch_message_state(
    conn: &mut Connection,
    device_key: &str,
    id: &str,
    new_state: Option<&str>,
    read: bool,
    token: HistToken,
) -> Result<bool, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    check_token(&tx, device_key, token)?;
    let n = tx
        .execute(
            "UPDATE messages SET state = ?1, read = ?2
             WHERE device_key = ?3 AND id = ?4",
            rusqlite::params![new_state, read, device_key, id],
        )
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(n > 0)
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml history::`
Expected: 3 PASS.

- [ ] **Step 6: Report to orchestrator** (no commands registered, no commit — see Release gate)

#### Task 6: Gated deletes, epoch empty-key case, inbound accept, file preservation (tests first)

**Files:**
- Modify: `src-tauri/src/history.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src-tauri/src/history.rs`:

```rust
    #[test]
    fn delete_all_epoch_rejects_stale_even_for_empty_keys() {
        let mut conn = temp_db("epoch");
        ensure_history_schema(&conn).unwrap();
        append_message(&mut conn, &entry("m1", "hola"), "a", token(&conn, "a")).unwrap();
        let old = HistToken { epoch: 0, rev: 0 };
        let epoch = delete_all_history(&mut conn).unwrap();
        assert_eq!(epoch, 1);
        assert_eq!(count_rows(&conn), 0);
        // THE empty-key hole: key "b" has no rows and no rev metadata, but
        // its stale epoch token is still rejected — per-key revs alone
        // cannot protect it, the global epoch does.
        assert_eq!(
            append_message(&mut conn, &entry("m2", "x"), "b", old).unwrap_err(),
            "stale-history"
        );
        // Current-epoch token for a brand-new key is accepted.
        append_message(&mut conn, &entry("m3", "y"), "b", token(&conn, "b")).unwrap();
        // Stale patches are rejected too, not just appends.
        assert_eq!(
            patch_message_state(&mut conn, "a", "m1", Some("sent"), false, old).unwrap_err(),
            "stale-history"
        );
    }

    #[test]
    fn inbound_accept_duplicate_conflict_and_after_delete() {
        let mut conn = temp_db("accept");
        ensure_history_schema(&conn).unwrap();
        assert!(matches!(
            accept_text(&mut conn, "k", "m1", "hola", 1, Some("aa")).unwrap(),
            Accept::New
        ));
        // Same id + same content: duplicate — no second row, no side effect.
        assert!(matches!(
            accept_text(&mut conn, "k", "m1", "hola", 2, Some("aa")).unwrap(),
            Accept::Duplicate
        ));
        assert_eq!(count_rows(&conn), 1);
        // Same id + different content: conflict reject, original intact.
        assert_eq!(
            accept_text(&mut conn, "k", "m1", "otra", 3, Some("bb")).unwrap_err(),
            "conflict"
        );
        // Same id + different conversation key: conflict reject (global PK).
        assert_eq!(
            accept_text(&mut conn, "otra", "m1", "hola", 4, Some("aa")).unwrap_err(),
            "conflict"
        );
        assert_eq!(count_rows(&conn), 1);
        // Inbound is NOT token-gated: any caller token is ignored — a
        // genuinely new message after any delete is accepted with the
        // current epoch/rev context.
        assert!(matches!(
            accept_text(&mut conn, "k", "m9", "post-delete", 5, None).unwrap(),
            Accept::New
        ));
        // Deletion is real (no tombstone blocks the id): after the delete,
        // the same id + content arrives again as genuinely new.
        delete_conversation(&mut conn, "k").unwrap();
        assert!(matches!(
            accept_text(&mut conn, "k", "m1", "hola", 6, Some("aa")).unwrap(),
            Accept::New
        ));
        // Legacy NULL-hash row: duplicate detection falls back to text.
        assert!(matches!(
            accept_text(&mut conn, "k", "m1", "hola", 7, None).unwrap(),
            Accept::Duplicate
        ));
    }

    #[test]
    fn deletion_is_db_only_files_byte_identical_after_one_and_all() {
        let mut conn = temp_db("files");
        ensure_history_schema(&conn).unwrap();
        let fa = temp_file("fa", b"file A bytes");
        let fb = temp_file("fb", b"file B bytes");
        let a_bytes = std::fs::read(&fa).unwrap();
        let b_bytes = std::fs::read(&fb).unwrap();
        let mut ea = entry("m1", "con archivo");
        ea.file_path = Some(fa.to_string_lossy().into_owned());
        let mut eb = entry("m2", "con archivo");
        eb.file_path = Some(fb.to_string_lossy().into_owned());
        append_message(&mut conn, &ea, "a", token(&conn, "a")).unwrap();
        append_message(&mut conn, &eb, "b", token(&conn, "b")).unwrap();

        delete_conversation(&mut conn, "a").unwrap();
        assert_eq!(std::fs::read(&fa).unwrap(), a_bytes, "per-key delete keeps bytes");
        assert!(fa.exists() && fa.parent().unwrap().is_dir());

        delete_all_history(&mut conn).unwrap();
        assert_eq!(std::fs::read(&fa).unwrap(), a_bytes, "delete_all keeps bytes");
        assert_eq!(std::fs::read(&fb).unwrap(), b_bytes, "delete_all keeps bytes");
        assert!(fa.exists() && fb.exists());
        assert_eq!(count_rows(&conn), 0);
    }

    fn temp_file(label: &str, bytes: &[u8]) -> std::path::PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "lan-chat-hist-file-{label}-{}-{n}.bin",
            std::process::id()
        ));
        std::fs::write(&p, bytes).unwrap();
        p
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml history::`
Expected: **FAIL** — `delete_all_history`, `Accept`, `accept_text` not defined (new tests); prior 3 still PASS.

- [ ] **Step 3: Implement** (above the tests module)

```rust
/// Deletes one conversation's rows and bumps `hist_rev:<key>` in one
/// transaction; returns the key's fresh token. Rev metadata is kept after
/// the rows are gone, so stale callers are rejected even for keys that no
/// longer exist in `messages`. DB-ONLY: files and folders referenced by
/// deleted rows are never touched.
pub fn delete_conversation(conn: &mut Connection, device_key: &str) -> Result<HistToken, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM messages WHERE device_key = ?1", [device_key])
        .map_err(|e| e.to_string())?;
    let token = HistToken {
        epoch: hist_epoch(&tx)?,
        rev: hist_rev(&tx, device_key)? + 1,
    };
    set_num(&tx, &format!("{REV_PREFIX}{device_key}"), token.rev)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(token)
}

/// Deletes every row and bumps the global `hist_epoch` in one transaction.
/// The epoch — not per-key revs alone — closes the empty-key hole: after
/// this, a stale token is rejected even for a key with no remaining rows or
/// rev metadata. Returns the new epoch. DB-ONLY: never touches files.
pub fn delete_all_history(conn: &mut Connection) -> Result<i64, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM messages", []).map_err(|e| e.to_string())?;
    let epoch = hist_epoch(&tx)? + 1;
    set_num(&tx, EPOCH_KEY, epoch)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(epoch)
}

/// Inbound acceptance record: one transaction, INSERT only — never
/// `INSERT OR REPLACE`. NOT token-gated: caller tokens play no role here; a
/// genuinely new message after any delete is accepted with the current
/// epoch/rev context. Same id, same key, same content (hash match; legacy
/// NULL-hash rows fall back to `text` equality) → `Duplicate` with no
/// second row; same id with different content or under a different key →
/// conflict. The row IS the acceptance record: the caller emits UI events
/// only after this returns `Ok(New)`.
pub fn accept_text(
    conn: &mut Connection,
    device_key: &str,
    id: &str,
    text: &str,
    at: i64,
    hash: Option<&str>,
) -> Result<Accept, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let existing: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT device_key, text, content_hash FROM messages WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other.to_string()),
        })?;
    if let Some((key0, text0, hash0)) = existing {
        tx.commit().map_err(|e| e.to_string())?; // read-only branch
        if key0 != device_key {
            return Err("conflict".into()); // same id, different conversation
        }
        let same = match (hash0.as_deref(), hash) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => text0 == text, // legacy fallback
        };
        if !same {
            return Err("conflict".into()); // same id, different payload
        }
        return Ok(Accept::Duplicate); // no second row, no second UI event
    }
    tx.execute(
        "INSERT INTO messages
         (id, device_key, mine, text, at, state, file_path, read, content_hash)
         VALUES (?1, ?2, 0, ?3, ?4, NULL, NULL, 0, ?5)",
        rusqlite::params![id, device_key, text, at, hash],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Accept::New)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml history:: && cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 6 PASS; check clean.

- [ ] **Step 5: Verify the slice is genuinely inert**

Run: `rg -n "history" src-tauri/src/lib.rs` → exactly one relevant match (`mod history;`), nothing in `generate_handler!`. Run: `rg -n "remove_file|fs::remove" src-tauri/src/history.rs` → matches only inside `#[cfg(test)] mod tests` (temp hygiene), none in non-test code. Run: `cargo test --manifest-path src-tauri/Cargo.toml` → full suite green.

- [ ] **Step 6: Report to orchestrator**

**Slice 4a exit criteria:** `cargo test` green (6 history tests + full suite); `history.rs` impl ≤ 400 lines excluding `#[cfg(test)]`; **no** Tauri commands registered for `history`, **no** frontend files touched; deletion path provably file-preserving (byte-identical real-tempfile test, one-key and all); migration idempotent behind `history_legacy_imported`.

### Slice 4b — Chat: atomic cutover + authorized inbound + honest outbound (~330)

**Files:** `src-tauri/src/lib.rs` (command exposure + `save_history` retirement), `src-tauri/src/hub/inbox.rs`, `src-tauri/src/hub/mod.rs`, `src-tauri/src/hub/protocol.rs`, `src/lib/backend.ts`, `src/lib/history.ts`, `src/App.tsx`

Contracts:
- **Atomic cutover (same slice, same release):** expose `load_history`, `append_message`, `patch_message_state`, `delete_conversation`, `delete_all_history` as Tauri commands and switch `history.ts`/`App.tsx` to them **in one change**, removing the legacy snapshot `save_history` command in the same change. No release ships with the old snapshot API and the new append API both live (mixed API is a spec violation). There is **no snapshot autosave anymore**: appends happen at send/receive time; every state change goes through `patch_message_state` with the token from the last `load_history`; retries patch state, never re-append; on `stale-history` the frontend reloads that conversation from the DB before continuing (documented, tested).
- `history.rs` acceptance (implemented in Slice 4a; used by chat here, files in 5a) — `inbox.rs` contributes authorization + hashing around it:

```rust
// src-tauri/src/history.rs (Slice 4a) — inbox.rs computes `hash`
// (sha2::Sha256 hex of the payload bytes) after authorizing the sender,
// then calls this. The row IS the acceptance record: one transaction,
// no tombstone before persist, INSERT only — never INSERT OR REPLACE.
pub enum Accept { New, Duplicate }
pub fn accept_text(
    conn: &mut rusqlite::Connection,
    key: &str,
    id: &str,
    text: &str,
    at: i64,
    hash: Option<&str>,
) -> Result<Accept, String>;
```

  Behavior: row exists under a different key → `Err("conflict")`; same key + equal content (hash match; legacy NULL-hash rows fall back to `text` equality) → `Ok(Duplicate)` (caller skips every side effect); same key + different content → `Err("conflict")`; no row → `INSERT` (`mine = 0`, `state = NULL`) → `Ok(New)`. Not token-gated: a genuinely new message after any delete is accepted with the current epoch/rev context.
- Inbound dispatch in `mod.rs`: require `from_id` live in `peers`, `from_sid` present, grant in `authorized[from_id]` — else drop + debug log. Resolve the conversation key from the grant's minted `hub:<native-uuid>`. Mint `id` via `identity::new_uuid()` when payload id is empty. `Ok(New)` → **persist before** `emit("message-received", ...)` (same event name/payload shape as the existing bridge, `lib.rs:635`/`lib.rs:941`). `Duplicate` → no event, no row. `Conflict` → log only.
- Outbound `hub_send_text(state, key, text, id) -> Result<String, String>`: key must be a grant-backed `hub:<native-uuid>`; **not connected** → `Err("hub-unavailable")`; peer conn id absent from runtime → `Err("peer-offline")`; peer present but no grant → `Err("pair-required")`; else build relay payload `{type:"chat", text, kind:"app", id}`, create a `oneshot` ack, send `Outbound` through `HubShared.outbound`, await the ack (5 s timeout → `Err("hub-unavailable")`). `"sent"` is returned **only** when the sink write succeeded, and the frontend maps it to `patchMessageState(key, id, "sent")` — never `delivered`/`read`.
- `App.tsx` `send()`/`retry()`: `key.startsWith("hub:")` → `hubSendText`; errors → `patchMessageState(..., "failed")` (+ guidance entry on `pair-required`). The legacy `web:*` fake-success branch stays untouched.
- `deleteConversation`/`deleteAll` invoke the commands exposed in this same atomic cutover; both surface the returned fresh token/epoch honestly and refresh from the DB.

Test examples:

```rust
#[test] fn inbound_chat_requires_sid_and_authorization() { /* unauthorized → dropped, no row, no event */ }
#[test] fn inbound_chat_persists_before_emit() { /* emit hook observes row already present */ }
#[test] fn duplicate_inbound_skips_event_and_row() { /* same id+hash → Accept::Duplicate, one row, no event */ }
#[test] fn conflicting_inbound_payload_is_rejected() { /* same id, different text → Err("conflict"), original row intact */ }
#[test] fn legacy_row_without_hash_uses_text_equality() { /* NULL content_hash duplicate detection */ }
#[tokio::test] async fn hub_send_text_unauthorized_fails_with_pair_required() { /* Err("pair-required"), no frame sent */ }
#[tokio::test] async fn hub_send_text_offline_peer_fails_honestly() { /* Err("peer-offline"), no state change */ }
```

### Slice 5a — Files: inbound staging pipeline (~260)

**Files:** `src-tauri/src/hub/inbox.rs`, `src-tauri/src/hub/mod.rs`, `src-tauri/src/lib.rs` (startup sweep call), `src/App.tsx` (system notice)

Contracts (`inbox.rs`):
- `fn stage_file(dir: &Path, id: &str, bytes: &[u8]) -> Result<PathBuf, String>` — write decoded bytes to `<dir>/.hub-stage/<id>` (create dir if missing).
- `fn accept_file(conn, key, id, name, stage_path, size, at, hash) -> Result<Accept, String>` — same duplicate/conflict rules as `accept_text` (row under a different key → conflict; NULL-hash legacy fallback compares `text` + `file_path` presence; **never** `INSERT OR REPLACE`); on `New`, insert row with `file_path = stage_path` **inside the same transaction** — row + staged file become visible together; a tx failure deletes the staged file (cleanup on error).
- `fn finalize_staged_file(conn, id, dir) -> Result<PathBuf, String>` — after commit: sanitize name via `Path::file_name`, collision loop `name.ext → name (1).ext…`, rename stage file into `dir`, `UPDATE messages SET file_path` to the final path. Rename/update failure keeps the row pointing at the (still valid) staged file + logs — nothing is lost, nothing faked.
- `fn sweep_stage_dir(conn, dir) -> usize` — at startup: delete `.hub-stage` entries with no referencing row (`file_path` match). No tombstones; crash windows converge to either an unreferenced file (swept) or a referenced one (kept). **Deletion-path invariant (persistence audit):** the sweep is the *only* `fs::remove_file` site in the codebase and never runs on the deletion path — `delete_conversation`/`delete_all_history` (Slice 4a) remove SQLite rows/metadata only; referenced sent/received files stay byte-identical after any delete (asserted by the Slice 4a real-tempfile test and re-asserted in Slice 6 E2E).
- Inbound guard: decoded size > 25 MiB → drop + `hub-file-error { key, name }` event (persisted system notice via existing system-entry path); disk-write failure → same event, no row.

Test examples:

```rust
#[test] fn file_name_sanitized_and_collision_suffixed() { /* "../../evil.txt" → "evil.txt"; second finalize → "evil (1).txt" */ }
#[test] fn oversized_inbound_file_rejected_with_error_notice() { /* >25MiB → no row, hub-file-error emitted */ }
#[test] fn tx_failure_removes_staged_file() { /* forced insert failure → stage dir empty */ }
#[test] fn sweep_removes_only_unreferenced_stage_files() { /* orphan swept, referenced file kept */ }
#[test] fn duplicate_file_inbound_writes_no_second_file() { /* same id+hash → one file on disk, no event */ }
```

### Slice 5b — Files: outbound + UI (~180)

**Files:** `src-tauri/src/hub/inbox.rs` (encoder) or a small `hub/base64.rs`, `src-tauri/src/lib.rs` (command), `src/lib/backend.ts`, `src/App.tsx`

Contracts:
- `hub_send_file(state, app, key, path, id) -> Result<String, String>`: `metadata.len() > 25 * 1024 * 1024` → `Err("file-too-large")`; read → base64 (hand-rolled encoder, ~20 lines, no new dep); authorization/peer checks identical to `hub_send_text`; relay `{type:"file", name, data, size, kind:"app", id}`; `"sent"` only on sink-write ack (same `Outbound` path as 4b).
- `App.tsx` `attachAndSend`/drag-drop: hub keys → `hubSendFile`; `file-too-large` → `patchEntry(..., "failed")` + Spanish copy mirroring web: "El archivo supera el máximo de 25 MB y no se envió."

Test examples:

```rust
#[test] fn base64_roundtrip_matches_known_vectors() { /* RFC 4648 vectors incl. padding */ }
#[tokio::test] async fn outbound_file_too_large_fails_without_reading() { /* Err("file-too-large") */ }
```

### Slice 6 — End-to-end verification + docs (~80)

Manual matrix (no deploy): local hub + two browsers + desktop via `LANCHAT_HUB_URL`; concurrent pairing of two browsers + third gets `busy`; 3 strikes + expiry; chat both ways; 25 MiB boundary both ways (25 MiB − 1 passes, 25 MiB + 1 refused); browser reload → new contact, old conversation offline, re-pair required; duplicate inbound replay → no double UI; delete conversation / delete all → sent/received files byte-identical, stale append/patch token rejected (epoch gate covers keys with no remaining rows), new inbound message still persists; desktop restart → history/names/offline contacts + stage sweep; hub kill → backoff reconnect, pairing invalidated; LAN regression (two desktops still pair and exchange with acks, frontend persistence riding the same atomic cutover). Update `web/README.md` (per-page sid, hub identity fields) and `PRODUCT.md` capability list.

---

## Release gate (orchestrator only)

The executor's contract ends at "report to orchestrator" per task: working tree dirty, all stated test commands green in the report. **No `git commit`, no `git push`, no deploy, no version bump, no PR** unless the orchestrator explicitly authorizes that specific action after reviewing the diff. Suggested review order: Slice 1a `hub/` pure modules → Slice 1b session loop + wiring diff → Slice 1c UI → per-slice on landing.

## Self-review notes

- **Audit coverage:** (1) per-page sid, no storage, grants bound to live conn id + reqId — Slices 2/3a; (2) receiver-verified acceptance: scoped uniqueness, conflict reject (different content **or** different key), duplicate skip, atomic row-as-record, persist-before-emit, INSERT-only (never `REPLACE`) — Slice 4a (row writes) + 4b (dispatch) + 5a (files); (3) delete ordering: global `hist_epoch` (delete_all, closes the empty-key hole) + per-key `hist_rev` (delete_conversation, metadata kept so stale callers are rejected even for nonexistent keys); inbound ungated so new-after-delete persists; **deletion is DB-only** — real-tempfile byte-identical test, no `fs::remove_file` in non-test code — Slice 4a + Slice 6 E2E; (4) snapshot model retired: transactional append + guarded state patch, retries patch instead of re-append, honest `stale-history` → reload — Slice 4a module + 4b atomic cutover (no mixed-API release); (5) bounded 2-slot peer/reqId pending map, per-peer rate ring, no global code, no PIN mutation — Slice 3a; (6) durable contact keys are desktop-minted grant UUIDs, browser sid never durable — Slices 2/3a/4b wording; (7) foundation split into 1a (~220) + 1b (~300) + 1c (~80), all units ≤ 400; Slice 4a fully expanded as an inert DB-only slice ≤ 400 impl lines (tests separate, real OS-tempfile SQLite); (8) every fully-expanded task is test-first; (9) all commit steps removed — explicit orchestrator Release gate; (10) `pair-required` guidance, browser-initiated pairing only — Slice 3b; (11) `hub/` module split + DB-only `history.rs`, `lib.rs` wiring only.
- Spec coverage: every design-doc decision maps to a slice; acceptance checklist items live in slice steps.
- Type consistency: `Outbound { text, ack }`, `PairGrant { req_id }` + `HubRuntime.contact_keys` (conn id → minted `hub:<native-uuid>`; both cleared on disconnect), `Accept { New, Duplicate }`, `HistToken { epoch, rev }`, `HubStatus { url, connected, peers }`, `hist_epoch`/`hist_rev:<key>` settings keys, and `pair-ok`/`pair-error {reqId, reason}` payloads are identical across all slices.
- Known simplifications, stated: web tests use real sockets (`node --test` + `ws` client); Rust loop internals are integration-tested through `run_session` only if pure-function tests prove insufficient — avoid over-mocking; `sha2` is an added dependency (integrity checks mandated by the audit), base64 stays hand-rolled (encoding, not crypto).
