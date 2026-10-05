// Hub client: WSS link to the public relay (presence, pairing, chat, files).
// TLS is always validated: tokio-tungstenite is compiled with
// `rustls-tls-webpki-roots` and no bypass exists in this module.

pub mod b64;
pub mod backoff;
pub mod client;
pub mod identity;
pub mod inbox;
pub mod pairing;
pub mod pairing_wire;
pub mod presence;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;

use self::client::HubClientConfig;
use self::inbox::{sanitize_name, FileError, ReceivedChat, ReceivedFile, MAX_FILE_BYTES};
use self::pairing_wire::PairingWire;
use self::presence::HubPresenceSnapshot;

/// Lifecycle of the hub link, visible to the frontend through `hub_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HubPhase {
    /// A connection attempt is in flight (connect + hello + welcome).
    Connecting,
    /// A valid welcome was received and the session id is live.
    Connected,
    /// Not connected; waiting before the next attempt.
    Backoff,
    /// The hub is disabled (e.g. identity failure). Only the hub is affected.
    Disabled,
}

/// Volatile hub session state. The ephemeral `session_id` is bound to the
/// current connection and cleared on every disconnect: a new hub session
/// invalidates anything built on the previous one. Never persisted.
/// `presence` is the last peers snapshot of THIS session only. `contact_keys`
/// maps live peer conn ids to their minted history keys (`hub:<uuid>`),
/// assigned at grant-commit and cleared with the session / peer departure.
pub struct HubRuntime {
    pub phase: HubPhase,
    pub connected: bool,
    pub session_id: Option<String>,
    pub reason: Option<String>,
    pub presence: HubPresenceSnapshot,
    pub contact_keys: HashMap<String, String>,
}

impl Default for HubRuntime {
    fn default() -> Self {
        Self {
            phase: HubPhase::Connecting,
            connected: false,
            session_id: None,
            reason: None,
            presence: HubPresenceSnapshot::default(),
            contact_keys: HashMap::new(),
        }
    }
}

/// State transition notifications. The listener seam keeps the runtime
/// testable without a GUI (production wiring forwards these to `hub-state`,
/// and presence snapshots to `hub-presence`).
#[derive(Debug, Clone, PartialEq)]
pub enum HubEvent {
    Connected {
        session_id: String,
    },
    Disconnected {
        reason: String,
    },
    /// The set of visible peers changed. Never maps to a link status.
    PresenceChanged {
        snapshot: HubPresenceSnapshot,
    },
}

pub type HubListener = Arc<dyn Fn(&HubEvent) + Send + Sync>;

/// Upper bound for one outbound chat text (trimmed, UTF-8 bytes).
pub const MAX_OUT_TEXT: usize = 4000;

/// Bound for waiting on the session loop's delivery ack after enqueueing an
/// outbound frame. Timeouts are never queued for retry: the frontend owns it.
pub const OUTBOUND_ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// One queued outbound relay frame with its delivery ack. The session task
/// owns the receiver and resolves the ack after the bounded sink write —
/// never while holding any other lock.
pub struct OutboundMsg {
    /// Fully serialized wire text (relay envelope included).
    pub text: String,
    /// `Ok` when the frame reached the sink; `Err` with a detail-free
    /// reason otherwise (serialization, write failure or timeout).
    pub ack: tokio::sync::oneshot::Sender<Result<(), String>>,
}

/// Production seams the session task needs beyond status events: DB access
/// for the inbound-chat persist path, the frontend emitter for accepted
/// messages, and SQLite persistence for minted contact keys. Set once at
/// startup; every seam is sync, short-lived and never held across an await.
pub struct HubHooks {
    /// Runs a closure with short-lived exclusive DB access. Production locks
    /// the app DB mutex; the closure must not outlive the call.
    pub with_db: Arc<dyn Fn(&mut dyn FnMut(&mut Connection)) + Send + Sync>,
    /// Emits an accepted inbound chat message (`hub-message-received`).
    /// Called strictly AFTER the row was committed.
    pub chat_emit: Arc<dyn Fn(&ReceivedChat) + Send + Sync>,
    /// Persists a minted contact key (`hub:<uuid>`) with the peer display
    /// name. Errors are logged non-fatally by the implementation.
    pub contact_persist: Arc<dyn Fn(&str, &str) + Send + Sync>,
    /// Resolves the current download directory for inbound files
    /// (production: `AppState.download_dir`). Sync and short-lived.
    pub download_dir: Arc<dyn Fn() -> String + Send + Sync>,
    /// Emits an accepted inbound file (`hub-file-received`). Called strictly
    /// AFTER the row was committed AND the file was finalized into the
    /// download dir.
    pub file_received: Arc<dyn Fn(&ReceivedFile) + Send + Sync>,
    /// Emits a refused authorized inbound file (`hub-file-error`): no row
    /// and no staged file exist when it fires.
    pub file_error: Arc<dyn Fn(&FileError) + Send + Sync>,
}

/// Desktop identity for the hub: the stable persisted device id, or a
/// definitive disablement. Identity failure never falls back to a random
/// per-launch id and never affects the rest of the app (LAN bridge, history).
pub enum HubIdentity {
    Enabled { device_id: String },
    Disabled { reason: String },
}

/// Pure wiring seam: turns the persisted-identity lookup result into hub
/// enablement. `Err` (and a defensively rejected empty id) disable ONLY the
/// hub and are surfaced as hub status/reason — never as a panic or a fake id.
pub fn decide_identity(res: Result<String, String>) -> HubIdentity {
    match res {
        Ok(id) if !id.trim().is_empty() => HubIdentity::Enabled { device_id: id },
        Ok(_) => HubIdentity::Disabled {
            reason: "device id is empty".to_string(),
        },
        Err(e) => HubIdentity::Disabled {
            reason: format!("device id unavailable: {e}"),
        },
    }
}

/// Frontend-facing status (Tauri command payload).
#[derive(Serialize, Clone)]
pub struct HubStatus {
    pub url: String,
    pub connected: bool,
    pub phase: HubPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Shared hub state for commands and the session loop. The mutex is only ever
/// held for a short state copy/swap — never across an await point.
pub struct HubShared {
    pub url: String,
    pub runtime: Mutex<HubRuntime>,
    /// Pairing wire state (Slice 3b). Short locks only — never held across
    /// an await point; the mutex is only touched by the session task and
    /// Tauri command handlers.
    pub pairing: Mutex<PairingWire>,
    /// Live outbound relay queue of the CURRENT session. Unbounded: the
    /// command enqueues without ever blocking on the session loop; back
    /// pressure is the bounded ack, never the queue. `None` whenever the
    /// session is down; short lock to clone the sender only.
    pub outbound: Mutex<Option<tokio::sync::mpsc::UnboundedSender<OutboundMsg>>>,
    /// Startup seams (see `HubHooks`); set once, read-only afterwards.
    hooks: OnceLock<HubHooks>,
    listener: Option<HubListener>,
}

/// Pure startup wiring: the shared hub state plus, when the identity is
/// usable, the client session loop (config + its shutdown receiver) to spawn
/// on the async runtime. Identity failure disables ONLY the hub: no client,
/// status carries the reason, the rest of the app is untouched.
pub struct HubStartup {
    pub shared: HubShared,
    /// `Some` only for an enabled identity; run it until `shutdown` fires.
    pub client: Option<(HubClientConfig, tokio::sync::watch::Receiver<bool>)>,
    /// Owner half of the shutdown signal, kept with the app state so exit can
    /// stop the loop even while it waits in connect or backoff.
    pub shutdown: tokio::sync::watch::Sender<bool>,
}

/// Builds the hub startup bundle. `listener` receives every state-transition
/// event (production wiring forwards it to the `hub-state` Tauri event).
pub fn build_startup(
    url: String,
    name: String,
    identity: HubIdentity,
    listener: Option<HubListener>,
) -> HubStartup {
    let shared = HubShared::new(url.clone(), listener);
    let (shutdown, rx) = self::client::shutdown_channel();
    let client = match identity {
        HubIdentity::Enabled { device_id } => Some((
            HubClientConfig {
                url,
                name,
                device_id,
                ..Default::default()
            },
            rx,
        )),
        HubIdentity::Disabled { reason } => {
            shared.set_disabled(reason);
            None
        }
    };
    HubStartup {
        shared,
        client,
        shutdown,
    }
}

/// Full status snapshot derived from a link event: the `hub-state` payload.
/// Pure on purpose — no shared-state read inside the notify path, no
/// re-entry into `HubShared`, and the fields mirror exactly what
/// `set_connected`/`set_disconnected` wrote for the same event.
/// Presence events are NOT link status: they map to `None` and the caller
/// forwards them as `hub-presence` instead (never a wrong or default status).
pub fn hub_event_payload(url: &str, event: &HubEvent) -> Option<HubStatus> {
    match event {
        HubEvent::Connected { session_id } => Some(HubStatus {
            url: url.to_string(),
            connected: true,
            phase: HubPhase::Connected,
            session_id: Some(session_id.clone()),
            reason: None,
        }),
        HubEvent::Disconnected { reason } => Some(HubStatus {
            url: url.to_string(),
            connected: false,
            phase: HubPhase::Backoff,
            session_id: None,
            reason: Some(reason.clone()),
        }),
        HubEvent::PresenceChanged { .. } => None,
    }
}

impl HubShared {
    pub fn new(url: String, listener: Option<HubListener>) -> Self {
        Self {
            url,
            runtime: Mutex::new(HubRuntime::default()),
            pairing: Mutex::new(PairingWire::new()),
            outbound: Mutex::new(None),
            hooks: OnceLock::new(),
            listener,
        }
    }

    /// Snapshot for `hub_status` (short lock).
    pub fn status(&self) -> HubStatus {
        let rt = self.runtime.lock().unwrap();
        HubStatus {
            url: self.url.clone(),
            connected: rt.connected,
            phase: rt.phase,
            session_id: rt.session_id.clone(),
            reason: rt.reason.clone(),
        }
    }

    /// Marks an attempt in flight: the previous session is already invalid.
    pub fn set_connecting(&self) {
        let cleared = {
            let mut rt = self.runtime.lock().unwrap();
            rt.phase = HubPhase::Connecting;
            rt.connected = false;
            rt.session_id = None;
            rt.reason = None;
            take_presence(&mut rt)
        };
        if let Ok(mut pairing) = self.pairing.lock() {
            pairing.on_link_down();
        }
        emit_clear(self, cleared);
    }

    /// Marks the session up after a VALID welcome; stores the ephemeral id.
    /// Emits `Connected`. Pairing starts a fresh in-memory session with it.
    pub fn set_connected(&self, session_id: String) {
        {
            let mut rt = self.runtime.lock().unwrap();
            rt.phase = HubPhase::Connected;
            rt.connected = true;
            rt.session_id = Some(session_id.clone());
            rt.reason = None;
        }
        if let Ok(mut pairing) = self.pairing.lock() {
            pairing.on_welcome(&session_id);
        }
        self.notify(&HubEvent::Connected { session_id });
    }

    /// Marks the session down and clears the ephemeral id. Emits
    /// `Disconnected` only on the connected -> disconnected transition, so
    /// repeated failed attempts never spam duplicate events. Presence is
    /// cleared with it: peers of a dead session are not peers. The outbound
    /// queue is cleared too: a dead session never accepts sends.
    pub fn set_disconnected(&self, reason: String) {
        let (was_connected, cleared) = {
            let mut rt = self.runtime.lock().unwrap();
            let was = rt.connected;
            rt.phase = HubPhase::Backoff;
            rt.connected = false;
            rt.session_id = None;
            rt.reason = Some(reason.clone());
            (was, take_presence(&mut rt))
        };
        // The stale sender dies with the session: pending acks in the old
        // queue resolve as errors when the receiver drops.
        if let Ok(mut outbound) = self.outbound.lock() {
            *outbound = None;
        }
        // Wipe pairs BEFORE the clear events flow: pending pairing UI must
        // read empty in both the event and the query by the time the link
        // is reported down.
        if let Ok(mut pairing) = self.pairing.lock() {
            pairing.on_link_down();
        }
        if was_connected {
            self.notify(&HubEvent::Disconnected { reason });
        }
        emit_clear(self, cleared);
    }

    /// Definitively disables the hub (identity failure). No reconnect loop.
    /// Presence is cleared silently, matching this path's no-events rule;
    /// pairing state is wiped silently with it.
    pub fn set_disabled(&self, reason: String) {
        let mut rt = self.runtime.lock().unwrap();
        rt.phase = HubPhase::Disabled;
        rt.connected = false;
        rt.session_id = None;
        rt.reason = Some(reason);
        take_presence(&mut rt);
        drop(rt);
        if let Ok(mut pairing) = self.pairing.lock() {
            pairing.wipe_silent();
        }
    }

    /// Replaces the current presence snapshot and emits `PresenceChanged`.
    /// Identical snapshots are dropped (the hub rebroadcasts on every group
    /// change; the UI must not be spammed with no-ops). Distinct snapshots
    /// also drive the pairing registry diff (register/remove web peers).
    pub fn set_presence(&self, snapshot: HubPresenceSnapshot) {
        {
            let mut rt = self.runtime.lock().unwrap();
            if rt.presence == snapshot {
                return;
            }
            rt.presence = snapshot.clone();
        }
        if let Ok(mut pairing) = self.pairing.lock() {
            pairing.on_presence(&snapshot);
        }
        self.notify(&HubEvent::PresenceChanged { snapshot });
    }

    /// Snapshot query for `hub_presence` (short lock, cloned payload).
    pub fn presence(&self) -> HubPresenceSnapshot {
        self.runtime.lock().unwrap().presence.clone()
    }

    /// Minted history contact key (`hub:<uuid>`) for a live paired conn.
    /// Short lock on the pairing wire, never held across an await.
    pub fn contact_for_conn(&self, conn_id: &str) -> Option<String> {
        self.pairing
            .lock()
            .ok()
            .and_then(|pairing| pairing.contact_for_conn(conn_id))
    }

    /// Conn owning a minted history contact key, if any. Short lock.
    pub fn conn_for_contact(&self, contact_key: &str) -> Option<String> {
        self.pairing
            .lock()
            .ok()
            .and_then(|pairing| pairing.conn_for_contact(contact_key))
    }

    /// Whether the conn holds a committed grant under the CURRENT hub
    /// session (grants die with the session, so "paired" is always
    /// session-scoped). Short lock, never held across an await.
    pub fn conn_authorized(&self, conn_id: &str) -> bool {
        self.pairing
            .lock()
            .ok()
            .map(|pairing| {
                pairing
                    .snapshot()
                    .paired
                    .iter()
                    .any(|p| p.conn_id == conn_id)
            })
            .unwrap_or(false)
    }

    /// Sends one chat text to a paired contact through the CURRENT session's
    /// outbound queue and awaits the bounded delivery ack. Gates, in order:
    /// local text validation -> live session + queue (`hub-unavailable`) ->
    /// contact resolves (`pair-required`) -> peer in presence
    /// (`peer-offline`). Locks are always short and never held across the
    /// ack await. No replay or queue on failure: the frontend owns retry.
    pub async fn send_text_to_contact(
        &self,
        key: &str,
        id: &str,
        text: &str,
    ) -> Result<String, String> {
        // Local input validation first: garbage input never depends on the
        // link state.
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_OUT_TEXT {
            return Err("invalid-text".to_string());
        }

        // Session up + queue registered (two short locks, no await between).
        let (connected, sender) = {
            let rt = self.runtime.lock().unwrap();
            let out = self.outbound.lock().ok().and_then(|o| o.clone());
            (rt.connected, out)
        };
        if !connected {
            return Err("hub-unavailable".to_string());
        }
        let Some(sender) = sender else {
            return Err("hub-unavailable".to_string());
        };

        // Contact routing (short lock).
        let Some(conn) = self.conn_for_contact(key) else {
            return Err("pair-required".to_string());
        };

        // Presence gate (short lock).
        let live = self.presence().peers.iter().any(|p| p.conn_id == conn);
        if !live {
            return Err("peer-offline".to_string());
        }

        // Fully serialized frame; enqueue + bounded ack. A closed queue or a
        // failed/absent ack both mean the frame was NOT confirmed delivered.
        let frame = serde_json::json!({
            "type": "relay",
            "to": conn,
            "payload": { "type": "chat", "text": text, "kind": "app", "id": id },
        });
        Self::await_delivery(sender, frame).await
    }

    /// Sends one file to a paired contact through the CURRENT session's
    /// outbound queue and awaits the bounded delivery ack. Local file
    /// validation happens BEFORE any link-state check: the size bound comes
    /// from metadata (never reading the payload) and the display name is the
    /// sanitized final path component. Gates, in order: file-too-large /
    /// file-read -> live session + queue (`hub-unavailable`) -> contact
    /// resolves (`pair-required`) -> peer in presence (`peer-offline`).
    /// No replay or queue on failure: the frontend owns retry.
    pub async fn send_file_to_contact(
        &self,
        key: &str,
        id: &str,
        path: &str,
    ) -> Result<String, String> {
        // Local input validation first: the size bound comes from metadata,
        // BEFORE a single byte is read.
        let meta = std::fs::metadata(path).map_err(|_| "file-read".to_string())?;
        if meta.len() as usize > MAX_FILE_BYTES {
            return Err("file-too-large".to_string());
        }
        let bytes = std::fs::read(path).map_err(|_| "file-read".to_string())?;
        let name = sanitize_name(
            std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(""),
        );
        let data = b64::encode(&bytes);

        // Session up + queue registered (two short locks, no await between).
        let (connected, sender) = {
            let rt = self.runtime.lock().unwrap();
            let out = self.outbound.lock().ok().and_then(|o| o.clone());
            (rt.connected, out)
        };
        if !connected {
            return Err("hub-unavailable".to_string());
        }
        let Some(sender) = sender else {
            return Err("hub-unavailable".to_string());
        };

        // Contact routing (short lock).
        let Some(conn) = self.conn_for_contact(key) else {
            return Err("pair-required".to_string());
        };

        // Presence gate (short lock).
        let live = self.presence().peers.iter().any(|p| p.conn_id == conn);
        if !live {
            return Err("peer-offline".to_string());
        }

        // Fully serialized frame; enqueue + bounded ack, exactly like text.
        let frame = serde_json::json!({
            "type": "relay",
            "to": conn,
            "payload": {
                "type": "file", "name": name, "data": data,
                "size": bytes.len(), "kind": "app", "id": id
            },
        });
        Self::await_delivery(sender, frame).await
    }

    /// Serializes one relay frame and waits for the session loop's bounded
    /// delivery ack. Shared tail of both outbound paths (text and file) so
    /// enqueue/ack semantics can never drift apart. A closed queue or a
    /// failed/absent ack both mean the frame was NOT confirmed delivered.
    async fn await_delivery(
        sender: tokio::sync::mpsc::UnboundedSender<OutboundMsg>,
        frame: serde_json::Value,
    ) -> Result<String, String> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        sender
            .send(OutboundMsg {
                text: frame.to_string(),
                ack: ack_tx,
            })
            .map_err(|_| "hub-unavailable".to_string())?;
        match tokio::time::timeout(OUTBOUND_ACK_TIMEOUT, ack_rx).await {
            Ok(Ok(Ok(()))) => Ok("sent".to_string()),
            _ => Err("hub-unavailable".to_string()),
        }
    }

    /// Installed startup seams, if any (set once at startup, read-only
    /// afterwards). The caller must never hold the reference across an
    /// await.
    pub fn hooks(&self) -> Option<&HubHooks> {
        self.hooks.get()
    }

    /// Installs the startup seams once, later from `lib.rs`. The contact
    /// persist seam is forwarded into the pairing wire so minted history
    /// keys persist at grant-commit. Returns `Err(hooks)` untouched — no
    /// seam is clobbered — if they were already installed.
    pub fn set_hooks(&self, hooks: HubHooks) -> Result<(), HubHooks> {
        if self.hooks.get().is_some() {
            return Err(hooks);
        }
        if let Ok(mut pairing) = self.pairing.lock() {
            let persist = hooks.contact_persist.clone();
            pairing.set_contact_persist(Arc::new(move |key, name| persist(key, name)));
        }
        self.hooks.set(hooks)
    }

    fn notify(&self, event: &HubEvent) {
        if let Some(listener) = &self.listener {
            listener(event);
        }
    }
}

/// Swaps presence out; `Some(old)` only when the old snapshot had content —
/// the signal that one `PresenceChanged(empty)` clear event is warranted.
fn take_presence(rt: &mut HubRuntime) -> Option<HubPresenceSnapshot> {
    let old = std::mem::take(&mut rt.presence);
    (!old.is_empty()).then_some(old)
}

/// Emits the single empty clear event after a transition, only when needed.
fn emit_clear(shared: &HubShared, cleared: Option<HubPresenceSnapshot>) {
    if cleared.is_some() {
        shared.notify(&HubEvent::PresenceChanged {
            snapshot: HubPresenceSnapshot::default(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn capture() -> (HubListener, Arc<Mutex<Vec<HubEvent>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        (Arc::new(move |e| sink.lock().unwrap().push(e.clone())), log)
    }

    #[test]
    fn valid_welcome_marks_connected_and_stores_ephemeral_id() {
        let (listener, log) = capture();
        let shared = HubShared::new("wss://hub.test/hub".into(), Some(listener));
        shared.set_connected("abc123".into());

        let st = shared.status();
        assert!(st.connected);
        assert_eq!(st.phase, HubPhase::Connected);
        assert_eq!(st.session_id.as_deref(), Some("abc123"));
        assert_eq!(
            *log.lock().unwrap(),
            vec![HubEvent::Connected {
                session_id: "abc123".into()
            }]
        );
    }

    #[test]
    fn disconnect_clears_session_state_and_emits_once() {
        let (listener, log) = capture();
        let shared = HubShared::new("wss://hub.test/hub".into(), Some(listener));
        shared.set_connected("abc123".into());
        shared.set_disconnected("socket closed".into());
        shared.set_disconnected("socket closed".into()); // duplicate: no re-emit

        let st = shared.status();
        assert!(!st.connected);
        assert_eq!(st.phase, HubPhase::Backoff);
        assert!(
            st.session_id.is_none(),
            "ephemeral session id must be cleared"
        );
        assert_eq!(st.reason.as_deref(), Some("socket closed"));
        assert_eq!(log.lock().unwrap().len(), 2); // Connected + one Disconnected
    }

    #[test]
    fn identity_failure_disables_only_the_hub() {
        match decide_identity(Err("no settings table".into())) {
            HubIdentity::Disabled { reason } => assert!(reason.contains("no settings table")),
            HubIdentity::Enabled { .. } => {
                panic!("identity failure must disable the hub, never fake an id")
            }
        }
        assert!(matches!(
            decide_identity(Ok("persisted-id".into())),
            HubIdentity::Enabled { .. }
        ));
    }

    #[test]
    fn empty_device_id_is_rejected_not_minted() {
        assert!(matches!(
            decide_identity(Ok("   ".into())),
            HubIdentity::Disabled { .. }
        ));
    }

    #[test]
    fn status_reports_disabled_phase() {
        let shared = HubShared::new("wss://hub.test/hub".into(), None);
        shared.set_disabled("device id unavailable: locked".into());

        let st = shared.status();
        assert_eq!(st.phase, HubPhase::Disabled);
        assert!(!st.connected);
        assert!(st.session_id.is_none());
        assert_eq!(st.reason.as_deref(), Some("device id unavailable: locked"));
    }

    #[test]
    fn build_startup_enabled_carries_actual_name_url_and_stable_id() {
        let startup = build_startup(
            "wss://hub.test/hub".into(),
            "Mesa-Fixed".into(),
            HubIdentity::Enabled {
                device_id: "stable-1".into(),
            },
            None,
        );
        let (config, _rx) = startup
            .client
            .expect("enabled identity must ship a client loop");
        assert_eq!(config.name, "Mesa-Fixed");
        assert_eq!(config.device_id, "stable-1");
        assert_eq!(config.url, "wss://hub.test/hub");
        // Production knobs: no injected reconnect delay, bounded timeouts.
        assert!(config.reconnect_delay.is_none());
        assert!(!startup.shutdown.is_closed());
        let st = startup.shared.status();
        assert_eq!(st.phase, HubPhase::Connecting, "loop is about to run");
    }

    #[test]
    fn build_startup_identity_failure_disables_only_hub_without_client() {
        let (listener, log) = capture();
        let startup = build_startup(
            "wss://hub.test/hub".into(),
            "Mesa-Fixed".into(),
            HubIdentity::Disabled {
                reason: "device id unavailable: locked".into(),
            },
            Some(listener),
        );
        assert!(
            startup.client.is_none(),
            "disabled hub must never spawn a client loop"
        );
        let st = startup.shared.status();
        assert_eq!(st.phase, HubPhase::Disabled);
        assert!(!st.connected);
        assert!(st.session_id.is_none());
        assert_eq!(st.reason.as_deref(), Some("device id unavailable: locked"));
        assert!(log.lock().unwrap().is_empty(), "disabled emits no events");
    }

    #[test]
    fn hub_event_payload_translates_events_to_full_status() {
        let up = hub_event_payload(
            "wss://hub.test/hub",
            &HubEvent::Connected {
                session_id: "sess-9".into(),
            },
        )
        .unwrap();
        assert_eq!(up.url, "wss://hub.test/hub");
        assert!(up.connected);
        assert_eq!(up.phase, HubPhase::Connected);
        assert_eq!(up.session_id.as_deref(), Some("sess-9"));
        assert!(up.reason.is_none());

        let down = hub_event_payload(
            "wss://hub.test/hub",
            &HubEvent::Disconnected {
                reason: "connection closed".into(),
            },
        )
        .unwrap();
        assert_eq!(down.url, "wss://hub.test/hub");
        assert!(!down.connected);
        assert_eq!(down.phase, HubPhase::Backoff);
        assert!(down.session_id.is_none());
        assert_eq!(down.reason.as_deref(), Some("connection closed"));
    }

    #[test]
    fn hub_state_event_serializes_snake_case_and_skips_none() {
        let up = serde_json::to_value(
            hub_event_payload(
                "wss://hub.test/hub",
                &HubEvent::Connected {
                    session_id: "s1".into(),
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(up["phase"], "connected");
        assert_eq!(up["session_id"], "s1");
        assert!(up.get("reason").is_none());

        let down = serde_json::to_value(
            hub_event_payload(
                "wss://hub.test/hub",
                &HubEvent::Disconnected {
                    reason: "connect timeout".into(),
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(down["phase"], "backoff");
        assert_eq!(down["reason"], "connect timeout");
        assert!(down.get("session_id").is_none());
    }

    // ---- Presence integration (Slice 2a) ----

    use self::presence::{HubPeer, HubPresenceSnapshot};

    fn snapshot_of(ids: &[&str]) -> HubPresenceSnapshot {
        HubPresenceSnapshot {
            peers: ids
                .iter()
                .map(|id| HubPeer {
                    conn_id: id.to_string(),
                    name: format!("n-{id}"),
                    kind: "web".into(),
                    sid: None,
                    caps: vec![],
                })
                .collect(),
        }
    }

    fn presence_events(log: &Mutex<Vec<HubEvent>>) -> Vec<HubPresenceSnapshot> {
        log.lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                HubEvent::PresenceChanged { snapshot } => Some(snapshot.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn set_presence_stores_snapshot_and_emits_presence_changed() {
        let (listener, log) = capture();
        let shared = HubShared::new("wss://hub.test/hub".into(), Some(listener));
        let snap = snapshot_of(&["w1", "w2"]);
        shared.set_presence(snap.clone());

        assert_eq!(
            shared.presence(),
            snap,
            "presence query returns the stored snapshot"
        );
        let events = presence_events(&log);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], snap);
    }

    #[test]
    fn identical_presence_snapshot_is_not_reemitted() {
        let (listener, log) = capture();
        let shared = HubShared::new("wss://hub.test/hub".into(), Some(listener));
        let snap = snapshot_of(&["w1"]);
        shared.set_presence(snap.clone());
        shared.set_presence(snap);

        assert_eq!(presence_events(&log).len(), 1);
    }

    #[test]
    fn replacing_presence_emits_each_distinct_snapshot() {
        let (listener, log) = capture();
        let shared = HubShared::new("wss://hub.test/hub".into(), Some(listener));
        shared.set_presence(snapshot_of(&["w1"]));
        shared.set_presence(snapshot_of(&["w1", "w2"]));

        let events = presence_events(&log);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].peers.len(), 2);
    }

    #[test]
    fn disconnect_clears_presence_and_emits_empty_once() {
        let (listener, log) = capture();
        let shared = HubShared::new("wss://hub.test/hub".into(), Some(listener));
        shared.set_connected("sess".into());
        shared.set_presence(snapshot_of(&["w1"]));
        shared.set_disconnected("closed".into());

        assert!(shared.presence().is_empty(), "session down means no peers");
        let events = presence_events(&log);
        assert_eq!(events.len(), 2, "non-empty snapshot then empty clear");
        assert!(events[1].is_empty());

        shared.set_disconnected("closed".into()); // duplicate disconnect: no spam
        assert_eq!(presence_events(&log).len(), 2);
    }

    #[test]
    fn disconnect_with_no_presence_emits_no_extra_clear() {
        let (listener, log) = capture();
        let shared = HubShared::new("wss://hub.test/hub".into(), Some(listener));
        shared.set_connected("sess".into());
        shared.set_disconnected("closed".into());
        assert!(
            presence_events(&log).is_empty(),
            "empty clear only when needed"
        );
    }

    #[test]
    fn new_attempt_resets_presence() {
        let (listener, log) = capture();
        let shared = HubShared::new("wss://hub.test/hub".into(), Some(listener));
        shared.set_connected("sess".into());
        shared.set_presence(snapshot_of(&["w1"]));
        shared.set_connecting();

        assert!(shared.presence().is_empty());
        let events = presence_events(&log);
        assert_eq!(events.len(), 2);
        assert!(events[1].is_empty());
    }

    #[test]
    fn disabled_hub_clears_presence_silently() {
        let (listener, log) = capture();
        let shared = HubShared::new("wss://hub.test/hub".into(), Some(listener));
        shared.set_connected("sess".into());
        shared.set_presence(snapshot_of(&["w1"]));
        shared.set_disabled("device id unavailable".into());

        assert!(shared.presence().is_empty());
        assert_eq!(
            presence_events(&log).len(),
            1,
            "disabled emits no new events"
        );
    }

    #[test]
    fn hub_event_payload_returns_none_for_presence_not_a_fake_status() {
        let payload = hub_event_payload(
            "wss://hub.test/hub",
            &HubEvent::PresenceChanged {
                snapshot: snapshot_of(&["w1"]),
            },
        );
        assert!(
            payload.is_none(),
            "presence must never be reported as link status"
        );
    }

    // ---- Contact persist wiring (set_hooks) ----

    #[test]
    fn set_hooks_forwards_contact_persist_to_pairing_commits() {
        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = calls.clone();
        let shared = HubShared::new("wss://hub.test/hub".into(), None);
        let installed = shared.set_hooks(HubHooks {
            with_db: Arc::new(|_: &mut dyn FnMut(&mut Connection)| {}),
            chat_emit: Arc::new(|_: &inbox::ReceivedChat| {}),
            contact_persist: Arc::new(move |key, name| {
                sink.lock()
                    .unwrap()
                    .push((key.to_string(), name.to_string()));
            }),
            download_dir: Arc::new(|| String::new()),
            file_received: Arc::new(|_: &inbox::ReceivedFile| {}),
            file_error: Arc::new(|_: &inbox::FileError| {}),
        });
        assert!(installed.is_ok(), "first install wins");
        let second = shared.set_hooks(HubHooks {
            with_db: Arc::new(|_: &mut dyn FnMut(&mut Connection)| {}),
            chat_emit: Arc::new(|_: &inbox::ReceivedChat| {}),
            contact_persist: Arc::new(|_, _| {}),
            download_dir: Arc::new(|| String::new()),
            file_received: Arc::new(|_: &inbox::ReceivedFile| {}),
            file_error: Arc::new(|_: &inbox::FileError| {}),
        });
        assert!(second.is_err(), "second install is rejected");

        shared.set_connected("sess".into());
        shared.set_presence(snapshot_of(&["w1"]));
        let key = {
            let mut pairing = shared.pairing.lock().unwrap();
            let frame = serde_json::json!({
                "type": "relay", "from_id": "w1",
                "payload": { "type": "pair-request", "reqId": "r1" }
            });
            pairing.handle_relay(&frame);
            let code = pairing.snapshot().pending[0].code.clone();
            let verify = serde_json::json!({
                "type": "relay", "from_id": "w1",
                "payload": { "type": "pair-verify", "reqId": "r1", "code": code }
            });
            let reply = pairing.handle_relay(&verify).unwrap();
            pairing.commit_relayed(&reply, "sess");
            pairing.contact_for_conn("w1")
        }
        .expect("commit minted a contact");
        assert!(key.starts_with("hub:"));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(key, "n-w1".to_string())],
            "the lib.rs-installed hook sees (contact, name) at commit"
        );
    }
}
