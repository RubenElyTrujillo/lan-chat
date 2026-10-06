// Tests for the pairing wire layer (Slice 3b). Kept in a separate file on
// purpose: pure algorithms live in pairing.rs, wire glue is exercised here
// with injected seams (no sockets, no sleeps).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::hub::pairing::{Clock, CodeGen, CodeGenError, CODE_TTL};
use crate::hub::pairing_wire::{PairedEntry, PairingSnapshot, PairingWire};
use crate::hub::presence::{HubPeer, HubPresenceSnapshot};

const SESS: &str = "sess-1";

#[derive(Clone)]
struct FakeClock(Arc<(Instant, AtomicU64)>);

impl FakeClock {
    fn started() -> Self {
        Self(Arc::new((Instant::now(), AtomicU64::new(0))))
    }
    fn advance(&self, d: Duration) {
        self.0 .1.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.0 .0 + Duration::from_millis(self.0 .1.load(Ordering::SeqCst))
    }
}

#[derive(Clone, Default)]
struct SeqGen(Arc<Mutex<u64>>);

impl CodeGen for SeqGen {
    fn generate(&mut self) -> Result<String, CodeGenError> {
        let mut n = self.0.lock().unwrap();
        *n += 1;
        Ok(format!("{:08}", 10_000_000 + *n))
    }
}

/// Generator that fails on demand: exercises the `Unavailable` -> `code`
/// reason mapping without faking hub internals.
#[derive(Clone, Default)]
struct FailingGen {
    fail: Arc<std::sync::atomic::AtomicBool>,
}

impl CodeGen for FailingGen {
    fn generate(&mut self) -> Result<String, CodeGenError> {
        if self.fail.load(Ordering::SeqCst) {
            Err(CodeGenError::Entropy)
        } else {
            Ok("12345678".into())
        }
    }
}

type Capture = Arc<Mutex<Vec<PairingSnapshot>>>;

struct Rig {
    wire: PairingWire,
    clock: FakeClock,
    emitted: Capture,
}

fn rig() -> Rig {
    let clock = FakeClock::started();
    let mut wire = PairingWire::with_seams(
        Box::new(clock.clone()),
        Box::new(clock.clone()),
        Box::new(SeqGen::default()),
    );
    let emitted: Capture = Arc::new(Mutex::new(Vec::new()));
    let sink = emitted.clone();
    wire.set_emitter(Arc::new(move |s| sink.lock().unwrap().push(s.clone())));
    Rig {
        wire,
        clock,
        emitted,
    }
}

/// Presence snapshot with peers keyed (conn_id, sid); empty sid = none.
fn presence(peers: &[(&str, &str)]) -> HubPresenceSnapshot {
    HubPresenceSnapshot {
        peers: peers
            .iter()
            .map(|(id, sid)| HubPeer {
                conn_id: id.to_string(),
                name: format!("n-{id}"),
                kind: "web".into(),
                sid: (!sid.is_empty()).then(|| sid.to_string()),
                caps: vec![],
            })
            .collect(),
    }
}

/// Exact hub relay shape: the private hub server implementation stamps
/// `from_id`/`from_name` and `from_sid` only when the conn announced one; no
/// sid = absent stamp. Wire contract documented in the private hub repo.
fn relay(from: &str, payload: Value) -> Value {
    json!({
        "type": "relay",
        "from_id": from,
        "from_name": format!("n-{from}"),
        "payload": payload
    })
}

fn request(from: &str, req: &str) -> Value {
    relay(from, json!({ "type": "pair-request", "reqId": req }))
}

fn verify(from: &str, req: &str, code: &str) -> Value {
    relay(
        from,
        json!({ "type": "pair-verify", "reqId": req, "code": code }),
    )
}

fn pair_ok(req: &str) -> Value {
    json!({ "type": "pair-ok", "reqId": req })
}

fn pair_error(req: &str, reason: &str) -> Value {
    json!({ "type": "pair-error", "reqId": req, "reason": reason })
}

/// Live session with w1/w2/w3 present.
fn live(r: &mut Rig) {
    r.wire.on_welcome(SESS);
    r.wire
        .on_presence(&presence(&[("w1", "sid-1"), ("w2", "sid-2"), ("w3", "")]));
    r.emitted.lock().unwrap().clear();
}

fn code_of(r: &mut Rig) -> String {
    r.wire.snapshot().pending[0].code.clone()
}

// ---- request path ----

#[test]
fn request_replies_nothing_and_code_lives_only_in_snapshot() {
    let mut r = rig();
    live(&mut r);
    // A payload-level `from` is raw data: never trusted over the envelope.
    let frame = relay(
        "w1",
        json!({ "type": "pair-request", "reqId": "r1", "from": "ghost", "kind": "app" }),
    );
    // A request NEVER completes pairing: no wire reply at all, so no reply
    // frame could ever carry or imply the code. pair-ok is reserved for a
    // correct pair-verify.
    assert!(
        r.wire.handle_relay(&frame).is_none(),
        "a pair-request must not draw any reply, least of all a pair-ok"
    );
    let snap = r.wire.snapshot();
    assert_eq!(snap.pending.len(), 1);
    let p = &snap.pending[0];
    assert_eq!(p.conn_id, "w1");
    assert_eq!(p.name, "n-w1");
    assert_eq!(p.req_id, "r1");
    assert_eq!(p.code.len(), 8);
    assert!(p.code.chars().all(|c| c.is_ascii_digit()));
    assert!(p.expires_in_ms > 0 && p.expires_in_ms <= CODE_TTL.as_millis() as u64);
    assert!(snap.paired.is_empty(), "a request never authorizes");
    assert_eq!(
        r.emitted.lock().unwrap().len(),
        1,
        "local snapshot still emits"
    );
}

#[test]
fn repeat_request_keeps_one_pending_and_stays_silent() {
    let mut r = rig();
    live(&mut r);
    assert!(r.wire.handle_relay(&request("w1", "r1")).is_none());
    let first = code_of(&mut r);
    assert!(
        r.wire.handle_relay(&request("w1", "r1")).is_none(),
        "a repeated request stays silent too: still no pair-ok"
    );
    let snap = r.wire.snapshot();
    assert_eq!(snap.pending.len(), 1);
    assert_eq!(
        snap.pending[0].code, first,
        "same live code, no re-generation"
    );
    assert!(snap.paired.is_empty());
}

#[test]
fn pair_ok_comes_only_from_a_correct_verify_never_from_a_request() {
    let mut r = rig();
    live(&mut r);
    // Request and repeat: silent, pending local-only, never authorized.
    assert!(r.wire.handle_relay(&request("w1", "r1")).is_none());
    assert!(r.wire.handle_relay(&request("w1", "r1")).is_none());
    assert_eq!(r.wire.snapshot().pending.len(), 1);
    assert!(r.wire.snapshot().paired.is_empty(), "a request never pairs");
    let code = code_of(&mut r);
    assert!(
        r.emitted
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.pending.len() == 1),
        "the code surfaces only through the local snapshot channel"
    );

    // Wrong verify: bounded error, never pair-ok.
    let wrong = r
        .wire
        .handle_relay(&verify("w1", "r1", "99999999"))
        .unwrap();
    assert_eq!(wrong.payload, pair_error("r1", "code"));

    // Correct verify: the ONLY pair-ok, correlated to the same req id, and
    // the code never rides along.
    let ok = r.wire.handle_relay(&verify("w1", "r1", &code)).unwrap();
    assert_eq!(ok.to, "w1");
    assert_eq!(ok.payload, pair_ok("r1"));
    assert!(
        !ok.payload.to_string().contains(&code),
        "code never travels the wire"
    );
    r.wire.commit_relayed(&ok, SESS);
    assert_eq!(
        r.wire.snapshot().paired.len(),
        1,
        "grant commits after the write"
    );
}

#[test]
fn request_reasons_map_busy_rate_and_unavailable() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    r.wire.handle_relay(&request("w2", "r1"));
    // w3: both slots taken -> busy
    assert_eq!(
        r.wire.handle_relay(&request("w3", "r1")).unwrap().payload,
        pair_error("r1", "busy")
    );
    // w2: ten generations in the rolling window -> rate
    for i in 0..10 {
        r.wire.handle_relay(&request("w2", &format!("g{i}")));
    }
    assert_eq!(
        r.wire.handle_relay(&request("w2", "gx")).unwrap().payload,
        pair_error("gx", "rate")
    );
}

#[test]
fn generator_failure_maps_to_detail_free_code_reason() {
    let clock = FakeClock::started();
    let gen = FailingGen::default();
    let mut wire = PairingWire::with_seams(
        Box::new(clock.clone()),
        Box::new(clock),
        Box::new(gen.clone()),
    );
    wire.on_welcome(SESS);
    wire.on_presence(&presence(&[("w1", "sid-1")]));
    gen.fail.store(true, Ordering::SeqCst);
    let reply = wire.handle_relay(&request("w1", "r1")).unwrap();
    assert_eq!(reply.payload, pair_error("r1", "code"));
    assert!(wire.snapshot().pending.is_empty(), "no code, no pending");
}

// ---- envelope validation ----

#[test]
fn malformed_frames_are_ignored_silently() {
    let mut r = rig();
    live(&mut r);
    let frames = vec![
        json!({ "type": "peers", "list": [] }),
        json!({ "type": "relay", "payload": { "type": "pair-request", "reqId": "r1" } }),
        json!({ "type": "relay", "from_id": 42, "payload": { "type": "pair-request", "reqId": "r1" } }),
        json!({ "type": "relay", "from_id": "w1" }),
        json!({ "type": "relay", "from_id": "w1", "payload": "nope" }),
        json!({ "type": "relay", "from_id": "w1", "payload": { "type": "chat-send" } }),
        // The hub never stamps `from`: a from-only frame is not a real relay.
        json!({ "type": "relay", "from": "w1", "payload": { "type": "pair-request", "reqId": "r1" } }),
    ];
    for f in frames {
        assert!(r.wire.handle_relay(&f).is_none(), "must ignore: {f}");
    }
    assert!(r.wire.snapshot().pending.is_empty());
    assert!(r.emitted.lock().unwrap().is_empty(), "ignores never emit");
}

#[test]
fn unregistered_conn_is_ignored_even_with_valid_payload() {
    let mut r = rig();
    live(&mut r);
    assert!(r.wire.handle_relay(&request("ghost", "r1")).is_none());
    assert!(
        r.wire.handle_relay(&request("wX", "r1")).is_none(),
        "not-in-presence conns are ignored, never error-replied"
    );
}

#[test]
fn from_sid_mismatch_is_ignored_match_is_accepted() {
    let mut r = rig();
    live(&mut r);
    let spoof = json!({
        "type": "relay", "from_id": "w1", "from_sid": "sid-2",
        "payload": { "type": "pair-request", "reqId": "r1" }
    });
    assert!(
        r.wire.handle_relay(&spoof).is_none(),
        "stamped sid mismatch ignored"
    );
    assert!(r.wire.snapshot().pending.is_empty());

    let honest = json!({
        "type": "relay", "from_id": "w1", "from_sid": "sid-1",
        "payload": { "type": "pair-request", "reqId": "r1" }
    });
    assert!(
        r.wire.handle_relay(&honest).is_none(),
        "accepted request stays silent"
    );
    assert_eq!(r.wire.snapshot().pending.len(), 1);
}

#[test]
fn legacy_from_only_frame_is_rejected_fail_closed() {
    let mut r = rig();
    live(&mut r);
    let frame = json!({
        "type": "relay", "from": "w1",
        "payload": { "type": "pair-request", "reqId": "r1" }
    });
    assert!(
        r.wire.handle_relay(&frame).is_none(),
        "from-only frame is not a hub relay: rejected, no legacy fallback"
    );
    assert!(r.wire.snapshot().pending.is_empty());
    assert!(r.emitted.lock().unwrap().is_empty(), "rejects never emit");
}

#[test]
fn envelope_identity_is_from_id_only_even_when_from_disagrees() {
    let mut r = rig();
    live(&mut r);
    let spoofed = json!({
        "type": "relay", "from": "ghost", "from_id": "w1", "from_name": "n-w1",
        "payload": { "type": "pair-request", "reqId": "r1" }
    });
    assert!(
        r.wire.handle_relay(&spoofed).is_none(),
        "accepted request stays silent: routing by from_id, no reply"
    );
    assert_eq!(r.wire.snapshot().pending[0].conn_id, "w1");

    let from_id_unknown = json!({
        "type": "relay", "from": "w1", "from_id": "ghost",
        "payload": { "type": "pair-request", "reqId": "r2" }
    });
    assert!(
        r.wire.handle_relay(&from_id_unknown).is_none(),
        "from_id wins even when it points at an unknown conn"
    );
    assert_eq!(
        r.wire.snapshot().pending.len(),
        1,
        "rejected frame adds nothing beyond w1's earlier request"
    );
}

#[test]
fn legacy_or_malformed_reqid_is_rejected_incompatible() {
    let mut r = rig();
    live(&mut r);
    let legacy = relay("w1", json!({ "type": "pair-request" }));
    let reply = r.wire.handle_relay(&legacy).unwrap();
    assert_eq!(reply.payload["reason"], "incompatible");
    assert!(
        r.wire.snapshot().pending.is_empty(),
        "never silently authorize"
    );

    let bad_charset = request("w1", "bad id!");
    assert_eq!(
        r.wire.handle_relay(&bad_charset).unwrap().payload["reason"],
        "incompatible"
    );
    assert!(r.wire.snapshot().pending.is_empty());
}

// ---- verify path ----

#[test]
fn three_wrong_codes_kill_pending_then_correct_is_stale_silent() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    let code = code_of(&mut r);
    for _ in 0..3 {
        assert_eq!(
            r.wire
                .handle_relay(&verify("w1", "r1", "99999999"))
                .unwrap()
                .payload,
            pair_error("r1", "code")
        );
    }
    assert!(
        r.wire.snapshot().pending.is_empty(),
        "pending dead after 3 strikes"
    );
    assert!(
        r.wire.handle_relay(&verify("w1", "r1", &code)).is_none(),
        "stale verify is silent, never authorized"
    );
}

#[test]
fn correct_code_grants_paired_and_replies_only_to_match() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    r.wire.handle_relay(&verify("w1", "r1", "99999999"));
    r.wire.handle_relay(&verify("w1", "r1", "99999999"));
    let code = code_of(&mut r);
    let reply = r.wire.handle_relay(&verify("w1", "r1", &code)).unwrap();
    assert_eq!(reply.payload, pair_ok("r1"));
    r.wire.commit_relayed(&reply, SESS);
    let snap = r.wire.snapshot();
    assert!(snap.pending.is_empty());
    assert_eq!(
        snap.paired,
        vec![PairedEntry {
            conn_id: "w1".into(),
            req_id: "r1".into(),
        }]
    );
}

#[test]
fn wrong_peer_probe_is_silent_and_consumes_nothing() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    let code = code_of(&mut r);
    assert!(
        r.wire.handle_relay(&verify("w2", "r1", &code)).is_none(),
        "another peer's verify never replies nor grants"
    );
    assert_eq!(r.wire.snapshot().pending.len(), 1, "w1's pending untouched");
    assert!(
        r.wire.handle_relay(&verify("w1", "r1", &code)).is_some(),
        "owner still verifies"
    );
}

#[test]
fn expired_verify_replies_code_and_query_drops_expired_without_reply() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    let code = code_of(&mut r);
    r.clock.advance(CODE_TTL);
    // Query drops expired state; no stale code ever reaches the UI.
    assert!(r.wire.snapshot().pending.is_empty());
    // Timer tick: expiry surfaces through an event, no network involved.
    r.emitted.lock().unwrap().clear();
    r.wire.expire_tick();
    assert_eq!(
        r.emitted.lock().unwrap().len(),
        1,
        "expiry emits cleared snapshot"
    );
    // Late verify: expired -> bounded error, never a grant.
    assert_eq!(
        r.wire
            .handle_relay(&verify("w1", "r1", &code))
            .unwrap()
            .payload,
        pair_error("r1", "code")
    );
    assert!(r.wire.snapshot().paired.is_empty());
}

// ---- cancel ----

#[test]
fn browser_cancel_clears_only_matching_pending() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    assert!(r
        .wire
        .handle_relay(&relay(
            "w1",
            json!({ "type": "pair-cancel", "reqId": "other" })
        ))
        .is_none());
    assert_eq!(
        r.wire.snapshot().pending.len(),
        1,
        "mismatched cancel is a no-op"
    );
    let reply = r
        .wire
        .handle_relay(&relay(
            "w1",
            json!({ "type": "pair-cancel", "reqId": "r1" }),
        ))
        .unwrap();
    assert_eq!(reply.payload, pair_ok("r1"));
    assert!(r.wire.snapshot().pending.is_empty());
}

#[test]
fn native_cancel_clears_only_own_pending_and_emits() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    r.emitted.lock().unwrap().clear();
    assert!(
        !r.wire.cancel("w1", "wrong"),
        "only the matching pending cancels"
    );
    assert!(r.wire.cancel("w1", "r1"));
    assert!(r.wire.snapshot().pending.is_empty());
    assert_eq!(
        r.emitted.lock().unwrap().len(),
        1,
        "UI cancel emits cleared snapshot"
    );
}

// ---- lifecycle ----

#[test]
fn presence_departure_drops_pending_and_grant_and_rejoin_inherits_nothing() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    {
        let code = code_of(&mut r);
        let reply = r.wire.handle_relay(&verify("w1", "r1", &code)).unwrap();
        r.wire.commit_relayed(&reply, SESS);
    }
    assert_eq!(r.wire.snapshot().paired.len(), 1);

    r.wire.on_presence(&presence(&[("w2", "sid-2")]));
    let snap = r.wire.snapshot();
    assert!(snap.paired.is_empty(), "departure kills the grant");
    assert!(snap.pending.is_empty());

    assert!(
        r.wire.handle_relay(&request("w1", "r9")).is_none(),
        "departed peer must be ignored"
    );
    // Rejoin is a fresh peer.
    r.wire.on_presence(&presence(&[("w1", "sid-1")]));
    assert!(
        r.wire.handle_relay(&request("w1", "r9")).is_none(),
        "request stays silent"
    );
    assert_eq!(
        r.wire.snapshot().pending.len(),
        1,
        "fresh code shows locally"
    );
    assert!(
        r.wire.snapshot().paired.is_empty(),
        "grant never resurrects"
    );
}

#[test]
fn link_down_then_welcome_resets_everything() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    let code = code_of(&mut r);
    r.wire.handle_relay(&verify("w1", "r1", &code));

    r.emitted.lock().unwrap().clear();
    r.wire.on_link_down();
    let snap = r.wire.snapshot();
    assert!(snap.pending.is_empty() && snap.paired.is_empty());
    let emitted = r.emitted.lock().unwrap();
    assert_eq!(emitted.len(), 1, "link down emits one empty snapshot");
    assert!(emitted[0].pending.is_empty() && emitted[0].paired.is_empty());
    drop(emitted);

    r.wire.on_welcome(SESS);
    assert!(
        r.wire.handle_relay(&request("w1", "r1")).is_none(),
        "no presence yet after welcome: requests ignored"
    );
}

// ---- shape ----

#[test]
fn snapshot_serializes_exact_frontend_shape() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    let v = serde_json::to_value(r.wire.snapshot()).unwrap();
    assert_eq!(v["pending"][0]["conn_id"], "w1");
    assert_eq!(v["pending"][0]["req_id"], "r1");
    assert_eq!(v["pending"][0]["name"], "n-w1");
    assert!(v["pending"][0]["code"].is_string());
    assert!(v["pending"][0]["expires_in_ms"].is_u64());
    assert_eq!(v["paired"], json!([]));

    let code = code_of(&mut r);
    let reply =
        serde_json::to_value(r.wire.handle_relay(&verify("w1", "r1", &code)).unwrap()).unwrap();
    assert_eq!(reply["to"], "w1");
    assert_eq!(reply["payload"]["type"], "pair-ok");
    assert_eq!(reply["payload"]["reqId"], "r1");
    assert!(
        reply["payload"].get("code").is_none(),
        "code never leaves the desktop"
    );
}

#[test]
fn next_deadline_tracks_soonest_pending_only() {
    let mut r = rig();
    live(&mut r);
    assert!(r.wire.next_deadline().is_none(), "no pending, no deadline");
    r.wire.handle_relay(&request("w1", "r1"));
    let d1 = r.wire.next_deadline().unwrap();
    r.wire.handle_relay(&request("w2", "r1"));
    let d2 = r.wire.next_deadline().unwrap();
    assert!(
        d2 >= d1,
        "deadline is the soonest expiry, monotonic with time"
    );
}

// ---- commit-after-send ----

#[test]
fn verify_ok_authorizes_only_after_the_reply_write_is_committed() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    r.emitted.lock().unwrap().clear();
    let code = code_of(&mut r);
    let reply = r.wire.handle_relay(&verify("w1", "r1", &code)).unwrap();
    assert_eq!(reply.payload, pair_ok("r1"));
    // Verified but uncommitted: nothing paired, nothing emitted yet.
    let snap = r.wire.snapshot();
    assert!(snap.pending.is_empty(), "the verified code was consumed");
    assert!(
        snap.paired.is_empty(),
        "no paired UI before the write succeeds"
    );
    assert!(
        r.emitted.lock().unwrap().is_empty(),
        "the paired emit waits for the write"
    );

    r.wire.commit_relayed(&reply, SESS);
    let snap = r.wire.snapshot();
    assert_eq!(
        snap.paired,
        vec![PairedEntry {
            conn_id: "w1".into(),
            req_id: "r1".into(),
        }]
    );
    assert_eq!(
        r.emitted.lock().unwrap().len(),
        1,
        "one paired snapshot event after commit"
    );
}

#[test]
fn commit_relayed_ignores_non_ok_replies_and_stale_sessions() {
    let mut r = rig();
    live(&mut r);
    // A pair-error carries no provisional: commit is a no-op.
    r.wire.handle_relay(&request("w1", "r1"));
    let err_reply = r
        .wire
        .handle_relay(&verify("w1", "r1", "99999999"))
        .unwrap();
    assert_eq!(err_reply.payload, pair_error("r1", "code"));
    r.wire.commit_relayed(&err_reply, SESS);
    assert!(r.wire.snapshot().paired.is_empty());
    assert_eq!(r.emitted.lock().unwrap().len(), 1, "no extra emit");

    let code = code_of(&mut r);
    let reply = r.wire.handle_relay(&verify("w1", "r1", &code)).unwrap();
    r.wire.commit_relayed(&reply, "other-session");
    let snap = r.wire.snapshot();
    assert!(
        snap.paired.is_empty(),
        "a commit under a stale session never pairs"
    );
    let emitted = r.emitted.lock().unwrap();
    assert!(
        emitted.iter().all(|s| s.paired.is_empty()),
        "no snapshot ever shows paired for a stale session"
    );
}

#[test]
fn commit_after_new_request_or_departure_is_rejected() {
    // New request supersedes the in-flight verify commit.
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    let code = code_of(&mut r);
    let reply = r.wire.handle_relay(&verify("w1", "r1", &code)).unwrap();
    r.wire.handle_relay(&request("w1", "r2"));
    let emitted = r.emitted.lock().unwrap().len();
    r.wire.commit_relayed(&reply, SESS);
    assert!(r.wire.snapshot().paired.is_empty());
    assert_eq!(
        r.emitted.lock().unwrap().len(),
        emitted,
        "no paired emit for a stale commit"
    );

    // Departure between verify and commit.
    let mut r2 = rig();
    live(&mut r2);
    r2.wire.handle_relay(&request("w1", "r1"));
    let code2 = code_of(&mut r2);
    let reply2 = r2.wire.handle_relay(&verify("w1", "r1", &code2)).unwrap();
    r2.wire
        .on_presence(&presence(&[("w2", "sid-2"), ("w3", "")]));
    r2.wire.commit_relayed(&reply2, SESS);
    assert!(
        r2.wire.snapshot().paired.is_empty(),
        "a departed peer's commit is stale"
    );
}

#[test]
fn rollback_reply_clears_provisional_and_emits_consistent_not_paired() {
    let mut r = rig();
    live(&mut r);
    r.wire.handle_relay(&request("w1", "r1"));
    let code = code_of(&mut r);
    let reply = r.wire.handle_relay(&verify("w1", "r1", &code)).unwrap();
    r.emitted.lock().unwrap().clear();
    r.wire.rollback_relayed(&reply);
    let snap = r.wire.snapshot();
    assert!(
        snap.pending.is_empty() && snap.paired.is_empty(),
        "not paired"
    );
    let emitted = r.emitted.lock().unwrap();
    assert_eq!(emitted.len(), 1, "one consistent cleared snapshot");
    assert!(emitted[0].pending.is_empty() && emitted[0].paired.is_empty());
    drop(emitted);

    // A non-ok reply rolls back nothing and emits nothing.
    let err_reply = r
        .wire
        .handle_relay(&relay("w1", json!({ "type": "pair-request" })))
        .unwrap();
    r.emitted.lock().unwrap().clear();
    r.wire.rollback_relayed(&err_reply);
    assert!(
        r.emitted.lock().unwrap().is_empty(),
        "non-ok rollback is a no-op"
    );

    // Pairing from scratch still works after a rollback: request silent,
    // code only local.
    assert!(
        r.wire.handle_relay(&request("w1", "r2")).is_none(),
        "request stays silent"
    );
    assert_eq!(
        r.wire.snapshot().pending.len(),
        1,
        "code shows locally again"
    );
}

// ---- contact keys (minted history identity per paired conn) ----

type ContactCapture = Arc<Mutex<Vec<(String, String)>>>;

/// Rig with a persist capture installed; returns (rig, captured calls).
fn contact_rig() -> (Rig, ContactCapture) {
    let mut r = rig();
    let calls: ContactCapture = Arc::new(Mutex::new(Vec::new()));
    let sink = calls.clone();
    r.wire.set_contact_persist(Arc::new(move |key, name| {
        sink.lock()
            .unwrap()
            .push((key.to_string(), name.to_string()));
    }));
    (r, calls)
}

/// Full pairing flow for one conn: request, verify with the live code,
/// commit. The mint happens on the commit.
fn pair_conn(r: &mut Rig, conn: &str, req: &str) {
    r.wire.handle_relay(&request(conn, req));
    let code = r
        .wire
        .snapshot()
        .pending
        .iter()
        .find(|p| p.conn_id == conn)
        .unwrap()
        .code
        .clone();
    let reply = r.wire.handle_relay(&verify(conn, req, &code)).unwrap();
    r.wire.commit_relayed(&reply, SESS);
}

#[test]
fn commit_mints_contact_once_and_regrant_keeps_it_stable() {
    let (mut r, calls) = contact_rig();
    live(&mut r);
    pair_conn(&mut r, "w1", "r1");

    let first = r
        .wire
        .contact_for_conn("w1")
        .expect("a committed grant mints a contact key");
    assert!(first.starts_with("hub:"), "history key shape: {first}");
    assert_eq!(first.len(), 4 + 36, "hub: prefix + uuid");
    assert_eq!(
        r.wire.conn_for_contact(&first).as_deref(),
        Some("w1"),
        "reverse lookup finds the conn"
    );

    // Re-grant the SAME conn: the key is reused, never re-minted.
    pair_conn(&mut r, "w1", "r2");
    assert_eq!(
        r.wire.contact_for_conn("w1").as_deref(),
        Some(first.as_str()),
        "stable across re-grants"
    );
    let calls = calls.lock().unwrap();
    assert!(calls.len() >= 2, "persist runs per successful commit");
    assert!(
        calls.iter().all(|(k, _)| k == &first),
        "every persist carries the same minted key"
    );
}

#[test]
fn distinct_conns_mint_distinct_contacts() {
    let (mut r, _) = contact_rig();
    live(&mut r);
    pair_conn(&mut r, "w1", "r1");
    pair_conn(&mut r, "w2", "r1");

    let a = r.wire.contact_for_conn("w1").unwrap();
    let b = r.wire.contact_for_conn("w2").unwrap();
    assert_ne!(a, b, "two conns never share a history key");
    assert_eq!(r.wire.conn_for_contact(&a).as_deref(), Some("w1"));
    assert_eq!(r.wire.conn_for_contact(&b).as_deref(), Some("w2"));
}

#[test]
fn persist_hook_receives_contact_and_presence_name() {
    let (mut r, calls) = contact_rig();
    live(&mut r);
    pair_conn(&mut r, "w1", "r1");

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "one persist for the first mint");
    assert!(
        calls[0].0.starts_with("hub:"),
        "contact key: {}",
        calls[0].0
    );
    assert_eq!(calls[0].1, "n-w1", "display name comes from presence");
}

#[test]
fn commit_without_persist_hook_still_mints_and_stale_commit_mints_nothing() {
    let mut r = rig();
    live(&mut r);
    pair_conn(&mut r, "w1", "r1");
    let k = r
        .wire
        .contact_for_conn("w1")
        .expect("stored in memory even with no persist hook");
    assert!(k.starts_with("hub:"));

    // A pair-error commit is a no-op: no grant, no mint.
    r.wire.handle_relay(&request("w2", "r9"));
    let err = r
        .wire
        .handle_relay(&verify("w2", "r9", "11111111"))
        .unwrap();
    r.wire.commit_relayed(&err, SESS);
    assert!(
        r.wire.contact_for_conn("w2").is_none(),
        "an error commit never mints"
    );
}

#[test]
fn peer_departure_clears_the_contact_both_ways() {
    let (mut r, _) = contact_rig();
    live(&mut r);
    pair_conn(&mut r, "w1", "r1");
    let k = r.wire.contact_for_conn("w1").unwrap();

    r.wire.on_presence(&presence(&[("w2", "sid-2")]));
    assert!(
        r.wire.contact_for_conn("w1").is_none(),
        "departure clears the forward entry"
    );
    assert!(
        r.wire.conn_for_contact(&k).is_none(),
        "departure clears the reverse entry"
    );
}

#[test]
fn link_down_and_fresh_welcome_clear_contacts() {
    let (mut r, _) = contact_rig();
    live(&mut r);
    pair_conn(&mut r, "w1", "r1");
    assert!(
        r.wire.contact_for_conn("w1").is_some(),
        "minted before the link drops"
    );

    r.wire.on_link_down();
    assert!(
        r.wire.contact_for_conn("w1").is_none(),
        "session down clears minted contacts"
    );
    r.wire.on_welcome(SESS);
    assert!(r.wire.contact_for_conn("w1").is_none());
}

#[test]
fn hubshared_helpers_forward_and_disconnect_clears() {
    use crate::hub::HubShared;
    let shared = HubShared::new("wss://hub.test/hub".into(), None);
    shared.set_connected(SESS.into());
    shared.set_presence(presence(&[("w1", "sid-1")]));

    // Drive the pairing through the shared wire exactly like the session
    // task does (guard dropped before the helpers: no re-entrant lock).
    let minted = {
        let mut pairing = shared.pairing.lock().unwrap();
        pairing.handle_relay(&request("w1", "r1"));
        let code = pairing
            .snapshot()
            .pending
            .iter()
            .find(|p| p.conn_id == "w1")
            .unwrap()
            .code
            .clone();
        let reply = pairing.handle_relay(&verify("w1", "r1", &code)).unwrap();
        pairing.commit_relayed(&reply, SESS);
        pairing.contact_for_conn("w1")
    };
    let key = minted.expect("commit minted through the shared wire");
    assert_eq!(
        shared.contact_for_conn("w1").as_deref(),
        Some(key.as_str()),
        "forward helper"
    );
    assert_eq!(
        shared.conn_for_contact(&key).as_deref(),
        Some("w1"),
        "reverse helper"
    );
    assert!(
        shared
            .conn_for_contact("hub:00000000-0000-4000-8000-000000000000")
            .is_none(),
        "unknown contact maps to nothing"
    );

    shared.set_disconnected("closed".into());
    assert!(
        shared.contact_for_conn("w1").is_none(),
        "disconnect clears contacts"
    );
}
