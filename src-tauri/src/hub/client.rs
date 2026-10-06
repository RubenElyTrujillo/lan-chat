// Hub client: single-task, bounded WSS session loop.
// TLS is always validated (rustls + webpki roots); no bypass exists here.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rusqlite::Connection;
use tokio::sync::watch;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use super::backoff::ReconnectBackoff;
use super::inbox::{self, ChatOutcome, ClipboardOutcome, FileOutcome};
use super::pairing_wire::{PairingWire, WireReply};
use super::{HubShared, OutboundMsg};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound for pushing a queued write out of the send buffer: the auto-queued
/// Pong and unicast pairing replies alike must never hang the loop on a
/// dead or stalled peer.
const REPLY_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Client knobs. `Default` uses the LANCHAT_HUB_URL override (empty url when
/// unset — the wiring layer never starts this client without a configured
/// hub), 10 s connect/welcome bounds and entropy-jittered reconnect backoff.
/// Tests build it with short durations instead.
pub struct HubClientConfig {
    pub url: String,
    /// Display name announced in hello; the hub truncates it to 24 chars.
    pub name: String,
    /// Stable device identity, announced as `sid` in hello.
    pub device_id: String,
    pub connect_timeout: Duration,
    pub welcome_timeout: Duration,
    /// Injectable reconnect delay for tests. `None` uses the production
    /// bounded full-jitter backoff with an entropy fallback.
    pub reconnect_delay: Option<Arc<dyn Fn() -> Duration + Send + Sync>>,
}

impl Default for HubClientConfig {
    fn default() -> Self {
        Self {
            url: super::identity::resolve_hub_url(std::env::var("LANCHAT_HUB_URL").ok().as_deref())
                .unwrap_or_default(),
            name: "desktop".into(),
            device_id: String::new(),
            connect_timeout: CONNECT_TIMEOUT,
            welcome_timeout: WELCOME_TIMEOUT,
            reconnect_delay: None,
        }
    }
}

/// Graceful shutdown signal shared with the wiring layer.
pub fn shutdown_channel() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

enum AttemptOutcome {
    Shutdown,
    /// Attempt-level failure (connect/hello/welcome); the reason was already
    /// published through `HubShared`.
    Failed,
    /// The attempt HAD a live session that ended (hub closed or error):
    /// the next reconnect deserves a fresh backoff curve.
    SessionEnded,
}

/// Resolves once shutdown is requested. A dropped sender (no one left to
/// signal) is treated as shutdown so the loop can never outlive its owner.
async fn shutdown_signal(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow_and_update() {
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

/// Runs the hub session loop until shutdown: connect (bounded) -> hello ->
/// welcome (bounded, only a valid type+id counts) -> serve until closed ->
/// bounded-jitter reconnect. Status flows exclusively through `HubShared`.
pub async fn run_hub_client(
    shared: Arc<HubShared>,
    config: HubClientConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut backoff = ReconnectBackoff::new();
    loop {
        if *shutdown.borrow_and_update() {
            shared.set_disconnected("shutdown".into());
            return;
        }
        match run_attempt(&shared, &config, &mut shutdown).await {
            AttemptOutcome::Shutdown => return,
            AttemptOutcome::Failed => {}
            AttemptOutcome::SessionEnded => backoff.reset(),
        }
        let delay = match &config.reconnect_delay {
            Some(delay) => delay(),
            None => backoff.next_delay(),
        };
        tokio::select! {
            _ = sleep(delay) => {}
            _ = shutdown_signal(&mut shutdown) => {
                shared.set_disconnected("shutdown".into());
                return;
            }
        }
    }
}

/// Resolves at the soonest pending pairing expiry (never when idle): the
/// session loop selects on it so expiry clears local state without
/// busy-polling and without sending anything on the wire.
async fn pairing_deadline(pairing: &Mutex<PairingWire>) {
    let deadline = pairing.lock().ok().and_then(|p| p.next_deadline());
    match deadline {
        Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
        None => std::future::pending::<()>().await,
    }
}

async fn run_attempt(
    shared: &HubShared,
    config: &HubClientConfig,
    shutdown: &mut watch::Receiver<bool>,
) -> AttemptOutcome {
    shared.set_connecting();

    // Bounded connect, cancellable by shutdown.
    let mut ws = tokio::select! {
        res = timeout(config.connect_timeout, connect_async(config.url.as_str())) => match res {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => {
                let reason = format!("connect failed: {e}");
                shared.set_disconnected(reason);
                return AttemptOutcome::Failed;
            }
            Err(_) => {
                let reason = "connect timeout".to_string();
                shared.set_disconnected(reason);
                return AttemptOutcome::Failed;
            }
        },
        _ = shutdown_signal(shutdown) => {
            shared.set_disconnected("shutdown".into());
            return AttemptOutcome::Shutdown;
        }
    };

    // Announce the device: type + display name + kind + stable sid + caps.
    // The hub keys peers on name/kind; `sid` carries the stable identity.
    let hello = serde_json::json!({
        "type": "hello",
        "name": config.name,
        "kind": "app",
        "sid": config.device_id,
        "caps": [],
    });
    if let Err(e) = ws.send(Message::text(hello.to_string())).await {
        let reason = format!("hello failed: {e}");
        shared.set_disconnected(reason);
        return AttemptOutcome::Failed;
    }

    // Bounded welcome: only a valid type+id counts.
    let session_id = match timeout(config.welcome_timeout, read_welcome(&mut ws)).await {
        Ok(Some(session_id)) => session_id,
        Ok(None) => {
            let reason = "connection closed before welcome".to_string();
            shared.set_disconnected(reason);
            return AttemptOutcome::Failed;
        }
        Err(_) => {
            let reason = "welcome timeout".to_string();
            shared.set_disconnected(reason);
            return AttemptOutcome::Failed;
        }
    };
    shared.set_connected(session_id.clone());

    // Outbound relay queue for THIS session: registered right after the
    // valid welcome, replacing any sender from a previous session. A stale
    // queue's pending acks resolve as errors when its receiver drops.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<OutboundMsg>();
    if let Ok(mut outbound) = shared.outbound.lock() {
        *outbound = Some(out_tx);
    }

    // Serve until the hub closes or shutdown. Presence snapshots are the only
    // application frames processed; every other text frame (relay/chat) and
    // any malformed JSON is ignored, preserving the last good snapshot.
    loop {
        let msg = tokio::select! {
            frame = ws.next() => match frame {
                Some(Ok(msg)) => msg,
                Some(Err(e)) => {
                    let reason = format!("connection error: {e}");
                    shared.set_disconnected(reason);
                    return AttemptOutcome::SessionEnded;
                }
                None => {
                    let reason = "connection closed".to_string();
                    shared.set_disconnected(reason);
                    return AttemptOutcome::SessionEnded;
                }
            },
            out = out_rx.recv() => match out {
                Some(outbound) => {
                    // Bounded sink write, then the ack resolves OUTSIDE any
                    // lock: nothing here is ever held across the await.
                    let res = send_outbound_frame(&mut ws, &outbound.text).await;
                    let _ = outbound.ack.send(res);
                    continue;
                }
                None => {
                    // All senders dropped: the session is being torn down.
                    // End the attempt instead of busy-spinning on a dead
                    // queue.
                    let _ = ws.close(None).await;
                    shared.set_disconnected("outbound queue closed".into());
                    return AttemptOutcome::SessionEnded;
                }
            },
            _ = pairing_deadline(&shared.pairing) => {
                // Local-only expiry sweep: refreshes the pairing snapshot.
                if let Ok(mut pairing) = shared.pairing.lock() {
                    pairing.expire_tick();
                }
                continue;
            }
            _ = shutdown_signal(shutdown) => {
                let _ = ws.close(None).await;
                shared.set_disconnected("shutdown".into());
                return AttemptOutcome::Shutdown;
            }
        };
        match msg {
            Message::Text(text) => {
                // `self_conn_id` is our own hub connection id (the welcome
                // id): parse_peers filters it out of the snapshot. Non-peers
                // payloads parse to None and are dropped — nothing is
                // processed or authorized on their behalf.
                let parsed = if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.as_str())
                {
                    match super::presence::parse_peers(&v, Some(&session_id)) {
                        Some(snapshot) => {
                            shared.set_presence(snapshot);
                            None
                        }
                        None if v.get("type").and_then(|t| t.as_str()) == Some("relay") => {
                            if v.get("payload")
                                .and_then(|p| p.get("type"))
                                .and_then(|t| t.as_str())
                                == Some("chat")
                            {
                                // Inbound chat: authorized + persisted +
                                // emitted through the startup seams. Never
                                // draws a reply; fully synchronous.
                                handle_chat_relay(shared, &v);
                                None
                            } else if v
                                .get("payload")
                                .and_then(|p| p.get("type"))
                                .and_then(|t| t.as_str())
                                == Some("clipboard")
                            {
                                // Inbound clipboard share: same
                                // authorization + persist path as chat but
                                // its own event (`hub-clipboard-received`
                                // via the clipboard hook, never the chat
                                // one). Synchronous, never a reply.
                                handle_clipboard_relay(shared, &v);
                                None
                            } else if v
                                .get("payload")
                                .and_then(|p| p.get("type"))
                                .and_then(|t| t.as_str())
                                == Some("file")
                            {
                                // Inbound file: authorized + persisted +
                                // finalized + emitted through the startup
                                // seams. Same contract as chat: synchronous,
                                // never a reply.
                                handle_file_relay(shared, &v);
                                None
                            } else {
                                // Pairing relay frame: handled strictly by
                                // envelope; the reply is unicast to one conn.
                                shared
                                    .pairing
                                    .lock()
                                    .ok()
                                    .and_then(|mut pairing| pairing.handle_relay(&v))
                            }
                        }
                        None => None,
                    }
                } else {
                    None
                };
                if let Some(reply) = parsed {
                    match write_reply(&mut ws, &reply).await {
                        Ok(()) => {
                            // Commit-after-send: pairing authorization only
                            // becomes visible once the reply reached the
                            // sink. Short lock, never held across the await.
                            if let Ok(mut pairing) = shared.pairing.lock() {
                                pairing.commit_relayed(&reply, &session_id);
                            }
                        }
                        Err(reason) => {
                            // Nothing was delivered: drop any provisional
                            // pairing state so the snapshot stays not-paired,
                            // then fail the attempt so the session clears —
                            // no half-closed loop, no authorization without a
                            // delivered pair-ok. Reasons stay detail-free.
                            if let Ok(mut pairing) = shared.pairing.lock() {
                                pairing.rollback_relayed(&reply);
                            }
                            shared.set_disconnected(reason);
                            return AttemptOutcome::Failed;
                        }
                    }
                }
            }
            Message::Ping(_) => {
                // tungstenite queued the Pong when the Ping was read; flush
                // now so it is sent promptly instead of waiting for the next
                // I/O (never silently defer the Pong).
                match timeout(REPLY_FLUSH_TIMEOUT, ws.flush()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        let reason = format!("pong flush failed: {e}");
                        shared.set_disconnected(reason);
                        return AttemptOutcome::SessionEnded;
                    }
                    Err(_) => {
                        let reason = "pong flush timeout".to_string();
                        shared.set_disconnected(reason);
                        return AttemptOutcome::SessionEnded;
                    }
                }
            }
            Message::Close(_) => {
                let _ = ws.close(None).await;
                let reason = "connection closed".to_string();
                shared.set_disconnected(reason);
                return AttemptOutcome::SessionEnded;
            }
            _ => {}
        }
    }
}

/// Current unix time in milliseconds (0 if the clock is before the epoch).
fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Inbound chat relay: resolves the authorization context from the CURRENT
/// shared state and runs the synchronous inbox pipeline (authorize ->
/// persist -> emit) inside the caller-provided DB seam. Fully synchronous:
/// no lock is ever held across an await and nothing here awaits. A frame
/// that fails any gate is silently skipped (debug log only); a duplicate is
/// also a no-op (the first copy already has its row and event).
fn handle_chat_relay(shared: &HubShared, frame: &serde_json::Value) {
    let Some(from_id) = frame.get("from_id").and_then(serde_json::Value::as_str) else {
        eprintln!("Hub: chat sin from_id descartado");
        return;
    };
    let from_sid = frame.get("from_sid").and_then(serde_json::Value::as_str);
    let Some(payload) = frame.get("payload") else {
        eprintln!("Hub: chat sin payload descartado");
        return;
    };
    // Authorization inputs are snapshotted under short locks BEFORE the DB
    // seam runs; no hub lock is held while the DB is locked.
    let Some(peer) = shared
        .presence()
        .peers
        .into_iter()
        .find(|p| p.conn_id == from_id)
    else {
        eprintln!("Hub: chat de conn desconocida descartado");
        return;
    };
    let Some(session_id) = shared.status().session_id else {
        eprintln!("Hub: chat sin sesión viva descartado");
        return;
    };
    let authorized = shared.conn_authorized(from_id);
    let contact_key = shared.contact_for_conn(from_id);
    let ctx = inbox::ChatContext {
        session_id,
        peer: Some(peer),
        authorized,
        contact_key,
    };
    let Some(hooks) = shared.hooks() else {
        eprintln!("Hub: chat descartado (seams no instalados)");
        return;
    };
    let chat_emit = hooks.chat_emit.clone();
    let mint = || super::identity::new_uuid();
    let mut outcome = None;
    let mut ctx = Some(ctx);
    (hooks.with_db)(&mut |conn: &mut Connection| {
        // Runs strictly after `accept_text` committed: the hook can trust
        // the row is visible (persist-before-emit is the inbox's contract).
        let emit = |msg: &inbox::ReceivedChat, _conn: &Connection| chat_emit(msg);
        outcome = Some(inbox::handle_chat_frame(
            from_sid,
            payload,
            ctx.take().expect("chat context is consumed exactly once"),
            &mint,
            unix_millis(),
            conn,
            &emit,
        ));
    });
    match outcome {
        Some(ChatOutcome::Accepted(msg)) => {
            eprintln!("Hub: mensaje entrante de {} aceptado", msg.name);
        }
        Some(ChatOutcome::Duplicate) => eprintln!("Hub: chat duplicado descartado"),
        Some(ChatOutcome::Conflict) => eprintln!("Hub: conflicto de id en chat entrante"),
        Some(ChatOutcome::Dropped(reason)) => {
            eprintln!("Hub: chat descartado ({reason})");
        }
        None => eprintln!("Hub: chat no procesado (sin acceso a la base)"),
    }
}

/// Inbound clipboard relay: the clipboard twin of `handle_chat_relay`.
/// Same short-lock authorization context and the SAME synchronous DB
/// persist path (a normal text row via `accept_text`); only the payload
/// bound (64_000 chars) and the emit hook differ — the clipboard hook
/// fires `hub-clipboard-received` and the chat hook is never called.
/// Fully synchronous; failures are log-only; duplicates are silent.
fn handle_clipboard_relay(shared: &HubShared, frame: &serde_json::Value) {
    let Some(from_id) = frame.get("from_id").and_then(serde_json::Value::as_str) else {
        eprintln!("Hub: clipboard sin from_id descartado");
        return;
    };
    let from_sid = frame.get("from_sid").and_then(serde_json::Value::as_str);
    let Some(payload) = frame.get("payload") else {
        eprintln!("Hub: clipboard sin payload descartado");
        return;
    };
    // Authorization inputs are snapshotted under short locks BEFORE the DB
    // seam runs; no hub lock is held while the DB is locked.
    let Some(peer) = shared
        .presence()
        .peers
        .into_iter()
        .find(|p| p.conn_id == from_id)
    else {
        eprintln!("Hub: clipboard de conn desconocida descartado");
        return;
    };
    let Some(session_id) = shared.status().session_id else {
        eprintln!("Hub: clipboard sin sesión viva descartado");
        return;
    };
    let authorized = shared.conn_authorized(from_id);
    let contact_key = shared.contact_for_conn(from_id);
    let ctx = inbox::ChatContext {
        session_id,
        peer: Some(peer),
        authorized,
        contact_key,
    };
    let Some(hooks) = shared.hooks() else {
        eprintln!("Hub: clipboard descartado (seams no instalados)");
        return;
    };
    let clipboard_emit = hooks.clipboard_emit.clone();
    let mint = || super::identity::new_uuid();
    let mut outcome = None;
    let mut ctx = Some(ctx);
    (hooks.with_db)(&mut |conn: &mut Connection| {
        // Runs strictly after `accept_text` committed: the hook can trust
        // the row is visible (persist-before-emit is the inbox's contract).
        let emit = |msg: &inbox::ReceivedClipboard, _conn: &Connection| clipboard_emit(msg);
        outcome = Some(inbox::handle_clipboard_frame(
            from_sid,
            payload,
            ctx.take()
                .expect("clipboard context is consumed exactly once"),
            &mint,
            unix_millis(),
            conn,
            &emit,
        ));
    });
    match outcome {
        Some(ClipboardOutcome::Accepted(msg)) => {
            eprintln!("Hub: clipboard entrante de {} aceptado", msg.name);
        }
        Some(ClipboardOutcome::Duplicate) => eprintln!("Hub: clipboard duplicado descartado"),
        Some(ClipboardOutcome::Conflict) => {
            eprintln!("Hub: conflicto de id en clipboard entrante")
        }
        Some(ClipboardOutcome::Dropped(reason)) => {
            eprintln!("Hub: clipboard descartado ({reason})");
        }
        None => eprintln!("Hub: clipboard no procesado (sin acceso a la base)"),
    }
}

/// Inbound file relay: the file twin of `handle_chat_relay`. Same
/// short-lock context snapshot, same synchronous DB seam; the download dir
/// resolves through its own startup seam. Fully synchronous: no lock is
/// ever held across an await and nothing here awaits. A frame that fails
/// any gate is silently skipped (log only); a duplicate is a no-op (the
/// first copy already has its row, file and event).
fn handle_file_relay(shared: &HubShared, frame: &serde_json::Value) {
    let Some(from_id) = frame.get("from_id").and_then(serde_json::Value::as_str) else {
        eprintln!("Hub: file sin from_id descartado");
        return;
    };
    let from_sid = frame.get("from_sid").and_then(serde_json::Value::as_str);
    let Some(payload) = frame.get("payload") else {
        eprintln!("Hub: file sin payload descartado");
        return;
    };
    // Authorization inputs are snapshotted under short locks BEFORE the DB
    // seam runs; no hub lock is held while the DB is locked.
    let Some(peer) = shared
        .presence()
        .peers
        .into_iter()
        .find(|p| p.conn_id == from_id)
    else {
        eprintln!("Hub: file de conn desconocida descartado");
        return;
    };
    let Some(session_id) = shared.status().session_id else {
        eprintln!("Hub: file sin sesión viva descartado");
        return;
    };
    let authorized = shared.conn_authorized(from_id);
    let contact_key = shared.contact_for_conn(from_id);
    let ctx = inbox::ChatContext {
        session_id,
        peer: Some(peer),
        authorized,
        contact_key,
    };
    let Some(hooks) = shared.hooks() else {
        eprintln!("Hub: file descartado (seams no instalados)");
        return;
    };
    let file_emit = hooks.file_received.clone();
    let file_error = hooks.file_error.clone();
    let download_dir = (hooks.download_dir)();
    let mint = || super::identity::new_uuid();
    let mut outcome = None;
    let mut ctx = Some(ctx);
    (hooks.with_db)(&mut |conn: &mut Connection| {
        // Runs strictly after the row commit AND the finalize rename: the
        // hook can trust both (persist+finalize-before-emit is the inbox's
        // contract).
        let emit = |f: &inbox::ReceivedFile, _conn: &Connection| file_emit(f);
        let on_err = |e: &inbox::FileError| file_error(e);
        outcome = Some(inbox::handle_file_frame(
            from_sid,
            payload,
            ctx.take().expect("file context is consumed exactly once"),
            std::path::Path::new(&download_dir),
            &mint,
            unix_millis(),
            conn,
            &emit,
            &on_err,
        ));
    });
    match outcome {
        Some(FileOutcome::Accepted(f)) => {
            eprintln!("Hub: archivo entrante de {} aceptado", f.name);
        }
        Some(FileOutcome::Duplicate) => eprintln!("Hub: file duplicado descartado"),
        Some(FileOutcome::Conflict) => eprintln!("Hub: conflicto de id en file entrante"),
        Some(FileOutcome::TooLarge { name }) => {
            eprintln!("Hub: file demasiado grande descartado ({name})")
        }
        Some(FileOutcome::Dropped(reason)) => eprintln!("Hub: file descartado ({reason})"),
        None => eprintln!("Hub: file no procesado (sin acceso a la base)"),
    }
}

/// Writes one queued outbound frame to the sink with the same bounded flush
/// as pairing replies. Failure reasons are detail-free: ack consumers only
/// ever map them to a generic delivery failure.
async fn send_outbound_frame(ws: &mut WsStream, text: &str) -> Result<(), String> {
    match timeout(REPLY_FLUSH_TIMEOUT, ws.send(Message::text(text))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("outbound write failed".to_string()),
        Err(_) => Err("outbound write timeout".to_string()),
    }
}

/// Writes one relay reply to the sink with a bounded flush. Failure reasons
/// never include reply contents: pairing codes never surface in status or
/// logs.
async fn write_reply(ws: &mut WsStream, reply: &WireReply) -> Result<(), String> {
    let text = serde_json::to_string(reply)
        .map_err(|_| "pairing reply serialization failed".to_string())?;
    match timeout(REPLY_FLUSH_TIMEOUT, ws.send(Message::text(text))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("pairing reply write failed: {e}")),
        Err(_) => Err("pairing reply write timeout".to_string()),
    }
}

/// Upper bound for a valid welcome session id.
const MAX_WELCOME_ID_LEN: usize = 128;

/// Only `{"type":"welcome","id":"<non-empty, <=128 chars>"}` is a valid
/// welcome. Everything else is ignored; the bounded welcome timeout decides.
fn parse_welcome(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("type")?.as_str()? != "welcome" {
        return None;
    }
    let id = v.get("id")?.as_str()?;
    let id = id.trim();
    (!id.is_empty() && id.len() <= MAX_WELCOME_ID_LEN).then(|| id.to_string())
}

async fn read_welcome(ws: &mut WsStream) -> Option<String> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                if let Some(id) = parse_welcome(text.as_str()) {
                    return Some(id);
                }
            }
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(_)) => {} // ping/pong/binary frames during handshake are ignored
            Some(Err(_)) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::{HubEvent, HubListener, HubPhase, OutboundMsg};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    type ServerWs = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    const SOON: Duration = Duration::from_millis(60);
    /// Upper bound for any await that could hang if the client stalls or
    /// panics: a clear failure beats a hung test suite.
    const TEST_BOUND: Duration = Duration::from_secs(2);

    async fn spawn_ws_server() -> (String, mpsc::Receiver<ServerWs>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                    if tx.send(ws).await.is_err() {
                        break;
                    }
                } else {
                    break;
                }
            }
        });
        (url, rx)
    }

    /// Accepts TCP but never completes the WS upgrade: connect stalls until
    /// the bounded timeout or shutdown.
    async fn spawn_black_hole() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        url
    }

    /// A closed port: connect fails immediately.
    async fn dead_port_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("ws://{addr}")
    }

    fn config(url: String, reconnect_delay: Duration) -> HubClientConfig {
        HubClientConfig {
            url,
            name: "tester".into(),
            device_id: "test-device".into(),
            connect_timeout: Duration::from_secs(2),
            welcome_timeout: SOON,
            reconnect_delay: Some(Arc::new(move || reconnect_delay)),
        }
    }

    /// Bounded `conns.recv()`: fails loudly instead of hanging on a stall.
    async fn accept_conn(conns: &mut mpsc::Receiver<ServerWs>) -> ServerWs {
        tokio::time::timeout(TEST_BOUND, conns.recv())
            .await
            .expect("timed out waiting for the client to connect")
            .expect("hub server task ended unexpectedly")
    }

    /// Bounded frame read: a clear panic instead of a hung test.
    async fn next_msg(ws: &mut ServerWs) -> Message {
        tokio::time::timeout(TEST_BOUND, ws.next())
            .await
            .expect("timed out waiting for a frame from the client")
            .expect("client stream ended unexpectedly")
            .expect("client stream error")
    }

    /// Reads the next relay reply frame the client unicasts to one conn.
    async fn read_relay(ws: &mut ServerWs) -> serde_json::Value {
        loop {
            let msg = next_msg(ws).await;
            let Message::Text(t) = msg else { continue };
            let v: serde_json::Value =
                serde_json::from_str(t.as_str()).expect("relay frame is JSON");
            assert_eq!(v["type"], "relay", "unicast envelope expected: {v}");
            return v;
        }
    }

    /// Fake hub: validates the FULL hello contract before proceeding —
    /// never a blind welcome. The desktop client must announce type, name,
    /// kind=app, its stable sid and its capability list.
    async fn require_hello(ws: &mut ServerWs) {
        let msg = next_msg(ws).await;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            other => panic!("expected hello text frame, got {other:?}"),
        };
        let v: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("hello is not JSON: {e}: {text}"));
        assert_eq!(v["type"], "hello", "hello must announce type=hello: {text}");
        assert_eq!(
            v["name"], "tester",
            "hello must announce the config name: {text}"
        );
        assert_eq!(v["kind"], "app", "desktop hello must be kind=app: {text}");
        assert_eq!(
            v["sid"], "test-device",
            "hello must carry the stable sid: {text}"
        );
        assert_eq!(
            v["caps"],
            serde_json::json!([]),
            "hello must carry caps: {text}"
        );
        assert!(
            v.get("device_id").is_none(),
            "hello must not carry a standalone device_id: {text}"
        );
    }

    fn event_channel() -> (HubListener, mpsc::Receiver<HubEvent>) {
        let (tx, rx) = mpsc::channel(16);
        let listener: HubListener = Arc::new(move |e| {
            let _ = tx.try_send(e.clone());
        });
        (listener, rx)
    }

    async fn wait_for_phase(shared: &HubShared, phase: HubPhase) {
        for _ in 0..200 {
            if shared.status().phase == phase {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let status_json =
            serde_json::to_string(&shared.status()).unwrap_or_else(|_| "unreadable".into());
        panic!("phase {phase:?} not reached; status was {status_json}");
    }

    #[tokio::test]
    async fn hello_welcome_sets_status_ping_pong_then_close_emits_disconnect() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws: ServerWs = accept_conn(&mut conns).await;
        require_hello(&mut ws).await;

        ws.send(Message::text(r#"{"type":"welcome","id":"sess-1"}"#))
            .await
            .unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ev,
            HubEvent::Connected {
                session_id: "sess-1".into()
            }
        );
        let st = shared.status();
        assert!(st.connected);
        assert_eq!(st.phase, HubPhase::Connected);
        assert_eq!(st.session_id.as_deref(), Some("sess-1"));

        // Unknown application frames are ignored; a Ping is answered with
        // exactly one timely Pong echoing the payload (tungstenite auto-pong).
        ws.send(Message::text(r#"{"type":"chat-caps","max_files":10}"#))
            .await
            .unwrap();
        ws.send(Message::Ping("ka".into())).await.unwrap();
        loop {
            match next_msg(&mut ws).await {
                Message::Pong(p) => {
                    assert_eq!(p.as_ref(), b"ka");
                    break;
                }
                Message::Text(_) => {} // chat-caps echo would be a bug elsewhere
                _ => {}
            }
        }
        // No duplicate Pong may follow the queued one.
        if let Ok(Some(Ok(Message::Pong(p)))) = timeout(Duration::from_millis(200), ws.next()).await
        {
            panic!("duplicate Pong after auto-pong flush: {p:?}");
        }

        ws.close(None).await.unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            HubEvent::Disconnected { reason } => {
                assert!(reason.contains("closed"), "reason was: {reason}")
            }
            other => panic!("expected Disconnected, got {other:?}"),
        }
        let st = shared.status();
        assert!(!st.connected);
        assert_eq!(st.phase, HubPhase::Backoff);
        assert!(st.session_id.is_none());

        sx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn malformed_welcome_and_missing_id_never_connect_and_time_out() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws: ServerWs = accept_conn(&mut conns).await;
        require_hello(&mut ws).await;
        // Malformed, missing, empty, mistyped and OVERSIZED ids are all
        // rejected: only {"type":"welcome","id":"<1..128 chars>"} connects.
        ws.send(Message::text("this is not json")).await.unwrap();
        ws.send(Message::text(r#"{"type":"welcome"}"#))
            .await
            .unwrap(); // no id
        ws.send(Message::text(r#"{"type":"welcome","id":"   "}"#))
            .await
            .unwrap(); // blank id
        ws.send(Message::text(r#"{"type":"welcome","id":123}"#))
            .await
            .unwrap(); // mistyped id
        let oversized = format!(r#"{{"type":"welcome","id":"{}"}}"#, "x".repeat(129));
        ws.send(Message::text(oversized)).await.unwrap();

        // Nothing may arrive for a while: no Connected, no Disconnected
        // (never was connected), while the bounded welcome timeout runs.
        let leaked = tokio::time::timeout(SOON + Duration::from_millis(40), events.recv()).await;
        assert!(leaked.is_err(), "unexpected event: {:?}", leaked);

        let st = shared.status();
        assert!(
            !st.connected,
            "invalid welcomes must never set up a session"
        );
        assert!(st.session_id.is_none());
        assert!(st.reason.as_deref().unwrap_or_default().contains("welcome"));

        sx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_cancels_a_stalled_connect() {
        let shared = Arc::new(HubShared::new("wss://unused".into(), None));
        let url = spawn_black_hole().await;
        let (sx, srx) = shutdown_channel();
        // 10 s connect bound: only shutdown can end this promptly.
        let cfg = HubClientConfig {
            connect_timeout: Duration::from_secs(10),
            ..config(url, Duration::from_secs(60))
        };
        let task = tokio::spawn(run_hub_client(shared.clone(), cfg, srx));

        wait_for_phase(&shared, HubPhase::Connecting).await;
        sx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        let st = shared.status();
        assert!(!st.connected);
        assert_eq!(st.phase, HubPhase::Backoff);
        assert_eq!(st.reason.as_deref(), Some("shutdown"));
    }

    #[tokio::test]
    async fn shutdown_cancels_backoff_sleep() {
        let shared = Arc::new(HubShared::new("wss://unused".into(), None));
        let url = dead_port_url().await;
        // 60 s injected backoff: only shutdown can end this promptly.
        let cfg = config(url, Duration::from_secs(60));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(shared.clone(), cfg, srx));

        wait_for_phase(&shared, HubPhase::Backoff).await;
        sx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        let st = shared.status();
        assert!(!st.connected);
        assert_eq!(st.reason.as_deref(), Some("shutdown"));
    }

    /// Full hello + welcome handshake, consuming the Connected event, so the
    /// session is live and presence frames can be exercised.
    async fn connect_session(
        conns: &mut mpsc::Receiver<ServerWs>,
        events: &mut mpsc::Receiver<HubEvent>,
        session_id: &str,
    ) -> ServerWs {
        let mut ws: ServerWs = accept_conn(conns).await;
        require_hello(&mut ws).await;
        ws.send(Message::text(format!(
            r#"{{"type":"welcome","id":"{session_id}"}}"#
        )))
        .await
        .unwrap();
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ev,
            HubEvent::Connected {
                session_id: session_id.into()
            },
            "session must be up before presence frames matter"
        );
        ws
    }

    #[tokio::test]
    async fn peers_frame_after_welcome_updates_presence_and_excludes_self() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;

        // The hub rebroadcasts the whole peer list; our own conn id (the
        // welcome id) must come back filtered out of the snapshot.
        let peers = serde_json::json!({
            "type": "peers",
            "list": [
                { "id": "self-1", "name": "Yo", "kind": "web" },
                { "id": "w2", "name": "Ana", "kind": "web", "sid": "sid-ana", "caps": ["chat-v1"] },
            ]
        });
        ws.send(Message::text(peers.to_string())).await.unwrap();

        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            HubEvent::PresenceChanged { snapshot } => {
                assert_eq!(
                    snapshot.peers.len(),
                    1,
                    "self must be excluded: {snapshot:?}"
                );
                assert_eq!(snapshot.peers[0].conn_id, "w2");
                assert_eq!(snapshot.peers[0].name, "Ana");
                assert_eq!(snapshot.peers[0].sid.as_deref(), Some("sid-ana"));
            }
            other => panic!("expected PresenceChanged, got {other:?}"),
        }
        let stored = shared.presence();
        assert_eq!(stored.peers.len(), 1, "Shared must store the snapshot");
        assert_eq!(stored.peers[0].conn_id, "w2");

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn app_kind_peers_are_filtered_web_only() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;

        let peers = serde_json::json!({
            "type": "peers",
            "list": [
                { "id": "w1", "name": "Web", "kind": "web" },
                { "id": "a1", "name": "App", "kind": "app" },
                { "id": "r1", "name": "Relay", "kind": "relay" },
                { "id": "n1", "name": "None" },
            ]
        });
        ws.send(Message::text(peers.to_string())).await.unwrap();

        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            HubEvent::PresenceChanged { snapshot } => {
                assert_eq!(
                    snapshot.peers.len(),
                    1,
                    "only web peers surface: {snapshot:?}"
                );
                assert_eq!(snapshot.peers[0].conn_id, "w1");
                assert_eq!(snapshot.peers[0].kind, "web");
            }
            other => panic!("expected PresenceChanged, got {other:?}"),
        }
        assert_eq!(shared.presence().peers.len(), 1);

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn malformed_peers_entries_are_skipped_and_garbage_frames_preserve_state() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;

        // One bad row never kills the snapshot; valid entries survive.
        let mixed = serde_json::json!({
            "type": "peers",
            "list": [
                42,
                { "id": "", "kind": "web" },
                { "id": "x".repeat(65), "kind": "web" },
                { "id": "w-ok", "name": "Ok", "kind": "web" },
            ]
        });
        ws.send(Message::text(mixed.to_string())).await.unwrap();
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            HubEvent::PresenceChanged { snapshot } => {
                assert_eq!(
                    snapshot.peers.len(),
                    1,
                    "only the valid entry: {snapshot:?}"
                );
                assert_eq!(snapshot.peers[0].conn_id, "w-ok");
            }
            other => panic!("expected PresenceChanged, got {other:?}"),
        }

        // Non-JSON text, non-peers application frames (relay/chat) and
        // peers-shaped garbage are all ignored: no new event, good state
        // preserved (never cleared by garbage).
        ws.send(Message::text("this is not json")).await.unwrap();
        ws.send(Message::text(r#"{"type":"relay","payload":{"x":1}}"#))
            .await
            .unwrap();
        ws.send(Message::text(r#"{"type":"peers"}"#)).await.unwrap();
        let leaked = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
        assert!(
            leaked.is_err(),
            "garbage frames must not emit: {:?}",
            leaked
        );
        assert_eq!(
            shared.presence().peers.len(),
            1,
            "garbage must preserve the good snapshot"
        );

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn disconnect_clears_presence_and_emits_empty_snapshot() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        let peers = serde_json::json!({
            "type": "peers",
            "list": [{ "id": "w1", "name": "Web", "kind": "web" }]
        });
        ws.send(Message::text(peers.to_string())).await.unwrap();
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ev, HubEvent::PresenceChanged { .. }));

        // Session down: peers of a dead session are not peers — one empty
        // clear snapshot after the Disconnected event.
        ws.close(None).await.unwrap();
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ev, HubEvent::Disconnected { .. }), "got {ev:?}");
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            HubEvent::PresenceChanged { snapshot } => {
                assert!(
                    snapshot.is_empty(),
                    "clear event must be empty: {snapshot:?}"
                );
            }
            other => panic!("expected empty PresenceChanged, got {other:?}"),
        }
        assert!(
            shared.presence().is_empty(),
            "Shared presence must be cleared"
        );

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    /// Slice 3b wire regression: a pair-request NEVER completes pairing —
    /// bounded silence on the wire while the code shows up only in the local
    /// pairing snapshot. Only a correct pair-verify yields the single
    /// unicast `pair-ok`, and the grant commits after that write.
    #[tokio::test]
    async fn relay_pair_request_stays_silent_until_a_correct_verify() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        let peers = serde_json::json!({
            "type": "peers",
            "list": [{ "id": "w1", "name": "Ana", "kind": "web", "sid": "sid-ana" }]
        });
        ws.send(Message::text(peers.to_string())).await.unwrap();
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(ev, HubEvent::PresenceChanged { .. }),
            "peer must be live first"
        );

        let frame = serde_json::json!({
            "type": "relay", "from_id": "w1", "from_name": "Ana", "from_sid": "sid-ana",
            "payload": { "type": "pair-request", "reqId": "r-1" }
        });
        ws.send(Message::text(frame.to_string())).await.unwrap();

        // Bounded silence: no incoming frame at all within 100ms — in
        // particular no pair-ok. A request never completes pairing.
        let silence = tokio::time::timeout(Duration::from_millis(100), ws.next()).await;
        assert!(
            silence.is_err(),
            "a pair-request must not draw any reply, got {silence:?}"
        );

        let snap = shared.pairing.lock().unwrap().snapshot();
        assert_eq!(snap.pending.len(), 1);
        assert_eq!(snap.pending[0].conn_id, "w1");
        assert_eq!(snap.pending[0].req_id, "r-1");
        assert_eq!(snap.pending[0].name, "Ana");
        assert_eq!(snap.pending[0].code.len(), 8);
        assert!(snap.paired.is_empty(), "a request never authorizes");
        let code = snap.pending[0].code.clone();

        // Only the correct verify pairs: one unicast pair-ok, code never
        // travels the wire.
        ws.send(Message::text(
            serde_json::json!({
                "type": "relay", "from_id": "w1", "from_name": "Ana", "from_sid": "sid-ana",
                "payload": { "type": "pair-verify", "reqId": "r-1", "code": code }
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let ok = read_relay(&mut ws).await;
        assert_eq!(ok["to"], "w1", "unicast to the requester only");
        assert_eq!(ok["payload"]["type"], "pair-ok");
        assert_eq!(ok["payload"]["reqId"], "r-1");
        assert!(
            ok["payload"].get("code").is_none(),
            "code never travels the wire"
        );

        // The pair-ok write succeeded: the grant commits afterwards.
        let mut paired = false;
        for _ in 0..200 {
            if !shared.pairing.lock().unwrap().snapshot().paired.is_empty() {
                paired = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(paired, "grant must commit after the pair-ok write");
        let snap = shared.pairing.lock().unwrap().snapshot();
        assert!(snap.pending.is_empty());
        assert_eq!(snap.paired[0].conn_id, "w1");
        assert_eq!(snap.paired[0].req_id, "r-1");

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn pair_verify_replies_ok_over_the_wire_then_pairs_after_the_write() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        let peers = serde_json::json!({
            "type": "peers",
            "list": [{ "id": "w1", "name": "Ana", "kind": "web", "sid": "sid-ana" }]
        });
        ws.send(Message::text(peers.to_string())).await.unwrap();
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(ev, HubEvent::PresenceChanged { .. }),
            "peer must be live first"
        );

        // pair-request: NO reply — pairing never completes on request; the
        // code shows up only in the local pairing snapshot.
        ws.send(Message::text(
            serde_json::json!({
                "type": "relay", "from_id": "w1", "from_name": "Ana", "from_sid": "sid-ana",
                "payload": { "type": "pair-request", "reqId": "r-1" }
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let silence = tokio::time::timeout(Duration::from_millis(100), ws.next()).await;
        assert!(
            silence.is_err(),
            "a pair-request must not draw any reply, got {silence:?}"
        );
        let code = shared.pairing.lock().unwrap().snapshot().pending[0]
            .code
            .clone();

        // pair-verify with the right code: the client must WRITE pair-ok to
        // the sink and only then surface paired state (commit-after-send).
        ws.send(Message::text(
            serde_json::json!({
                "type": "relay", "from_id": "w1", "from_name": "Ana", "from_sid": "sid-ana",
                "payload": { "type": "pair-verify", "reqId": "r-1", "code": code }
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let verify_ok = read_relay(&mut ws).await;
        assert_eq!(verify_ok["to"], "w1");
        assert_eq!(verify_ok["payload"]["type"], "pair-ok");
        assert_eq!(verify_ok["payload"]["reqId"], "r-1");
        assert!(
            verify_ok["payload"].get("code").is_none(),
            "code never travels"
        );

        // The pair-ok reached the sink: the paired snapshot must follow.
        let mut paired = false;
        for _ in 0..200 {
            if !shared.pairing.lock().unwrap().snapshot().paired.is_empty() {
                paired = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(paired, "paired state must commit after the pair-ok write");
        let snap = shared.pairing.lock().unwrap().snapshot();
        assert!(snap.pending.is_empty());
        assert_eq!(snap.paired[0].conn_id, "w1");
        assert_eq!(snap.paired[0].req_id, "r-1");

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn reconnects_with_fresh_session_after_close() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(1)),
            srx,
        ));

        let mut ws: ServerWs = accept_conn(&mut conns).await;
        require_hello(&mut ws).await;
        ws.send(Message::text(r#"{"type":"welcome","id":"s1"}"#))
            .await
            .unwrap();
        ws.close(None).await.unwrap();

        let ev = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ev,
            HubEvent::Connected {
                session_id: "s1".into()
            }
        );
        let ev = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ev, HubEvent::Disconnected { .. }));

        let mut ws: ServerWs = accept_conn(&mut conns).await;
        require_hello(&mut ws).await; // full hello contract holds on reconnect too
        ws.send(Message::text(r#"{"type":"welcome","id":"s2"}"#))
            .await
            .unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ev,
            HubEvent::Connected {
                session_id: "s2".into()
            }
        );
        assert_eq!(shared.status().session_id.as_deref(), Some("s2"));

        sx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    // ---- Inbound chat dispatch (Slice 4b: client wiring) ----

    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::history;
    use crate::hub::inbox::{FileError, ReceivedChat, ReceivedClipboard, ReceivedFile};
    use crate::hub::HubHooks;

    /// Real SQLite file in the OS temp dir (never in-memory), unique per call.
    fn temp_chat_db(label: &str) -> std::path::PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "lan-chat-client-chat-{label}-{}-{n}.db",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let conn = rusqlite::Connection::open(&path).unwrap();
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
        history::ensure_history_schema(&conn).unwrap();
        drop(conn);
        path
    }

    /// Installed startup seams over a real file DB. `emits` records every
    /// accepted message TOGETHER with the row count a second connection saw
    /// at emit time — the persist-before-emit proof at integration level.
    /// File hooks record every accepted file TOGETHER with whether the
    /// finalized path already existed on disk at emit time — the
    /// finalize-before-emit proof.
    struct ChatRig {
        db: Arc<Mutex<rusqlite::Connection>>,
        emits: Arc<Mutex<Vec<(ReceivedChat, usize)>>>,
        clips: Arc<Mutex<Vec<ReceivedClipboard>>>,
        persists: Arc<Mutex<Vec<(String, String)>>>,
        dir: std::path::PathBuf,
        files: Arc<Mutex<Vec<(ReceivedFile, bool)>>>,
        errors: Arc<Mutex<Vec<FileError>>>,
    }

    /// Unique temp download dir per call; stale ones from prior runs of the
    /// same label are removed first (test hygiene only).
    fn temp_download_dir(label: &str) -> std::path::PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "lan-chat-client-dl-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn install_chat_hooks(shared: &HubShared, label: &str) -> ChatRig {
        let db_path = temp_chat_db(label);
        let db = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));
        let emits = Arc::new(Mutex::new(Vec::new()));
        let clips = Arc::new(Mutex::new(Vec::new()));
        let persists = Arc::new(Mutex::new(Vec::new()));
        let dir = temp_download_dir(label);
        fs::create_dir_all(&dir).unwrap();
        let files = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let installed = shared.set_hooks(HubHooks {
            with_db: {
                let db = db.clone();
                Arc::new(move |run: &mut dyn FnMut(&mut rusqlite::Connection)| {
                    let mut guard = db.lock().unwrap();
                    run(&mut guard);
                })
            },
            chat_emit: {
                let log = emits.clone();
                let probe_path = db_path.clone();
                Arc::new(move |msg: &ReceivedChat| {
                    let probe = rusqlite::Connection::open(&probe_path).unwrap();
                    let rows: i64 = probe
                        .query_row(
                            "SELECT COUNT(*) FROM messages WHERE id = ?1",
                            rusqlite::params![msg.id],
                            |r| r.get(0),
                        )
                        .unwrap();
                    log.lock().unwrap().push((msg.clone(), rows as usize));
                })
            },
            clipboard_emit: {
                let log = clips.clone();
                let probe_path = db_path.clone();
                Arc::new(move |msg: &ReceivedClipboard| {
                    let probe = rusqlite::Connection::open(&probe_path).unwrap();
                    let rows: i64 = probe
                        .query_row(
                            "SELECT COUNT(*) FROM messages WHERE id = ?1",
                            rusqlite::params![msg.id],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(rows, 1, "clipboard emit must run AFTER the row committed");
                    log.lock().unwrap().push(msg.clone());
                })
            },
            contact_persist: {
                let log = persists.clone();
                Arc::new(move |key: &str, name: &str| {
                    log.lock()
                        .unwrap()
                        .push((key.to_string(), name.to_string()));
                })
            },
            download_dir: {
                let dir = dir.clone();
                Arc::new(move || dir.to_string_lossy().into_owned())
            },
            file_received: {
                let log = files.clone();
                Arc::new(move |f: &ReceivedFile| {
                    let finalized = std::path::Path::new(&f.path).exists();
                    log.lock().unwrap().push((f.clone(), finalized));
                })
            },
            file_error: {
                let log = errors.clone();
                Arc::new(move |e: &FileError| {
                    log.lock().unwrap().push(e.clone());
                })
            },
        });
        assert!(installed.is_ok(), "startup hooks install exactly once");
        ChatRig {
            db,
            emits,
            clips,
            persists,
            dir,
            files,
            errors,
        }
    }

    /// `(device_key, text, mine)` of a stored message row, if any.
    fn row_of(rig: &ChatRig, id: &str) -> Option<(String, String, i64)> {
        let guard = rig.db.lock().unwrap();
        guard
            .query_row(
                "SELECT device_key, text, mine FROM messages WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .ok()
    }

    /// Bounded wait until `n` emits were observed by the hook seam.
    async fn wait_for_emits(rig: &ChatRig, n: usize) {
        for _ in 0..400 {
            if rig.emits.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("emit #{n} never observed");
    }

    /// Full pairing dance over the wire: presence, pair-request (code is
    /// local-only), pair-verify with the real code, pair-ok unicast, grant
    /// committed. Returns once the conn is authorized for chat.
    async fn pair_web_peer(
        shared: &HubShared,
        events: &mut mpsc::Receiver<HubEvent>,
        ws: &mut ServerWs,
        conn_id: &str,
        name: &str,
        sid: &str,
    ) {
        let peers = serde_json::json!({
            "type": "peers",
            "list": [{ "id": conn_id, "name": name, "kind": "web", "sid": sid }]
        });
        ws.send(Message::text(peers.to_string())).await.unwrap();
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(ev, HubEvent::PresenceChanged { .. }),
            "peer must be live before pairing"
        );

        ws.send(Message::text(
            serde_json::json!({
                "type": "relay", "from_id": conn_id, "from_name": name, "from_sid": sid,
                "payload": { "type": "pair-request", "reqId": "r-1" }
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let mut code = String::new();
        for _ in 0..400 {
            let snap = shared.pairing.lock().unwrap().snapshot();
            if let Some(p) = snap.pending.first() {
                code = p.code.clone();
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(code.len(), 8, "pairing code must be ready");

        ws.send(Message::text(
            serde_json::json!({
                "type": "relay", "from_id": conn_id, "from_name": name, "from_sid": sid,
                "payload": { "type": "pair-verify", "reqId": "r-1", "code": code }
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let ok = read_relay(ws).await;
        assert_eq!(ok["to"], conn_id, "pair-ok is unicast to the requester");
        assert_eq!(ok["payload"]["type"], "pair-ok");
        for _ in 0..400 {
            if !shared.pairing.lock().unwrap().snapshot().paired.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("grant must commit after the pair-ok write");
    }

    fn chat_frame(from_id: &str, sid: &str, text: &str, id: &str) -> String {
        serde_json::json!({
            "type": "relay", "from_id": from_id, "from_name": "Ana", "from_sid": sid,
            "payload": { "type": "chat", "text": text, "id": id }
        })
        .to_string()
    }

    #[tokio::test]
    async fn paired_chat_persists_row_before_emit_with_correct_payload() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let rig = install_chat_hooks(&shared, "paired-chat");
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        pair_web_peer(&shared, &mut events, &mut ws, "w1", "Ana", "sid-ana").await;

        ws.send(Message::text(chat_frame("w1", "sid-ana", "hola", "m-1")))
            .await
            .unwrap();
        wait_for_emits(&rig, 1).await;

        let emits = rig.emits.lock().unwrap().clone();
        assert_eq!(emits.len(), 1);
        let (msg, rows_at_emit) = &emits[0];
        assert_eq!(
            *rows_at_emit, 1,
            "the row must be committed BEFORE the emit hook ran"
        );
        assert!(
            msg.key.starts_with("hub:"),
            "key is a minted contact: {msg:?}"
        );
        assert_eq!(msg.name, "Ana");
        assert_eq!(msg.text, "hola");
        assert_eq!(msg.id, "m-1");
        assert_eq!(
            row_of(&rig, "m-1"),
            Some((msg.key.clone(), "hola".into(), 0)),
            "stored row carries the granted contact key, text and mine=0"
        );
        assert_eq!(
            *rig.persists.lock().unwrap(),
            vec![(msg.key.clone(), "Ana".to_string())],
            "the grant minted and persisted exactly this contact"
        );

        // A chat never draws a reply.
        let stray = tokio::time::timeout(Duration::from_millis(150), ws.next()).await;
        assert!(
            stray.is_err(),
            "chat must not draw any reply, got {stray:?}"
        );

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn unpaired_and_unknown_sender_chat_drops_without_row_or_emit() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let rig = install_chat_hooks(&shared, "unpaired-chat");
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        // w1 is live but NEVER paired.
        let peers = serde_json::json!({
            "type": "peers",
            "list": [{ "id": "w1", "name": "Ana", "kind": "web", "sid": "sid-ana" }]
        });
        ws.send(Message::text(peers.to_string())).await.unwrap();
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ev, HubEvent::PresenceChanged { .. }));

        ws.send(Message::text(chat_frame("w1", "sid-ana", "hola", "m-1")))
            .await
            .unwrap();
        // Unknown conn: not in the presence snapshot at all.
        ws.send(Message::text(chat_frame("wz", "sid-ghost", "otra", "m-9")))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            rig.emits.lock().unwrap().is_empty(),
            "unauthorized chats never emit"
        );
        assert_eq!(row_of(&rig, "m-1"), None, "unpaired chats never persist");
        assert_eq!(row_of(&rig, "m-9"), None, "unknown senders never persist");
        assert!(rig.persists.lock().unwrap().is_empty());

        // Dropped chats never draw a reply either.
        let stray = tokio::time::timeout(Duration::from_millis(150), ws.next()).await;
        assert!(stray.is_err(), "dropped chat must not reply, got {stray:?}");

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn duplicate_chat_frame_is_one_row_and_one_emit() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let rig = install_chat_hooks(&shared, "duplicate-chat");
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        pair_web_peer(&shared, &mut events, &mut ws, "w1", "Ana", "sid-ana").await;

        let frame = chat_frame("w1", "sid-ana", "hola", "m-1");
        ws.send(Message::text(frame.clone())).await.unwrap();
        wait_for_emits(&rig, 1).await;

        // Exact replay: same id + same text.
        ws.send(Message::text(frame)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        let emits = rig.emits.lock().unwrap().clone();
        assert_eq!(emits.len(), 1, "duplicates never emit a second time");
        assert_eq!(
            row_of(&rig, "m-1"),
            Some((emits[0].0.key.clone(), "hola".into(), 0)),
            "exactly one row is stored"
        );

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    // ── Inbound clipboard dispatch ──────────────────────────────

    fn clipboard_frame(from_id: &str, sid: &str, text: &str, id: &str) -> String {
        serde_json::json!({
            "type": "relay", "from_id": from_id, "from_name": "Ana", "from_sid": sid,
            "payload": { "type": "clipboard", "text": text, "kind": "app", "id": id }
        })
        .to_string()
    }

    #[tokio::test]
    async fn paired_clipboard_persists_row_emits_clipboard_only_then_duplicate_silent() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let rig = install_chat_hooks(&shared, "paired-clipboard");
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        pair_web_peer(&shared, &mut events, &mut ws, "w1", "Ana", "sid-ana").await;

        let frame = clipboard_frame("w1", "sid-ana", "texto copiado", "c-1");
        ws.send(Message::text(frame.clone())).await.unwrap();

        for _ in 0..400 {
            if !rig.clips.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let clips = rig.clips.lock().unwrap().clone();
        assert_eq!(clips.len(), 1, "exactly one clipboard event");
        let msg = &clips[0];
        assert_eq!(msg.name, "Ana");
        assert_eq!(msg.text, "texto copiado");
        assert_eq!(msg.id, "c-1");
        assert!(msg.key.starts_with("hub:"));
        assert_eq!(
            row_of(&rig, "c-1"),
            Some((msg.key.clone(), "texto copiado".into(), 0)),
            "stored as a NORMAL text row under the granted contact key"
        );
        // The chat event is NEVER fired for a clipboard frame: the
        // frontend owns single-entry rendering.
        assert!(
            rig.emits.lock().unwrap().is_empty(),
            "clipboard must not emit the chat event"
        );

        // Exact replay: silent — no second clipboard event, no second row.
        ws.send(Message::text(frame)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(rig.clips.lock().unwrap().len(), 1, "duplicates stay silent");
        assert_eq!(
            row_of(&rig, "c-1"),
            Some((msg.key.clone(), "texto copiado".into(), 0))
        );

        // A clipboard frame never draws a reply.
        let stray = tokio::time::timeout(Duration::from_millis(150), ws.next()).await;
        assert!(stray.is_err(), "clipboard must not reply, got {stray:?}");

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn unauthorized_clipboard_frames_dropped_without_row_or_event() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let rig = install_chat_hooks(&shared, "unpaired-clipboard");
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        // w1 live but NEVER paired; wz unknown entirely.
        let peers = serde_json::json!({
            "type": "peers",
            "list": [{ "id": "w1", "name": "Ana", "kind": "web", "sid": "sid-ana" }]
        });
        ws.send(Message::text(peers.to_string())).await.unwrap();
        let ev = tokio::time::timeout(TEST_BOUND, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ev, HubEvent::PresenceChanged { .. }));

        ws.send(Message::text(clipboard_frame(
            "w1", "sid-ana", "hola", "c-1",
        )))
        .await
        .unwrap();
        ws.send(Message::text(clipboard_frame(
            "wz",
            "sid-ghost",
            "otra",
            "c-9",
        )))
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(rig.clips.lock().unwrap().is_empty());
        assert!(rig.emits.lock().unwrap().is_empty());
        assert_eq!(
            row_of(&rig, "c-1"),
            None,
            "unpaired clipboard never persists"
        );
        assert_eq!(row_of(&rig, "c-9"), None);

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    // ── Inbound file dispatch (Slice 5b: client wiring) ─────────

    #[tokio::test]
    async fn paired_file_frame_persists_finalizes_emits_then_duplicate_is_silent() {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let rig = install_chat_hooks(&shared, "paired-file");
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));

        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        pair_web_peer(&shared, &mut events, &mut ws, "w1", "Ana", "sid-ana").await;

        let frame = serde_json::json!({
            "type": "relay", "from_id": "w1", "from_name": "Ana", "from_sid": "sid-ana",
            "payload": { "type": "file", "name": "nota.txt", "data": "aG9sYQ==", "size": 4, "id": "f-1" }
        });
        ws.send(Message::text(frame.to_string())).await.unwrap();

        for _ in 0..400 {
            if !rig.files.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let files = rig.files.lock().unwrap().clone();
        assert_eq!(files.len(), 1, "exactly one file event");
        let (f, finalized_at_emit) = &files[0];
        assert!(f.key.starts_with("hub:"), "key is a minted contact: {f:?}");
        assert_eq!(f.name, "nota.txt");
        assert_eq!(f.size, 4);
        assert_eq!(f.id, "f-1");
        assert_eq!(
            f.path,
            rig.dir.join("nota.txt").to_string_lossy().into_owned()
        );
        assert!(
            *finalized_at_emit,
            "the file hook must fire only AFTER the finalize rename"
        );
        assert_eq!(fs::read(&f.path).unwrap(), b"hola");

        let guard = rig.db.lock().unwrap();
        let (key, text, fp): (String, String, String) = guard
            .query_row(
                "SELECT device_key, text, file_path FROM messages WHERE id = 'f-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        drop(guard);
        assert_eq!(key, f.key);
        assert_eq!(text, "nota.txt", "the row stores the display name");
        assert_eq!(fp, f.path, "the row points at the final path");

        // Exact replay: same id + same content -> no second event, no error.
        ws.send(Message::text(frame.to_string())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            rig.files.lock().unwrap().len(),
            1,
            "duplicate id never emits a second event"
        );
        assert!(rig.errors.lock().unwrap().is_empty());

        // A file never draws a reply.
        let stray = tokio::time::timeout(Duration::from_millis(150), ws.next()).await;
        assert!(
            stray.is_err(),
            "file must not draw any reply, got {stray:?}"
        );

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
        let _ = fs::remove_dir_all(&rig.dir);
    }

    // ── Outbound relay queue + hub_send_text ────────────────────

    /// Real session + paired web peer: returns (shared, ws, task, shutdown)
    /// with the grant committed, so `contact_for_conn(conn)` resolves.
    async fn paired_session(
        conn_id: &str,
    ) -> (
        Arc<HubShared>,
        ServerWs,
        tokio::task::JoinHandle<()>,
        watch::Sender<bool>,
    ) {
        let (listener, mut events) = event_channel();
        let (url, mut conns) = spawn_ws_server().await;
        let shared = Arc::new(HubShared::new(url.clone(), Some(listener)));
        let (sx, srx) = shutdown_channel();
        let task = tokio::spawn(run_hub_client(
            shared.clone(),
            config(url, Duration::from_millis(50)),
            srx,
        ));
        let mut ws = connect_session(&mut conns, &mut events, "self-1").await;
        pair_web_peer(&shared, &mut events, &mut ws, conn_id, "Ana", "sid-ana").await;
        (shared, ws, task, sx)
    }

    #[tokio::test]
    async fn session_up_registers_outbound_queue_and_teardown_clears_it() {
        let (shared, mut ws, task, sx) = paired_session("w1").await;

        assert!(
            shared.outbound.lock().unwrap().is_some(),
            "a live session must expose its outbound queue"
        );

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            shared.outbound.lock().unwrap().is_none(),
            "session teardown must clear the outbound queue"
        );
        let _ = ws.close(None).await;
    }

    #[tokio::test]
    async fn hub_send_text_unknown_contact_is_pair_required_and_sends_no_frame() {
        let (shared, mut ws, task, sx) = paired_session("w1").await;

        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_text_to_contact("hub:nope", "m-1", "hola"),
        )
        .await
        .unwrap();
        assert_eq!(res, Err("pair-required".to_string()));

        // No frame may reach the wire for an unknown contact.
        let stray = tokio::time::timeout(Duration::from_millis(150), ws.next()).await;
        assert!(stray.is_err(), "no frame must be sent, got {stray:?}");

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn hub_send_text_without_a_live_session_is_hub_unavailable() {
        let shared = Arc::new(HubShared::new("wss://unused".into(), None));
        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_text_to_contact("hub:whatever", "m-1", "hola"),
        )
        .await
        .unwrap();
        assert_eq!(res, Err("hub-unavailable".to_string()));
    }

    #[tokio::test]
    async fn hub_send_text_to_conn_absent_from_presence_is_peer_offline() {
        let (shared, _ws, task, sx) = paired_session("w1").await;
        let key = shared.contact_for_conn("w1").expect("grant minted a key");

        // Seed the disagreement window the guard exists for: a natural
        // departure wipes the contact first (pair-required), so only direct
        // runtime seeding models "contact resolves, peer not in presence".
        shared.runtime.lock().unwrap().presence = Default::default();

        let res =
            tokio::time::timeout(TEST_BOUND, shared.send_text_to_contact(&key, "m-1", "hola"))
                .await
                .unwrap();
        assert_eq!(res, Err("peer-offline".to_string()));

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn hub_send_text_happy_path_delivers_exact_relay_frame_and_acks_sent() {
        let (shared, mut ws, task, sx) = paired_session("w1").await;
        let key = shared.contact_for_conn("w1").expect("grant minted a key");

        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_text_to_contact(&key, "m-7", "hola hub"),
        )
        .await
        .unwrap();
        assert_eq!(res, Ok("sent".to_string()));

        let frame = read_relay(&mut ws).await;
        assert_eq!(frame["to"], "w1");
        assert_eq!(
            frame["payload"],
            serde_json::json!({
                "type": "chat",
                "text": "hola hub",
                "kind": "app",
                "id": "m-7"
            }),
            "the exact chat relay payload must reach the wire"
        );

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn hub_send_text_times_out_to_hub_unavailable_when_never_acked() {
        let (shared, _ws, task, sx) = paired_session("w1").await;
        let key = shared.contact_for_conn("w1").expect("grant minted a key");

        // Paused clock: the production ack bound elapses virtually, keeping
        // the test fast while exercising the real timeout path. The swapped
        // queue is never drained, so the oneshot ack never resolves.
        tokio::time::pause();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OutboundMsg>();
        *shared.outbound.lock().unwrap() = Some(tx);

        let res = shared.send_text_to_contact(&key, "m-9", "hola").await;
        assert_eq!(res, Err("hub-unavailable".to_string()));

        sx.send(true).unwrap();
        tokio::time::resume();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn hub_send_text_rejects_empty_and_oversized_text_before_any_state() {
        let shared = Arc::new(HubShared::new("wss://unused".into(), None));
        for bad in ["", "   "] {
            let res = shared.send_text_to_contact("hub:x", "m-1", bad).await;
            assert_eq!(res, Err("invalid-text".to_string()), "input {bad:?}");
        }
        let oversized = "x".repeat(4001);
        let res = shared
            .send_text_to_contact("hub:x", "m-1", &oversized)
            .await;
        assert_eq!(res, Err("invalid-text".to_string()));
    }

    // ── Outbound clipboard: hub_send_clipboard ──────────────────

    #[tokio::test]
    async fn hub_send_clipboard_unknown_contact_is_pair_required_and_sends_no_frame() {
        let (shared, mut ws, task, sx) = paired_session("w1").await;

        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_clipboard_to_contact("hub:nope", "c-1", "hola"),
        )
        .await
        .unwrap();
        assert_eq!(res, Err("pair-required".to_string()));

        let stray = tokio::time::timeout(Duration::from_millis(150), ws.next()).await;
        assert!(stray.is_err(), "no frame must be sent, got {stray:?}");

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn hub_send_clipboard_without_a_live_session_is_hub_unavailable() {
        let shared = Arc::new(HubShared::new("wss://unused".into(), None));
        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_clipboard_to_contact("hub:whatever", "c-1", "hola"),
        )
        .await
        .unwrap();
        assert_eq!(res, Err("hub-unavailable".to_string()));
    }

    #[tokio::test]
    async fn hub_send_clipboard_to_conn_absent_from_presence_is_peer_offline() {
        let (shared, _ws, task, sx) = paired_session("w1").await;
        let key = shared.contact_for_conn("w1").expect("grant minted a key");
        shared.runtime.lock().unwrap().presence = Default::default();

        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_clipboard_to_contact(&key, "c-1", "hola"),
        )
        .await
        .unwrap();
        assert_eq!(res, Err("peer-offline".to_string()));

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn hub_send_clipboard_happy_path_delivers_exact_relay_frame_and_acks_sent() {
        let (shared, mut ws, task, sx) = paired_session("w1").await;
        let key = shared.contact_for_conn("w1").expect("grant minted a key");

        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_clipboard_to_contact(&key, "c-7", "hola clipboard"),
        )
        .await
        .unwrap();
        assert_eq!(res, Ok("sent".to_string()));

        let frame = read_relay(&mut ws).await;
        assert_eq!(frame["to"], "w1");
        assert_eq!(
            frame["payload"],
            serde_json::json!({
                "type": "clipboard",
                "text": "hola clipboard",
                "kind": "app",
                "id": "c-7"
            }),
            "the exact clipboard relay payload must reach the wire"
        );

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn hub_send_clipboard_rejects_empty_and_over_bound_text_before_any_state() {
        let shared = Arc::new(HubShared::new("wss://unused".into(), None));
        for bad in ["", "   "] {
            let res = shared.send_clipboard_to_contact("hub:x", "c-1", bad).await;
            assert_eq!(res, Err("invalid-text".to_string()), "input {bad:?}");
        }
        let over = "x".repeat(crate::clipboard::MAX_CLIPBOARD_TEXT + 1);
        let res = shared
            .send_clipboard_to_contact("hub:x", "c-1", &over)
            .await;
        assert_eq!(res, Err("invalid-text".to_string()));
        // At the bound (after trim) it passes the gate; the session gate
        // rejects with hub-unavailable (no link), proving gate order.
        let exact = "x".repeat(crate::clipboard::MAX_CLIPBOARD_TEXT);
        let res = shared
            .send_clipboard_to_contact("hub:x", "c-1", &format!("  {exact} "))
            .await;
        assert_eq!(res, Err("hub-unavailable".to_string()));
    }

    // ── Outbound files: hub_send_file ───────────────────────────

    /// Writes a temp input file for outbound-file tests and returns its full
    /// path (the caller cleans up its dir).
    fn temp_file(label: &str, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lan-out-file-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn cleanup_temp_file(path: &std::path::Path) {
        let dir = path.parent().expect("temp file has a parent").to_path_buf();
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(dir);
    }

    #[tokio::test]
    async fn hub_send_file_unknown_contact_is_pair_required_and_sends_no_frame() {
        let (shared, mut ws, task, sx) = paired_session("w1").await;
        let path = temp_file("pair-req", "nota.txt", b"hola");

        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_file_to_contact("hub:nope", "f-1", &path.to_string_lossy()),
        )
        .await
        .unwrap();
        assert_eq!(res, Err("pair-required".to_string()));

        // No frame may reach the wire for an unknown contact.
        let stray = tokio::time::timeout(Duration::from_millis(150), ws.next()).await;
        assert!(stray.is_err(), "no frame must be sent, got {stray:?}");

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
        cleanup_temp_file(&path);
        let _ = ws.close(None).await;
    }

    #[tokio::test]
    async fn hub_send_file_oversized_fails_file_too_large_before_reading() {
        // MAX_FILE_BYTES + 1 bytes AND unreadable: a metadata-first size gate
        // answers "file-too-large" without ever touching the contents; a
        // read-first implementation would surface the read error instead.
        let bytes = vec![0u8; crate::hub::inbox::MAX_FILE_BYTES + 1];
        let path = temp_file("oversize", "big.bin", &bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        }

        let shared = Arc::new(HubShared::new("wss://unused".into(), None));
        let res = shared
            .send_file_to_contact("hub:x", "f-big", &path.to_string_lossy())
            .await;
        assert_eq!(
            res,
            Err("file-too-large".to_string()),
            "the size gate must precede any read attempt"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        }
        cleanup_temp_file(&path);
    }

    #[tokio::test]
    async fn hub_send_file_happy_path_delivers_exact_relay_frame_and_acks_sent() {
        let (shared, mut ws, task, sx) = paired_session("w1").await;
        let key = shared.contact_for_conn("w1").expect("grant minted a key");

        let bytes = b"PDF!".to_vec();
        let path = temp_file("happy", "informe.txt", &bytes);

        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_file_to_contact(&key, "f-9", &path.to_string_lossy()),
        )
        .await
        .unwrap();
        assert_eq!(res, Ok("sent".to_string()));

        let frame = read_relay(&mut ws).await;
        assert_eq!(frame["to"], "w1");
        // The temp file lives inside a directory: the payload name must be
        // the bare final component only.
        assert_eq!(
            frame["payload"],
            serde_json::json!({
                "type": "file",
                "name": "informe.txt",
                "data": crate::hub::b64::encode(&bytes),
                "size": bytes.len(),
                "kind": "app",
                "id": "f-9"
            }),
            "the exact file relay payload must reach the wire"
        );
        assert_eq!(
            crate::hub::b64::decode(frame["payload"]["data"].as_str().unwrap()),
            Ok(bytes.clone()),
            "wire data must decode back to the original bytes"
        );

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
        cleanup_temp_file(&path);
        let _ = ws.close(None).await;
    }

    #[tokio::test]
    async fn hub_send_file_to_conn_absent_from_presence_is_peer_offline() {
        let (shared, _ws, task, sx) = paired_session("w1").await;
        let key = shared.contact_for_conn("w1").expect("grant minted a key");
        let path = temp_file("offline", "nota.txt", b"hola");

        // Seed the disagreement window: contact resolves, presence is empty.
        shared.runtime.lock().unwrap().presence = Default::default();

        let res = tokio::time::timeout(
            TEST_BOUND,
            shared.send_file_to_contact(&key, "f-2", &path.to_string_lossy()),
        )
        .await
        .unwrap();
        assert_eq!(res, Err("peer-offline".to_string()));

        sx.send(true).unwrap();
        tokio::time::timeout(TEST_BOUND, task)
            .await
            .unwrap()
            .unwrap();
        cleanup_temp_file(&path);
    }

    /// TLS regression: the wss connector must have a process crypto provider
    /// (rustls `ring` feature). Without it, `connect_async` PANICS while
    /// building the rustls ClientConfig ("no process-level CryptoProvider"),
    /// silently killing the hub task — and with panic=abort in release,
    /// crashing at launch. The connector is only built once TCP succeeds, so
    /// this binds a local accept-and-drop listener: the wss attempt must
    /// resolve as `Err` (TLS/EOF), never panic and never hang.
    #[tokio::test]
    async fn wss_connect_to_local_tcp_completes_as_err_without_provider_panic() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        let outcome = timeout(
            Duration::from_secs(5),
            connect_async(format!("wss://{addr}/hub")),
        )
        .await;
        assert!(outcome.is_ok(), "connect_async must complete, not hang");
        assert!(
            matches!(outcome.unwrap(), Err(_)),
            "wss connect to a non-TLS socket must resolve as Err (no CryptoProvider panic)"
        );
    }
}
