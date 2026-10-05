// Pure browser-pairing state machine for the desktop hub (Slice 3a).
//
// Contract:
// - Pairing is IN-MEMORY ONLY: codes, pending attempts and grants die with
//   this struct. Nothing here reuses the LAN PIN flows, the legacy 8789
//   bridge, or persists any credential or history. No serde, no network.
// - Pure auth: the caller registers peers it already validated as LIVE hub
//   connections (presence, cap 32) and removes them on departure. A claimed
//   `sid` is stored as a display hint and NEVER authorizes anything.
// - Correlation is (browser conn id, req id): req ids are bounded ASCII
//   (same charset rule as presence sids, <= 64 chars). The code is always
//   exactly 8 digits from the injected CSPRNG generator (uniform, rejection
//   sampled on the system path). A generator failure yields `Unavailable`:
//   no code, no pending, no grant.
// - Codes live 120s strict (`created + TTL` inclusive = expired), a peer
//   holds at most one pending, at most 2 peers hold pendings at once, and a
//   wrong code is allowed at most 3 attempts before the pending dies.
// - Repeating the same (conn, req id) returns the live code WITHOUT
//   extending its TTL and WITHOUT counting a generation. A new req id for
//   the same peer replaces (invalidates) the old pending.
// - Rate limit: at most 10 code generations per peer per rolling 600s
//   window. History is trimmed before each request and dies with the peer
//   registration, so total memory is bounded (<= 32 peers x <= 10 entries).
//   This is NOT sybil protection: reconnecting resets history by design —
//   the presence cap is the real gate.
// - Grants bind (browser conn id, req id, desktop hub session). A new hub
//   session, a link drop, or the peer's departure invalidates them — even
//   if the same sid returns later. Explicit cancellation removes ONLY the
//   matching pending attempt; existing grants survive unless explicitly
//   revoked (revocation wiring is a later slice).
// - Wrong peer/session/request never grants and never consumes another
//   peer's pending: every lookup is keyed by the caller's own conn id.
// - Time and randomness are injected seams so every rule above is
//   deterministically testable; production uses `Instant::now` + getrandom.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use super::presence::{is_valid_sid, MAX_CONN_ID_LEN, MAX_SID_LEN};

/// Code lifetime. At `created + CODE_TTL` (inclusive) the code is dead.
pub const CODE_TTL: Duration = Duration::from_secs(120);
/// Rolling window for the per-peer generation rate limit.
pub const GENERATION_WINDOW: Duration = Duration::from_secs(600);
/// Distinct peers allowed to hold a live code at the same time.
pub const MAX_PENDING_PEERS: usize = 2;
/// Wrong-code attempts before the pending is invalidated.
pub const MAX_CODE_ATTEMPTS: u32 = 3;
/// Code generations allowed per peer per `GENERATION_WINDOW`.
pub const MAX_GENERATIONS_PER_WINDOW: usize = 10;
/// Mirrors the presence peer cap: registrations are bounded, so the rate
/// history and grant maps are bounded by construction.
pub const MAX_REGISTERED_PEERS: usize = 32;
pub const MAX_REQ_ID_LEN: usize = 64;

/// Injected monotonic clock (production: `Instant::now`).
pub trait Clock: Send {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Secure code generation failure. Deliberately detail-free: nothing about
/// the entropy source leaks to callers or logs.
#[derive(Debug)]
pub enum CodeGenError {
    Entropy,
}

impl fmt::Display for CodeGenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "secure code generation failed")
    }
}

/// Injected code generator (production: CSPRNG via getrandom, uniform
/// 8-digit codes by rejection sampling). Implementations MUST return
/// exactly 8 ASCII digits; the state machine does not second-guess them.
pub trait CodeGen: Send {
    fn generate(&mut self) -> Result<String, CodeGenError>;
}

pub struct SystemCodeGen;

const CODE_MODULUS: u32 = 100_000_000;

impl SystemCodeGen {
    /// Uniform code in `[00000000, 99999999]`: rejection-sample a full u32
    /// against the largest multiple of 10^8, then take the remainder. The
    /// rejection zone is ~2.2%, so the loop terminates practically always.
    fn random_code() -> Result<String, CodeGenError> {
        let zone = (u32::MAX / CODE_MODULUS) * CODE_MODULUS;
        loop {
            let mut bytes = [0u8; 4];
            getrandom::fill(&mut bytes).map_err(|_| CodeGenError::Entropy)?;
            let value = u32::from_be_bytes(bytes);
            if value < zone {
                return Ok(format!("{:08}", value % CODE_MODULUS));
            }
        }
    }
}

impl CodeGen for SystemCodeGen {
    fn generate(&mut self) -> Result<String, CodeGenError> {
        Self::random_code()
    }
}

/// Result of a code request. `Busy`/`RateLimited` map to the planned wire
/// reasons `busy`/`rate`; `Unavailable` covers "no code possible" (no live
/// session, peer not registered, malformed req id, or generator failure —
/// planned wire reason `code`). No JSON/network happens in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestOutcome {
    CodeReady { code: String },
    Busy,
    RateLimited,
    Unavailable,
}

/// Result of a code verification. `Stale` = nothing to verify for this
/// caller (unknown peer/pending or mismatched req id) and it never mutates
/// state, so probes cannot consume another peer's attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    Authorized(Grant),
    WrongCode,
    Expired,
    Stale,
}

/// In-memory proof that a browser conn completed pairing under the CURRENT
/// desktop hub session. Never persisted, never advertised as a capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub conn_id: String,
    pub req_id: String,
    pub session_id: String,
}

struct PendingCode {
    req_id: String,
    code: String,
    created: Instant,
    wrong: u32,
}

struct PeerRecord {
    /// Untrusted display hint from presence; never used for authorization.
    sid: Option<String>,
    /// Generation timestamps inside the rolling window (bounded by sweep).
    generations: Vec<Instant>,
}

pub struct PairingHub {
    clock: Box<dyn Clock>,
    code_gen: Box<dyn CodeGen>,
    session: Option<String>,
    /// Registered LIVE peers (bounded by MAX_REGISTERED_PEERS).
    peers: HashMap<String, PeerRecord>,
    /// Live codes keyed by browser conn id (<= MAX_PENDING_PEERS entries).
    pending: HashMap<String, PendingCode>,
    /// Grants keyed by browser conn id (bounded by the peer registry).
    grants: HashMap<String, Grant>,
    /// Verified-but-uncommitted grants awaiting the pair-ok write. Never
    /// exposed through `authorized()`/`grants()`: authorization becomes
    /// visible only after the reply reached the sink (commit-after-send).
    provisional: HashMap<String, Grant>,
}

impl Default for PairingHub {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingHub {
    pub fn new() -> Self {
        Self::with_seams(Box::new(SystemClock), Box::new(SystemCodeGen))
    }

    /// Test/determinism seam: injected clock and code generator.
    pub fn with_seams(clock: Box<dyn Clock>, code_gen: Box<dyn CodeGen>) -> Self {
        Self {
            clock,
            code_gen,
            session: None,
            peers: HashMap::new(),
            pending: HashMap::new(),
            grants: HashMap::new(),
            provisional: HashMap::new(),
        }
    }

    /// A new desktop hub session started: everything granted or pending under
    /// the previous session is invalid, full stop.
    pub fn on_session_started(&mut self, session_id: &str) {
        self.wipe();
        self.session = Some(session_id.to_string());
    }

    /// The hub link dropped: no session means nothing pairs or verifies.
    pub fn on_link_down(&mut self) {
        self.wipe();
        self.session = None;
    }

    /// Registers a caller-validated LIVE peer. Re-registration of the same
    /// live conn refreshes the sid hint and keeps rate history (presence
    /// rebroadcasts are idempotent). Returns `false` when the registry is
    /// full or the conn id is invalid — the caller must treat that as "no
    /// pairing for this peer".
    pub fn register_peer(&mut self, conn_id: &str, sid: Option<&str>) -> bool {
        if conn_id.is_empty() || conn_id.len() > MAX_CONN_ID_LEN {
            return false;
        }
        let sid = sid.map(|s| s.chars().take(MAX_SID_LEN).collect::<String>());
        if let Some(record) = self.peers.get_mut(conn_id) {
            record.sid = sid;
            return true;
        }
        if self.peers.len() >= MAX_REGISTERED_PEERS {
            return false;
        }
        self.peers.insert(
            conn_id.to_string(),
            PeerRecord {
                sid,
                generations: Vec::new(),
            },
        );
        true
    }

    /// Peer departure: its rate history, pending code and grant die with it.
    /// A later registration with the same sid is a fresh peer and inherits
    /// nothing.
    pub fn remove_peer(&mut self, conn_id: &str) {
        self.peers.remove(conn_id);
        self.pending.remove(conn_id);
        self.grants.remove(conn_id);
        self.provisional.remove(conn_id);
    }

    /// Requests (or repeats) a pairing code for a registered live peer.
    pub fn request_code(&mut self, conn_id: &str, req_id: &str) -> RequestOutcome {
        if self.session.is_none() || !is_valid_sid(req_id) {
            return RequestOutcome::Unavailable;
        }
        // Expired codes free slots and stale rate entries leave the window
        // before any decision is made.
        self.sweep();
        // A fresh request supersedes any in-flight verify: its pair-ok, if
        // still being written, must never resurrect a grant afterwards.
        self.provisional.remove(conn_id);
        let Some(record) = self.peers.get_mut(conn_id) else {
            return RequestOutcome::Unavailable;
        };
        if let Some(p) = self.pending.get(conn_id) {
            if p.req_id == req_id {
                // Repeat: same live code, TTL and attempt state untouched,
                // and no generation counted.
                return RequestOutcome::CodeReady {
                    code: p.code.clone(),
                };
            }
        }
        if record.generations.len() >= MAX_GENERATIONS_PER_WINDOW {
            return RequestOutcome::RateLimited;
        }
        let others = self.pending.len() - self.pending.contains_key(conn_id) as usize;
        if others >= MAX_PENDING_PEERS {
            return RequestOutcome::Busy;
        }
        let code = match self.code_gen.generate() {
            Ok(code) => code,
            Err(_) => return RequestOutcome::Unavailable,
        };
        let now = self.clock.now();
        record.generations.push(now);
        self.pending.insert(
            conn_id.to_string(),
            PendingCode {
                req_id: req_id.to_string(),
                code: code.clone(),
                created: now,
                wrong: 0,
            },
        );
        RequestOutcome::CodeReady { code }
    }

    /// Verifies a code for the caller's OWN pending. Any mismatch of peer,
    /// session or req id lands in `Stale` without mutating anything.
    pub fn verify_code(&mut self, conn_id: &str, req_id: &str, code: &str) -> VerifyOutcome {
        let Some(p) = self.pending.get(conn_id) else {
            return VerifyOutcome::Stale;
        };
        if p.req_id != req_id {
            return VerifyOutcome::Stale;
        }
        if self.clock.now().duration_since(p.created) >= CODE_TTL {
            self.pending.remove(conn_id);
            return VerifyOutcome::Expired;
        }
        if p.code == code {
            self.pending.remove(conn_id);
            let grant = Grant {
                conn_id: conn_id.to_string(),
                req_id: req_id.to_string(),
                session_id: self.session.clone().unwrap_or_default(),
            };
            // Not authorized yet: the grant stays provisional until the
            // wire layer confirms the pair-ok reached the sink.
            self.provisional.insert(conn_id.to_string(), grant.clone());
            return VerifyOutcome::Authorized(grant);
        }
        if let Some(p) = self.pending.get_mut(conn_id) {
            p.wrong += 1;
            if p.wrong >= MAX_CODE_ATTEMPTS {
                self.pending.remove(conn_id);
            }
        }
        VerifyOutcome::WrongCode
    }

    /// Browser-side cancellation: removes ONLY the matching pending attempt.
    /// Grants from earlier pairings are untouched (explicit revocation is a
    /// later slice). A cancel matching an already-verified req id also
    /// invalidates that in-flight commit (the pair-ok write may still be in
    /// flight). Returns whether something was cancelled.
    pub fn cancel_request(&mut self, conn_id: &str, req_id: &str) -> bool {
        match self.pending.get(conn_id) {
            Some(p) if p.req_id == req_id => {
                self.pending.remove(conn_id);
                // A cancel racing the pair-ok write invalidates the commit.
                self.provisional.remove(conn_id);
                true
            }
            _ => {
                // Pending already consumed (e.g. verified): a matching cancel
                // still kills the provisional grant.
                if self
                    .provisional
                    .get(conn_id)
                    .is_some_and(|g| g.req_id == req_id)
                {
                    self.provisional.remove(conn_id);
                    return true;
                }
                false
            }
        }
    }

    /// Query for the future wire layer: is this conn authorized right now?
    pub fn authorized(&self, conn_id: &str) -> Option<&Grant> {
        self.grants.get(conn_id)
    }

    /// Commit-after-send: promotes the verified grant for `conn_id` into a
    /// live grant ONLY when the pair-ok reply provably reached the sink:
    /// same req id, the same CURRENT hub session, peer still live. Any
    /// mismatch discards the provisional (a stale commit never grants).
    pub fn commit_grant(&mut self, conn_id: &str, req_id: &str, session_id: &str) -> Option<Grant> {
        let staged = self.provisional.remove(conn_id)?;
        let valid = staged.req_id == req_id
            && staged.session_id == session_id
            && self.session.as_deref() == Some(session_id)
            && self.peers.contains_key(conn_id);
        if valid {
            self.grants.insert(conn_id.to_string(), staged.clone());
            Some(staged)
        } else {
            None
        }
    }

    /// Reply write failed: drop the provisional grant so nothing authorizes
    /// and the snapshot stays consistent (not paired).
    pub fn discard_provisional(&mut self, conn_id: &str) {
        self.provisional.remove(conn_id);
    }

    /// Read-only wire view (Slice 3b): live pendings as
    /// `(conn_id, req_id, code, created)` so the wire layer derives UI
    /// state from this hub instead of keeping a divergent mirror.
    pub fn pendings(&self) -> Vec<(String, String, String, Instant)> {
        self.pending
            .iter()
            .map(|(conn, p)| (conn.clone(), p.req_id.clone(), p.code.clone(), p.created))
            .collect()
    }

    /// Read-only wire view: live grants (bounded by the peer registry).
    pub fn grants(&self) -> Vec<Grant> {
        self.grants.values().cloned().collect()
    }

    /// Frees expired pendings and trims every peer's generation window.
    fn sweep(&mut self) {
        let now = self.clock.now();
        self.pending
            .retain(|_, p| now.duration_since(p.created) < CODE_TTL);
        let cutoff = now.checked_sub(GENERATION_WINDOW);
        for record in self.peers.values_mut() {
            record
                .generations
                .retain(|t| cutoff.map_or(true, |c| *t >= c));
        }
    }

    fn wipe(&mut self) {
        self.pending.clear();
        self.grants.clear();
        self.provisional.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    const SESS: &str = "sess-1";
    const WRONG: &str = "99999999";

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

    #[derive(Clone)]
    struct SeqGen {
        fail: Arc<AtomicBool>,
        n: Arc<Mutex<u64>>,
    }

    impl SeqGen {
        fn new() -> Self {
            Self {
                fail: Arc::new(AtomicBool::new(false)),
                n: Arc::new(Mutex::new(0)),
            }
        }
        fn set_fail(&self, fail: bool) {
            self.fail.store(fail, Ordering::SeqCst);
        }
    }

    impl CodeGen for SeqGen {
        fn generate(&mut self) -> Result<String, CodeGenError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(CodeGenError::Entropy);
            }
            let mut n = self.n.lock().unwrap();
            *n += 1;
            Ok(format!("{:08}", 10_000_000 + *n))
        }
    }

    struct Rig {
        hub: PairingHub,
        clock: FakeClock,
        gen: SeqGen,
    }

    /// Live hub: session up, w1/w2/w3 registered, deterministic seams.
    fn rig() -> Rig {
        let clock = FakeClock::started();
        let gen = SeqGen::new();
        let mut hub = PairingHub::with_seams(Box::new(clock.clone()), Box::new(gen.clone()));
        hub.on_session_started(SESS);
        for c in ["w1", "w2", "w3"] {
            assert!(hub.register_peer(c, Some("sid-hint")));
        }
        Rig { hub, clock, gen }
    }

    fn ready(rig: &mut Rig, conn: &str, req: &str) -> String {
        match rig.hub.request_code(conn, req) {
            RequestOutcome::CodeReady { code } => code,
            other => panic!("expected CodeReady for {conn}, got {other:?}"),
        }
    }

    fn granted(rig: &mut Rig) -> Grant {
        let code = ready(rig, "w1", "r1");
        match rig.hub.verify_code("w1", "r1", &code) {
            VerifyOutcome::Authorized(g) => {
                assert_eq!(
                    rig.hub.commit_grant("w1", "r1", SESS),
                    Some(g.clone()),
                    "the helper commits so the grant is live"
                );
                g
            }
            other => panic!("expected Authorized, got {other:?}"),
        }
    }

    #[test]
    fn request_issues_exactly_8_digit_code() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        assert_eq!(code.len(), 8);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn repeat_same_req_id_reuses_code_without_extending_ttl() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        r.clock.advance(Duration::from_secs(119));
        assert_eq!(ready(&mut r, "w1", "r1"), code, "repeat returns same code");
        r.clock.advance(Duration::from_secs(2));
        assert_eq!(
            r.hub.verify_code("w1", "r1", &code),
            VerifyOutcome::Expired,
            "TTL counts from the original creation, not the repeat"
        );
    }

    #[test]
    fn repeats_do_not_count_as_generations() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r0");
        for _ in 0..5 {
            assert_eq!(ready(&mut r, "w1", "r0"), code);
        }
        for i in 1..10 {
            ready(&mut r, "w1", &format!("r{i}"));
        }
        assert_eq!(
            r.hub.request_code("w1", "r11"),
            RequestOutcome::RateLimited,
            "only the 10 real generations counted"
        );
    }

    #[test]
    fn new_req_id_replaces_old_pending() {
        let mut r = rig();
        let old = ready(&mut r, "w1", "r1");
        let new = ready(&mut r, "w1", "r2");
        assert_ne!(old, new);
        assert_eq!(r.hub.verify_code("w1", "r1", &old), VerifyOutcome::Stale);
        assert_eq!(
            r.hub.verify_code("w1", "r2", &new),
            VerifyOutcome::Authorized(Grant {
                conn_id: "w1".into(),
                req_id: "r2".into(),
                session_id: SESS.into(),
            })
        );
    }

    #[test]
    fn three_wrong_codes_invalidate_the_pending() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        for _ in 0..3 {
            assert_eq!(
                r.hub.verify_code("w1", "r1", WRONG),
                VerifyOutcome::WrongCode
            );
        }
        assert_eq!(
            r.hub.verify_code("w1", "r1", &code),
            VerifyOutcome::Stale,
            "pending is gone after 3 strikes, even with the right code"
        );
    }

    #[test]
    fn two_wrong_then_correct_code_grants() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        assert_eq!(
            r.hub.verify_code("w1", "r1", WRONG),
            VerifyOutcome::WrongCode
        );
        assert_eq!(
            r.hub.verify_code("w1", "r1", WRONG),
            VerifyOutcome::WrongCode
        );
        assert!(matches!(
            r.hub.verify_code("w1", "r1", &code),
            VerifyOutcome::Authorized(_)
        ));
    }

    #[test]
    fn wrong_req_id_is_stale_and_consumes_no_attempt() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        assert_eq!(r.hub.verify_code("w1", "nope", &code), VerifyOutcome::Stale);
        assert!(matches!(
            r.hub.verify_code("w1", "r1", &code),
            VerifyOutcome::Authorized(_)
        ));
    }

    #[test]
    fn wrong_peer_cannot_consume_another_pending() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        assert_eq!(
            r.hub.verify_code("w2", "r1", &code),
            VerifyOutcome::Stale,
            "w2 has no pending; w1's attempts are untouched"
        );
        assert_eq!(
            r.hub.verify_code("w1", "r1", WRONG),
            VerifyOutcome::WrongCode
        );
        assert!(matches!(
            r.hub.verify_code("w1", "r1", &code),
            VerifyOutcome::Authorized(_)
        ));
    }

    #[test]
    fn third_peer_is_busy_when_two_pendings_are_live() {
        let mut r = rig();
        ready(&mut r, "w1", "r1");
        ready(&mut r, "w2", "r1");
        assert_eq!(r.hub.request_code("w3", "r1"), RequestOutcome::Busy);
    }

    #[test]
    fn replacing_own_pending_is_not_busy() {
        let mut r = rig();
        ready(&mut r, "w1", "r1");
        ready(&mut r, "w2", "r1");
        assert!(matches!(
            r.hub.request_code("w1", "r2"),
            RequestOutcome::CodeReady { .. }
        ));
    }

    #[test]
    fn expired_pendings_free_busy_slots() {
        let mut r = rig();
        ready(&mut r, "w1", "r1");
        ready(&mut r, "w2", "r1");
        assert_eq!(r.hub.request_code("w3", "r1"), RequestOutcome::Busy);
        r.clock.advance(CODE_TTL);
        assert!(matches!(
            r.hub.request_code("w3", "r1"),
            RequestOutcome::CodeReady { .. }
        ));
    }

    #[test]
    fn ttl_boundary_is_strict_at_120s() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        r.clock.advance(Duration::from_secs(119));
        assert!(matches!(
            r.hub.verify_code("w1", "r1", &code),
            VerifyOutcome::Authorized(_)
        ));
        let mut r2 = rig();
        let code2 = ready(&mut r2, "w1", "r1");
        r2.clock.advance(Duration::from_secs(120));
        assert_eq!(
            r2.hub.verify_code("w1", "r1", &code2),
            VerifyOutcome::Expired
        );
    }

    #[test]
    fn generator_failure_is_unavailable_with_no_pending_or_grant() {
        let mut r = rig();
        r.gen.set_fail(true);
        assert_eq!(r.hub.request_code("w1", "r1"), RequestOutcome::Unavailable);
        assert!(r.hub.authorized("w1").is_none());
        assert_eq!(
            r.hub.verify_code("w1", "r1", "12345678"),
            VerifyOutcome::Stale
        );
        r.gen.set_fail(false);
        assert!(matches!(
            r.hub.request_code("w1", "r1"),
            RequestOutcome::CodeReady { .. }
        ));
    }

    #[test]
    fn rate_limit_is_10_generations_per_peer_per_window() {
        let mut r = rig();
        for i in 0..10 {
            assert!(matches!(
                r.hub.request_code("w1", &format!("r{i}")),
                RequestOutcome::CodeReady { .. }
            ));
        }
        assert_eq!(r.hub.request_code("w1", "r10"), RequestOutcome::RateLimited);
        assert!(matches!(
            r.hub.request_code("w2", "r0"),
            RequestOutcome::CodeReady { .. }
        ));
    }

    #[test]
    fn rate_window_trims_after_600s() {
        let mut r = rig();
        for i in 0..10 {
            ready(&mut r, "w1", &format!("r{i}"));
        }
        assert_eq!(r.hub.request_code("w1", "rx"), RequestOutcome::RateLimited);
        r.clock.advance(GENERATION_WINDOW);
        assert_eq!(
            r.hub.request_code("w1", "rx"),
            RequestOutcome::RateLimited,
            "an entry exactly 600s old is still inside the window"
        );
        r.clock.advance(Duration::from_millis(1));
        assert!(matches!(
            r.hub.request_code("w1", "rx"),
            RequestOutcome::CodeReady { .. }
        ));
    }

    #[test]
    fn request_without_live_session_is_unavailable() {
        let clock = FakeClock::started();
        let gen = SeqGen::new();
        let mut hub = PairingHub::with_seams(Box::new(clock), Box::new(gen));
        assert!(hub.register_peer("w1", None));
        assert_eq!(hub.request_code("w1", "r1"), RequestOutcome::Unavailable);
        hub.on_session_started(SESS);
        assert!(matches!(
            hub.request_code("w1", "r1"),
            RequestOutcome::CodeReady { .. }
        ));
    }

    #[test]
    fn unregistered_peer_or_invalid_req_id_is_unavailable() {
        let mut r = rig();
        assert_eq!(r.hub.request_code("wX", "r1"), RequestOutcome::Unavailable);
        for bad in ["", &"a".repeat(65), "bad id!"] {
            assert_eq!(
                r.hub.request_code("w1", bad),
                RequestOutcome::Unavailable,
                "req id {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn registration_is_capped_at_32_peers() {
        let hub = PairingHub::new();
        let mut hub = hub;
        hub.on_session_started(SESS);
        for i in 0..32 {
            assert!(hub.register_peer(&format!("p{i}"), None));
        }
        assert!(!hub.register_peer("p32", None));
    }

    #[test]
    fn session_reset_invalidates_pending_and_grants() {
        let mut r = rig();
        granted(&mut r);
        ready(&mut r, "w2", "r1");
        r.hub.on_session_started("sess-2");
        assert!(r.hub.authorized("w1").is_none());
        assert_eq!(
            r.hub.verify_code("w2", "r1", "11111111"),
            VerifyOutcome::Stale
        );
    }

    #[test]
    fn link_down_clears_grants_pending_and_session() {
        let mut r = rig();
        granted(&mut r);
        r.hub.on_link_down();
        assert!(r.hub.authorized("w1").is_none());
        assert_eq!(r.hub.request_code("w1", "r2"), RequestOutcome::Unavailable);
        r.hub.on_session_started(SESS);
        assert!(matches!(
            r.hub.request_code("w1", "r2"),
            RequestOutcome::CodeReady { .. }
        ));
    }

    #[test]
    fn peer_leave_drops_grant_and_sid_return_does_not_resurrect_it() {
        let mut r = rig();
        granted(&mut r);
        r.hub.remove_peer("w1");
        assert!(r.hub.authorized("w1").is_none());
        assert!(r.hub.register_peer("w1", Some("sid-hint")));
        assert!(
            r.hub.authorized("w1").is_none(),
            "same sid returning must not resurrect the grant"
        );
    }

    #[test]
    fn cancel_removes_only_matching_pending_and_keeps_grant() {
        let mut r = rig();
        granted(&mut r);
        let code2 = ready(&mut r, "w1", "r2");
        assert!(!r.hub.cancel_request("w1", "other"), "mismatch is a no-op");
        assert!(matches!(
            r.hub.verify_code("w1", "r2", &code2),
            VerifyOutcome::Authorized(_)
        ));
        let current = r.hub.authorized("w1").cloned().unwrap();
        ready(&mut r, "w1", "r3");
        assert!(r.hub.cancel_request("w1", "r3"));
        assert_eq!(
            r.hub.verify_code("w1", "r3", "11111111"),
            VerifyOutcome::Stale
        );
        assert_eq!(
            r.hub.authorized("w1"),
            Some(&current),
            "cancellation never touches existing grants"
        );
    }

    // ---- commit-after-send (provisional grants) ----

    #[test]
    fn verified_grant_is_not_authorized_until_committed() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        assert!(matches!(
            r.hub.verify_code("w1", "r1", &code),
            VerifyOutcome::Authorized(_)
        ));
        assert!(
            r.hub.authorized("w1").is_none(),
            "a verified grant must not authorize before the pair-ok write"
        );
        assert!(r.hub.grants().is_empty());
        let grant = Grant {
            conn_id: "w1".into(),
            req_id: "r1".into(),
            session_id: SESS.into(),
        };
        assert_eq!(r.hub.commit_grant("w1", "r1", SESS), Some(grant.clone()));
        assert_eq!(r.hub.authorized("w1"), Some(&grant));
    }

    #[test]
    fn commit_is_rejected_for_wrong_req_dead_peer_or_session_mismatch() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        r.hub.verify_code("w1", "r1", &code);
        assert_eq!(
            r.hub.commit_grant("w1", "other", SESS),
            None,
            "req id must match the verified pending"
        );
        assert_eq!(
            r.hub.commit_grant("w1", "r1", "other-session"),
            None,
            "caller session must match the verified session"
        );
        assert_eq!(
            r.hub.commit_grant("w1", "r1", SESS),
            None,
            "a failed commit discards the provisional: one shot only"
        );

        let mut r2 = rig();
        let code2 = ready(&mut r2, "w1", "r1");
        r2.hub.verify_code("w1", "r1", &code2);
        r2.hub.remove_peer("w1");
        assert_eq!(
            r2.hub.commit_grant("w1", "r1", SESS),
            None,
            "a departed peer's commit is stale"
        );

        let mut r3 = rig();
        let code3 = ready(&mut r3, "w1", "r1");
        r3.hub.verify_code("w1", "r1", &code3);
        r3.hub.on_session_started("sess-2");
        assert_eq!(
            r3.hub.commit_grant("w1", "r1", SESS),
            None,
            "a new hub session invalidates the pending commit"
        );
        // Pairing under the new session still works from scratch.
        assert!(matches!(
            r3.hub.request_code("w1", "r2"),
            RequestOutcome::CodeReady { .. }
        ));
    }

    #[test]
    fn new_request_or_cancel_after_verify_invalidates_the_pending_commit() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        r.hub.verify_code("w1", "r1", &code);
        // The peer restarts pairing while the pair-ok reply is in flight.
        assert!(matches!(
            r.hub.request_code("w1", "r2"),
            RequestOutcome::CodeReady { .. }
        ));
        assert_eq!(
            r.hub.commit_grant("w1", "r1", SESS),
            None,
            "a new request supersedes the in-flight verify"
        );

        let mut r2 = rig();
        let code2 = ready(&mut r2, "w1", "r1");
        r2.hub.verify_code("w1", "r1", &code2);
        assert!(r2.hub.cancel_request("w1", "r1"), "matching cancel wins");
        assert_eq!(
            r2.hub.commit_grant("w1", "r1", SESS),
            None,
            "a cancel racing the write invalidates the commit"
        );
    }

    #[test]
    fn discard_provisional_drops_pending_commit_without_granting() {
        let mut r = rig();
        let code = ready(&mut r, "w1", "r1");
        r.hub.verify_code("w1", "r1", &code);
        r.hub.discard_provisional("w1");
        assert!(r.hub.authorized("w1").is_none(), "rollback never grants");
        assert!(r.hub.grants().is_empty());
        assert_eq!(r.hub.commit_grant("w1", "r1", SESS), None);
        // The conn can pair again from scratch.
        assert!(matches!(
            r.hub.request_code("w1", "r2"),
            RequestOutcome::CodeReady { .. }
        ));
    }

    #[test]
    fn system_code_gen_shapes_uniform_8_digit_codes() {
        let mut gen = SystemCodeGen;
        for _ in 0..200 {
            let code = gen.generate().unwrap();
            assert_eq!(code.len(), 8);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
    }
}
