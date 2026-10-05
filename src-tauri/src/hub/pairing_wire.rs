// Pairing wire layer (Slice 3b): glue between the pure `PairingHub` state
// machine and the hub runtime/transport. No sockets here.
//
// Contract:
// - The hub owns pairing truth (codes, attempts, TTL, grants). This layer
//   never mirrors it: snapshots and deadlines are DERIVED via the hub's
//   read-only views, so UI state can never diverge from auth state.
// - Inbound relay envelopes `{"type":"relay","from_id":<conn>,"payload":{...}}`
//   are handled ONLY for peers live in the CURRENT presence snapshot (web
//   kind). A stamped `from_sid` must match the registered sid for that conn;
//   routing is strictly by conn id — names and sids never authorize.
//   Envelope `from` is never stamped by the hub and never read (fail closed:
//   a from-only frame is dropped, no legacy fallback); raw payload-level
//   `from`/`kind` fields are untrusted and ignored.
// - Recognized payloads: `pair-request`/`pair-verify`/`pair-cancel`, each
//   with a bounded `reqId` (same charset rules as sids). A recognized pair
//   payload without a valid reqId (legacy browser) gets `pair-error
//   incompatible` — never a silent authorization. Anything else is ignored.
// - A `pair-request` NEVER completes pairing: a ready code is local-only
//   (snapshot event for the desktop UI) and draws NO wire reply — only
//   `busy`/`rate`/`code`/`incompatible` errors are replied. `pair-ok` is
//   reserved for a correct `pair-verify` (commit-after-send).
// - Replies go only to the requesting conn, directly on the active sink:
//   `pair-ok`/`pair-error` with `reqId` and reason `code|busy|rate|
//   incompatible`. The generated code NEVER travels the wire, logs, or
//   errors: it is exposed only through `snapshot()` (Tauri `hub_pairing`).
// - Stale verifies (wrong peer / unknown / mismatched req id) are SILENT:
//   no reply, no state change, probes learn nothing.
// - Lifecycle: `on_welcome` starts a fresh hub session, `on_presence` diffs
//   registrations (departure drops pending+grant), `on_link_down` wipes
//   everything and emits one empty snapshot so the pending UI is empty in
//   both event and query. `set_disabled` wipes silently (no-event rule).
// - Expiry never touches the network: `snapshot()`/`expire_tick()` derive
//   from the clock, `next_deadline()` lets the runtime select on the soonest
//   expiry instead of busy-polling.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use serde_json::{json, Value};

use super::pairing::{
    Clock, CodeGen, PairingHub, RequestOutcome, SystemClock, SystemCodeGen, VerifyOutcome, CODE_TTL,
};
use super::presence::{is_valid_sid, HubPresenceSnapshot};

pub const PAYLOAD_REQUEST: &str = "pair-request";
pub const PAYLOAD_VERIFY: &str = "pair-verify";
pub const PAYLOAD_CANCEL: &str = "pair-cancel";

/// One pending pairing attempt, shown ONLY on the desktop UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingEntry {
    pub conn_id: String,
    pub name: String,
    pub req_id: String,
    pub code: String,
    /// Milliseconds left before the code dies (>= 0, never negative).
    pub expires_in_ms: u64,
}

/// One completed pairing (in-memory, current session only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairedEntry {
    pub conn_id: String,
    pub req_id: String,
}

/// `hub_pairing` query / `hub-pairing` event payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PairingSnapshot {
    pub pending: Vec<PendingEntry>,
    pub paired: Vec<PairedEntry>,
}

/// A reply for the active WS sink: unicast envelope to one conn. Serialized
/// exactly as `{"type":"relay","to":...,"payload":{...}}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireReply {
    /// Always `"relay"`: the sink envelope type.
    #[serde(rename = "type")]
    kind: &'static str,
    pub to: String,
    pub payload: Value,
}

impl WireReply {
    fn unicast(to: &str, payload: Value) -> Self {
        Self {
            kind: "relay",
            to: to.to_string(),
            payload,
        }
    }
}

/// Notified whenever the pairing snapshot changes (production wiring emits
/// the `hub-pairing` Tauri event; must not re-enter the wire lock).
pub type PairingEmitter = Arc<dyn Fn(&PairingSnapshot) + Send + Sync>;

/// Persist seam for minted contact keys: receives the `hub:<uuid>` history
/// key and the peer display name at grant-commit time.
pub type ContactPersist = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct PairingWire {
    hub: PairingHub,
    /// Display-expiry clock; reads the same monotonic source as the hub's
    /// (kept separate because `PairingHub` owns its boxed clock).
    clock: Box<dyn Clock>,
    /// Current presence web peers: the ONLY conns allowed to pair.
    names: HashMap<String, String>,
    /// Registered sid per conn (may be None); validated against a stamped
    /// `from_sid` when the envelope carries one.
    sids: HashMap<String, Option<String>>,
    /// Minted history contact keys (`hub:<uuid>`) per paired conn.
    contacts: HashMap<String, String>,
    last: Option<PairingSnapshot>,
    emit: Option<PairingEmitter>,
    persist: Option<ContactPersist>,
}

impl Default for PairingWire {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingWire {
    pub fn new() -> Self {
        Self::with_seams(
            Box::new(SystemClock),
            Box::new(SystemClock),
            Box::new(SystemCodeGen),
        )
    }

    /// Test/determinism seam. `hub_clock` drives the state machine,
    /// `wire_clock` derives display expiry/deadlines — inject the same
    /// deterministic clock (cloned) in tests, `SystemClock` twice in prod.
    pub fn with_seams(
        hub_clock: Box<dyn Clock>,
        wire_clock: Box<dyn Clock>,
        code_gen: Box<dyn CodeGen>,
    ) -> Self {
        Self {
            hub: PairingHub::with_seams(hub_clock, code_gen),
            clock: wire_clock,
            names: HashMap::new(),
            sids: HashMap::new(),
            contacts: HashMap::new(),
            last: None,
            emit: None,
            persist: None,
        }
    }

    pub fn set_emitter(&mut self, emit: PairingEmitter) {
        self.emit = Some(emit);
    }

    /// Installs the contact-key persist seam (see `HubHooks::contact_persist`).
    pub fn set_contact_persist(&mut self, persist: ContactPersist) {
        self.persist = Some(persist);
    }

    /// Minted history contact key for a paired conn, if any.
    pub fn contact_for_conn(&self, conn_id: &str) -> Option<String> {
        self.contacts.get(conn_id).cloned()
    }

    /// Conn that owns a minted history contact key, if any.
    pub fn conn_for_contact(&self, contact_key: &str) -> Option<String> {
        self.contacts
            .iter()
            .find(|(_, key)| key.as_str() == contact_key)
            .map(|(conn, _)| conn.clone())
    }

    /// Valid welcome: a new hub session starts; nothing from before pairs.
    pub fn on_welcome(&mut self, session_id: &str) {
        self.hub.on_session_started(session_id);
        self.names.clear();
        self.sids.clear();
        self.contacts.clear();
        self.emit_if_changed();
    }

    /// Session down: wipe pairs BEFORE the clear events flow so the pending
    /// UI is empty in both the emitted snapshot and the next query.
    pub fn on_link_down(&mut self) {
        self.hub.on_link_down();
        self.names.clear();
        self.sids.clear();
        self.contacts.clear();
        self.emit_if_changed();
    }

    /// Definitive disablement: wipe with NO event (the disabled-hub
    /// no-events rule keeps status reporting quiet).
    pub fn wipe_silent(&mut self) {
        self.hub.on_link_down();
        self.names.clear();
        self.sids.clear();
        self.contacts.clear();
        self.last = Some(self.snapshot());
    }

    /// Diffs the presence snapshot: registers web peers, removes departed
    /// ones (their pending and grant die with them).
    pub fn on_presence(&mut self, snap: &HubPresenceSnapshot) {
        for peer in &snap.peers {
            self.names.insert(peer.conn_id.clone(), peer.name.clone());
            self.sids.insert(peer.conn_id.clone(), peer.sid.clone());
            self.hub.register_peer(&peer.conn_id, peer.sid.as_deref());
        }
        let live: HashSet<&str> = snap.peers.iter().map(|p| p.conn_id.as_str()).collect();
        let departed: Vec<String> = self
            .names
            .keys()
            .filter(|c| !live.contains(c.as_str()))
            .cloned()
            .collect();
        for conn in departed {
            self.hub.remove_peer(&conn);
            self.names.remove(&conn);
            self.sids.remove(&conn);
            self.contacts.remove(&conn);
        }
        self.emit_if_changed();
    }

    /// Handles one inbound relay frame. `None` = ignore (no reply).
    pub fn handle_relay(&mut self, frame: &Value) -> Option<WireReply> {
        if frame.get("type")?.as_str()? != "relay" {
            return None;
        }
        // Identity comes ONLY from the server-stamped `from_id`. The hub
        // never sends a bare `from`, so a missing/non-string from_id fails
        // closed: no legacy fallback, the frame is dropped.
        let from = frame.get("from_id")?.as_str()?;
        // Envelope `from` must be a peer live in the CURRENT presence.
        if !self.names.contains_key(from) {
            return None;
        }
        // A server-stamped from_sid must match the registered sid; absent
        // stamps are accepted (conn id remains the sole routing key).
        if let Some(stamped) = frame.get("from_sid").and_then(Value::as_str) {
            if self.sids.get(from).map(Option::as_deref) != Some(Some(stamped)) {
                return None;
            }
        }
        let payload = frame.get("payload")?.as_object()?;
        // Payload-level from/kind are raw data: never read, never trusted.
        let kind = payload.get("type")?.as_str()?;
        let req = payload.get("reqId").and_then(Value::as_str);
        let (reply, emit_now) = match kind {
            PAYLOAD_REQUEST => (self.handle_request(from, req), true),
            PAYLOAD_VERIFY => {
                let code = payload.get("code").and_then(Value::as_str);
                self.handle_verify(from, req, code)
            }
            PAYLOAD_CANCEL => (self.handle_cancel(from, req), true),
            _ => (None, true),
        };
        // A silent request still emits: the ready code is local-only UI
        // state. A successful verify does NOT emit here: authorization
        // becomes visible only after the pair-ok write succeeds
        // (`commit_relayed`). Emission is change-detected, so error and
        // no-op paths never emit.
        if emit_now {
            self.emit_if_changed();
        }
        reply
    }

    /// Native UI cancel: clears ONLY the matching own pending; no network
    /// message is sent. Returns whether something was cancelled.
    pub fn cancel(&mut self, conn_id: &str, req_id: &str) -> bool {
        let removed = self.hub.cancel_request(conn_id, req_id);
        if removed {
            self.emit_if_changed();
        }
        removed
    }

    /// Derived UI snapshot. Expired entries are dropped here so a query can
    /// never surface a stale code.
    pub fn snapshot(&self) -> PairingSnapshot {
        let now = self.clock.now();
        let pending = self
            .hub
            .pendings()
            .into_iter()
            .filter(|(.., created)| now.duration_since(*created) < CODE_TTL)
            .map(|(conn_id, req_id, code, created)| {
                let name = self.names.get(&conn_id).cloned().unwrap_or_default();
                PendingEntry {
                    expires_in_ms: created
                        .checked_add(CODE_TTL)
                        .and_then(|end| end.checked_duration_since(now))
                        .map_or(0, |d| d.as_millis() as u64),
                    conn_id,
                    name,
                    req_id,
                    code,
                }
            })
            .collect();
        let paired = self
            .hub
            .grants()
            .into_iter()
            .map(|g| PairedEntry {
                conn_id: g.conn_id,
                req_id: g.req_id,
            })
            .collect();
        PairingSnapshot { pending, paired }
    }

    /// Soonest pending expiry for the runtime's select-based timer.
    pub fn next_deadline(&self) -> Option<Instant> {
        let now = self.clock.now();
        self.hub
            .pendings()
            .into_iter()
            .filter(|(.., created)| now.duration_since(*created) < CODE_TTL)
            .filter_map(|(.., created)| created.checked_add(CODE_TTL))
            .min()
    }

    /// Timer tick: emits an updated snapshot if expiry changed it. Purely
    /// local — never sends anything on the wire.
    pub fn expire_tick(&mut self) {
        self.emit_if_changed();
    }

    fn handle_request(&mut self, from: &str, req: Option<&str>) -> Option<WireReply> {
        let Some(req) = req else {
            return Some(self.error(from, "", "incompatible"));
        };
        if !is_valid_sid(req) {
            return Some(self.error(from, req, "incompatible"));
        }
        // A ready code NEVER completes pairing: no wire reply. The code is
        // local-only (snapshot event); pair-ok is reserved for a correct
        // pair-verify (commit-after-send). Errors stay bounded replies.
        match self.hub.request_code(from, req) {
            RequestOutcome::CodeReady { .. } => None,
            RequestOutcome::Busy => Some(self.error(from, req, "busy")),
            RequestOutcome::RateLimited => Some(self.error(from, req, "rate")),
            RequestOutcome::Unavailable => Some(self.error(from, req, "code")),
        }
    }

    fn handle_verify(
        &mut self,
        from: &str,
        req: Option<&str>,
        code: Option<&str>,
    ) -> (Option<WireReply>, bool) {
        let (Some(req), Some(code)) = (req, code) else {
            return (
                Some(self.error(from, req.unwrap_or(""), "incompatible")),
                true,
            );
        };
        if !is_valid_sid(req) {
            return (Some(self.error(from, req, "incompatible")), true);
        }
        // Only an 8-digit code shape reaches the hub; anything else is a
        // malformed payload, not a wrong attempt.
        if code.len() != 8 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return (Some(self.error(from, req, "incompatible")), true);
        }
        match self.hub.verify_code(from, req, code) {
            // Verified but NOT paired: the grant stays provisional until the
            // caller commits after the pair-ok write succeeded.
            VerifyOutcome::Authorized(_) => (Some(self.ok(from, req)), false),
            VerifyOutcome::WrongCode | VerifyOutcome::Expired => {
                (Some(self.error(from, req, "code")), true)
            }
            // Wrong peer / unknown / mismatched req id: silent, no mutation.
            VerifyOutcome::Stale => (None, true),
        }
    }

    fn handle_cancel(&mut self, from: &str, req: Option<&str>) -> Option<WireReply> {
        let Some(req) = req else {
            return Some(self.error(from, "", "incompatible"));
        };
        if !is_valid_sid(req) {
            return Some(self.error(from, req, "incompatible"));
        }
        // Correlated additive reply only on a real cancellation.
        self.hub
            .cancel_request(from, req)
            .then(|| self.ok(from, req))
    }

    /// Confirms a pairing reply reached the sink: commits the provisional
    /// grant (pair-ok verify replies only) under the CURRENT session, mints
    /// the conn's history contact key (once per conn; reused on re-grant)
    /// and emits the paired snapshot. Must be called AFTER the write
    /// succeeded, never while holding any other lock and never across an
    /// await.
    pub fn commit_relayed(&mut self, reply: &WireReply, session_id: &str) {
        let Some((conn, req)) = self.pair_ok_of(reply) else {
            return;
        };
        let committed = self.hub.commit_grant(conn, req, session_id).is_some();
        if committed {
            self.mint_contact(conn);
        }
        self.emit_if_changed();
    }

    /// One stable history key per conn: minted at the first committed
    /// grant, reused on re-grant. The persist seam receives the key with
    /// the peer's current display name; a mint failure (entropy) stores
    /// nothing and never invents a key.
    fn mint_contact(&mut self, conn: &str) {
        let contact = match self.contacts.get(conn) {
            Some(existing) => existing.clone(),
            None => {
                let Ok(uuid) = crate::hub::identity::new_uuid() else {
                    return;
                };
                let key = format!("hub:{uuid}");
                self.contacts.insert(conn.to_string(), key.clone());
                key
            }
        };
        let name = self.names.get(conn).cloned().unwrap_or_default();
        if let Some(persist) = &self.persist {
            persist(&contact, &name);
        }
    }

    /// A pairing reply never reached the sink (serialization or write
    /// failure): discards any provisional grant and emits the consistent
    /// not-paired snapshot so event and query agree.
    pub fn rollback_relayed(&mut self, reply: &WireReply) {
        let Some((conn, _)) = self.pair_ok_of(reply) else {
            return;
        };
        self.hub.discard_provisional(conn);
        self.emit_if_changed();
    }

    /// `(conn, reqId)` of a unicast pair-ok reply, else `None`.
    fn pair_ok_of<'a>(&self, reply: &'a WireReply) -> Option<(&'a str, &'a str)> {
        if reply.payload.get("type").and_then(Value::as_str) != Some("pair-ok") {
            return None;
        }
        let req = reply.payload.get("reqId").and_then(Value::as_str)?;
        Some((reply.to.as_str(), req))
    }

    fn ok(&self, to: &str, req: &str) -> WireReply {
        WireReply::unicast(to, json!({ "type": "pair-ok", "reqId": req }))
    }

    fn error(&self, to: &str, req: &str, reason: &str) -> WireReply {
        WireReply::unicast(
            to,
            json!({ "type": "pair-error", "reqId": req, "reason": reason }),
        )
    }

    /// Emits when the derived snapshot differs from the last emitted one.
    fn emit_if_changed(&mut self) {
        let snap = self.snapshot();
        if self.last.as_ref() == Some(&snap) {
            return;
        }
        self.last = Some(snap.clone());
        if let Some(emit) = &self.emit {
            emit(&snap);
        }
    }
}

#[cfg(test)]
#[path = "pairing_wire_tests.rs"]
mod tests;
