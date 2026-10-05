// Focused presence module: parsing, bounds and normalization for hub
// "peers" snapshots. Pure — no shared state, no I/O — so every rule here is
// unit-testable without a socket.
//
// Contract (mirrors the hub's web presence rules):
// - A peer entry is valid only with a non-empty conn id (<= 64 chars) and
//   kind "web"; app/relay kinds are deferred and skipped for now.
// - `sid` is an UNTRUSTED HINT: 1..=64 chars of [A-Za-z0-9_-]. Missing or
//   invalid → `None` (the peer still shows, marked as legacy/incompatible by
//   the UI). A sid never authorizes anything and duplicate sids are listed
//   as separate peers keyed by conn id — no merging, no global-history
//   security guarantee.
// - `caps` is bounded: at most 8 string items of 1..=32 chars; anything
//   malformed degrades to an empty list, never a panic.
// - Invalid entries are skipped individually; one bad row never kills the
//   whole snapshot. The snapshot itself is capped at MAX_PEERS entries.

use serde::Serialize;

pub const MAX_PEERS: usize = 32;
pub const MAX_CONN_ID_LEN: usize = 64;
pub const MAX_PEER_NAME_LEN: usize = 48;
pub const MAX_SID_LEN: usize = 64;
pub const MAX_CAPS: usize = 8;
pub const MAX_CAP_LEN: usize = 32;

/// One peer in a presence snapshot, keyed by its hub connection id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HubPeer {
    pub conn_id: String,
    /// Display name; may be empty. Never an identity guarantee.
    pub name: String,
    /// Always "web" for now; app/relay peers are deferred.
    pub kind: String,
    /// Untrusted stable-id hint; absent for legacy or malformed announcements.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    /// Bounded capability list; empty means none advertised.
    pub caps: Vec<String>,
}

/// Full normalized presence snapshot; replaces the previous one wholesale.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HubPresenceSnapshot {
    pub peers: Vec<HubPeer>,
}

impl HubPresenceSnapshot {
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

/// sid validity, mirroring the hub: 1..=64 chars, [A-Za-z0-9_-] only
/// (UUIDs with hyphens are valid).
pub fn is_valid_sid(sid: &str) -> bool {
    sid.len() >= 1
        && sid.len() <= MAX_SID_LEN
        && sid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Char-safe truncation for display strings.
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Parses `{"type":"peers","list":[...]}` into a bounded normalized snapshot.
/// `self_conn_id` (our hub connection id) is filtered out of the list.
/// Returns `None` for garbage payloads (caller ignores); invalid entries are
/// skipped without aborting the loop.
pub fn parse_peers(
    value: &serde_json::Value,
    self_conn_id: Option<&str>,
) -> Option<HubPresenceSnapshot> {
    if value.get("type")?.as_str()? != "peers" {
        return None;
    }
    let list = value.get("list")?.as_array()?;
    let mut peers = Vec::new();
    for entry in list {
        if let Some(peer) = parse_peer(entry, self_conn_id) {
            peers.push(peer);
            if peers.len() >= MAX_PEERS {
                break;
            }
        }
    }
    Some(HubPresenceSnapshot { peers })
}

/// Parses one peer entry; `None` means "skip this entry", never "crash".
fn parse_peer(entry: &serde_json::Value, self_conn_id: Option<&str>) -> Option<HubPeer> {
    let obj = entry.as_object()?;
    let conn_id = obj.get("id")?.as_str()?;
    if conn_id.is_empty() || conn_id.len() > MAX_CONN_ID_LEN {
        return None;
    }
    if Some(conn_id) == self_conn_id {
        return None; // self filter: our own connection is not a "peer"
    }
    let kind = obj.get("kind")?.as_str()?;
    if kind != "web" {
        return None; // app/relay peers deferred
    }
    let name = match obj.get("name").and_then(|n| n.as_str()) {
        Some(n) => truncate_chars(n, MAX_PEER_NAME_LEN),
        None => String::new(),
    };
    let sid = match obj.get("sid").and_then(|s| s.as_str()) {
        Some(s) if is_valid_sid(s) => Some(s.to_string()),
        _ => None,
    };
    let caps = parse_caps(obj.get("caps"));
    Some(HubPeer {
        conn_id: conn_id.to_string(),
        name,
        kind: kind.to_string(),
        sid,
        caps,
    })
}

/// Bounded caps: valid string items only, at most MAX_CAPS of them.
fn parse_caps(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|c| c.as_str())
        .map(str::trim)
        .filter(|c| !c.is_empty() && c.chars().count() <= MAX_CAP_LEN)
        .take(MAX_CAPS)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(list: serde_json::Value) -> Option<HubPresenceSnapshot> {
        parse_peers(&json!({ "type": "peers", "list": list }), None)
    }

    fn parse_self(list: serde_json::Value, self_id: &str) -> Option<HubPresenceSnapshot> {
        parse_peers(&json!({ "type": "peers", "list": list }), Some(self_id))
    }

    #[test]
    fn valid_entry_fully_parsed() {
        let snap = parse(json!([{
            "id": "abc123def456",
            "name": "Ana",
            "kind": "web",
            "sid": "0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f",
            "caps": ["chat-v1"],
        }]))
        .unwrap();
        assert_eq!(snap.peers.len(), 1);
        let p = &snap.peers[0];
        assert_eq!(p.conn_id, "abc123def456");
        assert_eq!(p.name, "Ana");
        assert_eq!(p.kind, "web");
        assert_eq!(
            p.sid.as_deref(),
            Some("0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f")
        );
        assert_eq!(p.caps, vec!["chat-v1"]);
    }

    #[test]
    fn garbage_payloads_yield_none_not_panic() {
        assert!(parse_peers(&json!(null), None).is_none());
        assert!(parse_peers(&json!("peers"), None).is_none());
        assert!(parse_peers(&json!({}), None).is_none());
        assert!(parse_peers(&json!({ "type": "welcome" }), None).is_none());
        assert!(parse_peers(&json!({ "type": "peers" }), None).is_none());
        assert!(parse_peers(&json!({ "type": "peers", "list": 42 }), None).is_none());
        assert!(parse_peers(&json!({ "type": "peers", "list": "x" }), None).is_none());
    }

    #[test]
    fn invalid_entries_are_skipped_others_kept() {
        let snap = parse(json!([
            42,
            "nope",
            {},
            { "kind": "web" },                      // no id
            { "id": "", "kind": "web" },            // empty id
            { "id": "x".repeat(65), "kind": "web" }, // id too long
            { "id": 7, "kind": "web" },             // mistyped id
            { "id": "good1", "kind": "web" },
        ]))
        .unwrap();
        assert_eq!(snap.peers.len(), 1);
        assert_eq!(snap.peers[0].conn_id, "good1");
    }

    #[test]
    fn only_web_kind_is_surfaced() {
        let snap = parse(json!([
            { "id": "w1", "name": "Web", "kind": "web" },
            { "id": "a1", "name": "App", "kind": "app" },
            { "id": "r1", "name": "Relay", "kind": "relay" },
            { "id": "n1", "name": "None" },
        ]))
        .unwrap();
        assert_eq!(snap.peers.len(), 1);
        assert_eq!(snap.peers[0].conn_id, "w1");
    }

    #[test]
    fn self_connection_is_filtered_out() {
        let snap = parse_self(
            json!([
                { "id": "mine", "name": "Yo", "kind": "web" },
                { "id": "other", "name": "Otro", "kind": "web" },
            ]),
            "mine",
        )
        .unwrap();
        assert_eq!(snap.peers.len(), 1);
        assert_eq!(snap.peers[0].conn_id, "other");
    }

    #[test]
    fn missing_sid_stays_none_never_invented() {
        let snap = parse(json!([{ "id": "legacy", "name": "Viejo", "kind": "web" }])).unwrap();
        assert_eq!(snap.peers[0].sid, None, "legacy peer shows without a sid");
    }

    #[test]
    fn invalid_sids_are_omitted_but_entry_stays() {
        let snap = parse(json!([
            { "id": "largo", "name": "L", "kind": "web", "sid": "a".repeat(65) },
            { "id": "raro", "name": "R", "kind": "web", "sid": "bad sid!" },
            { "id": "tipo", "name": "T", "kind": "web", "sid": 123 },
            { "id": "ok64", "name": "O", "kind": "web", "sid": "a".repeat(64) },
        ]))
        .unwrap();
        assert!(snap
            .peers
            .iter()
            .all(|p| p.conn_id != "tipo" || p.sid.is_none()));
        assert_eq!(
            snap.peers
                .iter()
                .find(|p| p.conn_id == "largo")
                .unwrap()
                .sid,
            None
        );
        assert_eq!(
            snap.peers.iter().find(|p| p.conn_id == "raro").unwrap().sid,
            None
        );
        assert_eq!(
            snap.peers.iter().find(|p| p.conn_id == "ok64").unwrap().sid,
            Some("a".repeat(64))
        );
    }

    #[test]
    fn caps_are_bounded_and_sanitized() {
        let snap = parse(json!([{
            "id": "c",
            "name": "C",
            "kind": "web",
            "caps": ["a", 42, "", "   ", "x".repeat(33), "b", "c", "d", "e", "f", "g", "h", "i"],
        }]))
        .unwrap();
        assert_eq!(
            snap.peers[0].caps,
            vec!["a", "b", "c", "d", "e", "f", "g", "h"]
        );
    }

    #[test]
    fn malformed_caps_degrade_to_empty() {
        let snap = parse(json!([
            { "id": "s", "name": "S", "kind": "web", "caps": "nope" },
            { "id": "n", "name": "N", "kind": "web" },
        ]))
        .unwrap();
        assert!(snap.peers[0].caps.is_empty());
        assert!(snap.peers[1].caps.is_empty());
    }

    #[test]
    fn duplicate_sids_stay_separate_peers() {
        let snap = parse(json!([
            { "id": "one", "name": "Uno", "kind": "web", "sid": "same" },
            { "id": "two", "name": "Dos", "kind": "web", "sid": "same" },
        ]))
        .unwrap();
        assert_eq!(
            snap.peers.len(),
            2,
            "no merge: conn id is the key, sid is a hint"
        );
    }

    #[test]
    fn snapshot_is_capped_at_max_peers() {
        let list: Vec<_> = (0..MAX_PEERS + 5)
            .map(|i| json!({ "id": format!("p{i}"), "name": "P", "kind": "web" }))
            .collect();
        let snap = parse(serde_json::Value::Array(list)).unwrap();
        assert_eq!(snap.peers.len(), MAX_PEERS);
    }

    #[test]
    fn long_names_are_truncated_and_missing_names_are_empty() {
        let snap = parse(json!([
            { "id": "n1", "name": "ñ".repeat(60), "kind": "web" },
            { "id": "n2", "kind": "web" },
        ]))
        .unwrap();
        assert_eq!(snap.peers[0].name.chars().count(), MAX_PEER_NAME_LEN);
        assert_eq!(snap.peers[1].name, "");
    }

    #[test]
    fn empty_snapshot_is_valid_and_serializes_with_empty_peers() {
        let snap = parse(json!([])).unwrap();
        assert!(snap.is_empty());
        let v = serde_json::to_value(&snap).unwrap();
        assert_eq!(v, json!({ "peers": [] }));
    }

    #[test]
    fn snapshot_serialization_skips_missing_sid_and_keeps_bounded_shape() {
        let snap = parse(json!([
            { "id": "w1", "name": "Ana", "kind": "web", "sid": "sid-1", "caps": ["chat-v1"] },
            { "id": "w2", "name": "", "kind": "web" },
        ]))
        .unwrap();
        let v = serde_json::to_value(&snap).unwrap();
        assert_eq!(v["peers"][0]["conn_id"], "w1");
        assert_eq!(v["peers"][0]["sid"], "sid-1");
        assert_eq!(v["peers"][0]["caps"], json!(["chat-v1"]));
        assert!(
            v["peers"][1].get("sid").is_none(),
            "missing sid is omitted, not null"
        );
        assert_eq!(v["peers"][1]["caps"], json!([]));
    }
}
