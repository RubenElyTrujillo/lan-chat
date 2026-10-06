use rusqlite::Connection;

/// Resolves the hub URL from `LANCHAT_HUB_URL` (trimmed, non-empty). `None`
/// means no hub is configured: public builds run LAN-only by default and an
/// operator opts in per machine via the env var (private hub deployments).
pub fn resolve_hub_url(env_value: Option<&str>) -> Option<String> {
    env_value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
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
        .query_row(
            "SELECT value FROM settings WHERE key = 'device_id'",
            [],
            |r| r.get(0),
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_url_is_none_by_default_no_baked_endpoint() {
        assert_eq!(resolve_hub_url(None), None, "public default is LAN-only");
        assert_eq!(
            resolve_hub_url(Some("   ")),
            None,
            "whitespace-only counts as unset"
        );
    }

    #[test]
    fn env_override_wins_and_trims() {
        assert_eq!(
            resolve_hub_url(Some("  ws://localhost:8788/hub ")),
            Some("ws://localhost:8788/hub".to_string())
        );
    }

    #[test]
    fn uuid_format_is_v4() {
        let u = uuid_from_random([0u8; 16]);
        assert_eq!(u, "00000000-0000-4000-8000-000000000000");
        let u2 = new_uuid().unwrap();
        assert_eq!(u2.len(), 36);
        assert_eq!(u2.as_bytes()[14], b'4');
        assert!(matches!(u2.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
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

    #[test]
    fn missing_settings_table_returns_error_not_fake_id() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let result = read_or_create_device_id(&conn);
        assert!(
            result.is_err(),
            "must surface a Result error, never mint a fake identity"
        );
    }
}
