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
/// Message-key prefix for hub contacts: a paired web peer's history key is
/// `hub:<uuid>` with the uuid minted at grant-commit time.
pub const HUB_KEY_PREFIX: &str = "hub:";

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

#[derive(Clone, Debug)]
pub enum Accept {
    New,
    Duplicate,
}

/// Explicit delivery-state ordering: a patch may advance a row's state but
/// never move it backwards (None < "pending" < "sent" < "delivered";
/// unknown values rank lowest so a known transition always wins over them,
/// and they can never win over a known one).
fn state_rank(s: Option<&str>) -> u8 {
    match s {
        None => 0,
        Some("pending") => 1,
        Some("sent") => 2,
        Some("delivered") => 3,
        Some(_) => 0,
    }
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
    match conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    }) {
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

/// Idempotent schema migration for the append model: additive `content_hash`
/// column + scoped uniqueness index. Safe to run on every startup — it only
/// adds what is missing and never writes app data. Legacy snapshot rows stay
/// valid (`content_hash` NULL → dedup falls back to `text` equality).
/// This migration deliberately does NOT touch `history_legacy_imported`:
/// that flag marks "localStorage snapshot imported" and is set exclusively
/// by `import_legacy`, in the same transaction as the imported rows.
pub fn ensure_history_schema(conn: &Connection) -> Result<(), String> {
    // Idempotent existence check: a "duplicate column" failure is a plain
    // SQLITE_ERROR with no dedicated code, so the column is probed instead.
    let has_hash_col: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'content_hash'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if has_hash_col == 0 {
        conn.execute("ALTER TABLE messages ADD COLUMN content_hash TEXT", [])
            .map_err(|e| e.to_string())?;
    }
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_key_id
         ON messages(device_key, id)",
        [],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())?;
    // Additive contacts metadata for hub-paired peers. Schema-only here:
    // rows are written exclusively by `upsert_contact` at grant-commit.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS hub_contacts (
            uuid TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
        [],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Stores (or renames) one hub contact. Contacts are metadata, not messages:
/// `INSERT OR REPLACE` is correct here (messages keep the INSERT-only rule).
pub fn upsert_contact(conn: &Connection, uuid: &str, name: &str) -> Result<(), String> {
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT OR REPLACE INTO hub_contacts (uuid, name, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![uuid, name, created_at],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// All stored contacts as `hub:<uuid>` -> display name (the history keys the
/// frontend uses for hub conversations).
pub fn load_contacts(conn: &Connection) -> Result<HashMap<String, String>, String> {
    let mut stmt = conn
        .prepare("SELECT uuid, name FROM hub_contacts")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut map = HashMap::new();
    for row in rows {
        let (uuid, name) = row.map_err(|e| e.to_string())?;
        map.insert(format!("{HUB_KEY_PREFIX}{uuid}"), name);
    }
    Ok(map)
}

/// DISTINCT non-NULL `file_path` values referenced by history rows. Used at
/// startup to re-grant asset-scope permissions (grants live in memory only,
/// so without this every history image breaks after a restart). Best-effort
/// read: an unusable `messages` table yields an empty vec.
pub fn file_paths(conn: &Connection) -> Vec<String> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT file_path FROM messages WHERE file_path IS NOT NULL",
    ) else {
        return Vec::new();
    };
    stmt.query_map([], |row| row.get::<_, String>(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// One-time import of the frontend localStorage snapshot: flag check, schema
/// migration, one `INSERT OR IGNORE` per row, and the flag itself — all in a
/// single transaction, so the flag can only ever land together with the rows
/// it vouches for. Returns `Ok(false)` when the flag is already set (honest
/// no-op: the import never runs twice, a re-run after deletes would
/// mislabel rows).
pub fn import_legacy(
    conn: &mut Connection,
    history: &HashMap<String, Vec<HistoryEntry>>,
) -> Result<bool, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    if has_flag(&tx, LEGACY_FLAG)? {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(false);
    }
    ensure_history_schema(&tx)?;
    for (device_key, entries) in history {
        for e in entries {
            tx.execute(
                "INSERT OR IGNORE INTO messages
                 (id, device_key, mine, text, at, state, file_path, read, content_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
                rusqlite::params![
                    e.id,
                    device_key,
                    e.mine,
                    e.text,
                    e.at,
                    e.state,
                    e.file_path,
                    e.read
                ],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, '1')",
        [LEGACY_FLAG],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(true)
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

/// Patches ONLY the delivery/read columns, gated by the token; content and
/// file_path are never touched and nothing is reinserted. A stale/out-of-order
/// token is rejected. A backward state transition per the explicit delivery
/// ordering is never applied (the read flag still applies). Returns
/// Ok(false) for a nonexistent row — an honest no-op, never faked success.
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
    let found: Option<Option<String>> = tx
        .query_row(
            "SELECT state FROM messages WHERE device_key = ?1 AND id = ?2",
            rusqlite::params![device_key, id],
            |r| r.get::<_, Option<String>>(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other.to_string()),
        })?;
    let Some(current) = found else {
        tx.commit().map_err(|e| e.to_string())?; // read-only branch
        return Ok(false);
    };
    let next: Option<String> = if state_rank(new_state) < state_rank(current.as_deref()) {
        current // no downgrade: keep the higher delivery state
    } else {
        new_state.map(str::to_string)
    };
    let n = tx
        .execute(
            "UPDATE messages SET state = ?1, read = ?2
             WHERE device_key = ?3 AND id = ?4",
            rusqlite::params![next, read, device_key, id],
        )
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(n > 0)
}

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
    tx.execute("DELETE FROM messages", [])
        .map_err(|e| e.to_string())?;
    let epoch = hist_epoch(&tx)? + 1;
    set_num(&tx, EPOCH_KEY, epoch)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(epoch)
}

/// Inbound acceptance record: one transaction, INSERT only — never
/// `INSERT OR REPLACE`. RESERVED FOR HUB-INBOUND message acceptance; the LAN
/// path has no current caller for it — kept as the designed entry point for
/// the hub inbound flow. NOT token-gated: caller tokens play no role here; a
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
        let conn = temp_db("migr");
        conn.execute(
            "INSERT INTO messages (id, device_key, mine, text, at, state, file_path, read)
             VALUES ('l1', 'k', 1, 'legacy', 1, 'sent', 'files/legacy.bin', 0)",
            [],
        )
        .unwrap();
        ensure_history_schema(&conn).unwrap();
        ensure_history_schema(&conn).unwrap(); // second run: no-op, no error
        let (text, at, state, file_path, hash): (
            String,
            i64,
            String,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT text, at, state, file_path, content_hash FROM messages WHERE id = 'l1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(text, "legacy");
        assert_eq!(at, 1);
        assert_eq!(state, "sent");
        assert_eq!(
            file_path.as_deref(),
            Some("files/legacy.bin"),
            "legacy file_path is preserved byte-identical by the migration"
        );
        assert!(hash.is_none(), "legacy rows keep NULL content_hash");
        // The schema migration must NOT set the legacy-import flag: that flag
        // belongs to history_import_legacy only, or the one-time localStorage
        // import is dead on arrival.
        assert!(!has_flag(&conn, LEGACY_FLAG).unwrap());
    }

    #[test]
    fn import_legacy_inserts_rows_sets_flag_then_second_import_is_noop() {
        let mut conn = temp_db("imp");
        ensure_history_schema(&conn).unwrap();
        assert!(!has_flag(&conn, LEGACY_FLAG).unwrap());
        let mut e1 = entry("l1", "hola");
        e1.file_path = Some("files/l1.bin".into());
        let mut map = HashMap::new();
        map.insert("k".to_string(), vec![e1, entry("l2", "chau")]);
        assert!(
            import_legacy(&mut conn, &map).unwrap(),
            "first import imports"
        );
        assert!(
            has_flag(&conn, LEGACY_FLAG).unwrap(),
            "flag set in the same tx"
        );
        assert_eq!(count_rows(&conn), 2);
        let (text, file_path, mine, hash): (String, Option<String>, bool, Option<String>) = conn
            .query_row(
                "SELECT text, file_path, mine, content_hash FROM messages WHERE id = 'l1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(text, "hola");
        assert_eq!(file_path.as_deref(), Some("files/l1.bin"));
        assert!(mine);
        assert!(hash.is_none(), "imported rows keep NULL content_hash");
        // Second import: flag set -> honest no-op, no duplicate rows.
        assert!(!import_legacy(&mut conn, &map).unwrap());
        assert_eq!(count_rows(&conn), 2, "no duplicates on re-import");
    }

    #[test]
    fn import_legacy_empty_map_still_sets_flag_once() {
        let mut conn = temp_db("impempty");
        ensure_history_schema(&conn).unwrap();
        assert!(import_legacy(&mut conn, &HashMap::new()).unwrap());
        assert!(
            has_flag(&conn, LEGACY_FLAG).unwrap(),
            "empty import still sets the flag"
        );
        assert_eq!(count_rows(&conn), 0);
        assert!(
            !import_legacy(&mut conn, &HashMap::new()).unwrap(),
            "second run is a no-op"
        );
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
        let mut e = entry("m1", "hola");
        e.file_path = Some("keep/me.bin".into());
        append_message(&mut conn, &e, "k", t0).unwrap();
        assert!(patch_message_state(&mut conn, "k", "m1", Some("sent"), false, t0).unwrap());
        let (text, file_path, state): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT text, file_path, state FROM messages WHERE id = 'm1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(text, "hola"); // a patch never rewrites content
        assert_eq!(file_path.as_deref(), Some("keep/me.bin")); // nor file_path
        assert_eq!(state.as_deref(), Some("sent"));
        // Forward transition is accepted...
        assert!(patch_message_state(&mut conn, "k", "m1", Some("delivered"), false, t0).unwrap());
        // ...but an out-of-order older transition must NOT downgrade.
        assert!(patch_message_state(&mut conn, "k", "m1", Some("sent"), true, t0).unwrap());
        let (state, read): (Option<String>, bool) = conn
            .query_row(
                "SELECT state, read FROM messages WHERE id = 'm1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state.as_deref(), Some("delivered"));
        assert!(read); // the read flag still applies on a blocked downgrade
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

    #[test]
    fn delete_all_epoch_rejects_stale_even_for_empty_keys() {
        let mut conn = temp_db("epoch");
        ensure_history_schema(&conn).unwrap();
        let ta = token(&conn, "a");
        append_message(&mut conn, &entry("m1", "hola"), "a", ta).unwrap();
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
        let tb = token(&conn, "b");
        append_message(&mut conn, &entry("m3", "y"), "b", tb).unwrap();
        // Stale patches are rejected too, not just appends.
        assert_eq!(
            patch_message_state(&mut conn, "a", "m1", Some("sent"), false, old).unwrap_err(),
            "stale-history"
        );
    }

    #[test]
    fn delete_all_bumps_epoch_and_preserves_settings() {
        let mut conn = temp_db("pres");
        ensure_history_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('pin', '1234'), ('device_id', 'D1')",
            [],
        )
        .unwrap();
        let tk = token(&conn, "k");
        append_message(&mut conn, &entry("m1", "x"), "k", tk).unwrap();
        delete_conversation(&mut conn, "k").unwrap(); // rev k -> 1
        assert_eq!(delete_all_history(&mut conn).unwrap(), 1);
        assert_eq!(hist_epoch(&conn).unwrap(), 1);
        assert_eq!(count_rows(&conn), 0);
        assert!(
            !has_flag(&conn, LEGACY_FLAG).unwrap(),
            "delete_all never sets the legacy flag; only the import does"
        );
        let pin: String = conn
            .query_row("SELECT value FROM settings WHERE key = 'pin'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(pin, "1234");
        let dev: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'device_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dev, "D1");
        // Plan semantics: per-key rev metadata survives delete_all; the
        // bumped global epoch alone is what invalidates every old token.
        assert_eq!(hist_rev(&conn, "k").unwrap(), 1);
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
        let ta = token(&conn, "a");
        let tb = token(&conn, "b");
        append_message(&mut conn, &ea, "a", ta).unwrap();
        append_message(&mut conn, &eb, "b", tb).unwrap();

        delete_conversation(&mut conn, "a").unwrap();
        assert_eq!(
            std::fs::read(&fa).unwrap(),
            a_bytes,
            "per-key delete keeps bytes"
        );
        assert!(fa.exists() && fa.parent().unwrap().is_dir());
        // The other conversation's rows survive a per-key delete.
        assert_eq!(count_rows(&conn), 1);

        delete_all_history(&mut conn).unwrap();
        assert_eq!(
            std::fs::read(&fa).unwrap(),
            a_bytes,
            "delete_all keeps bytes"
        );
        assert_eq!(
            std::fs::read(&fb).unwrap(),
            b_bytes,
            "delete_all keeps bytes"
        );
        assert!(fa.exists() && fb.exists());
        assert_eq!(count_rows(&conn), 0);
    }

    #[test]
    fn same_rev_out_of_order_sequence_no_state_regression() {
        let mut conn = temp_db("seq");
        ensure_history_schema(&conn).unwrap();
        let t = token(&conn, "k");
        // Appends and patches share one rev; none of them bump it.
        append_message(&mut conn, &entry("a1", "1"), "k", t).unwrap();
        append_message(&mut conn, &entry("a2", "2"), "k", t).unwrap();
        assert!(patch_message_state(&mut conn, "k", "a1", Some("sent"), false, t).unwrap());
        // Late-arriving ack upgrades cleanly...
        assert!(patch_message_state(&mut conn, "k", "a1", Some("delivered"), false, t).unwrap());
        // ...and a replayed older ack causes no state regression on a1...
        assert!(patch_message_state(&mut conn, "k", "a1", Some("sent"), false, t).unwrap());
        // ...nor on the untouched sibling row.
        assert!(patch_message_state(&mut conn, "k", "a2", Some("sent"), true, t).unwrap());
        let (s1, r1): (Option<String>, bool) = conn
            .query_row(
                "SELECT state, read FROM messages WHERE id = 'a1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let (s2, r2): (Option<String>, bool) = conn
            .query_row(
                "SELECT state, read FROM messages WHERE id = 'a2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(s1.as_deref(), Some("delivered"));
        assert!(!r1);
        assert_eq!(s2.as_deref(), Some("sent"));
        assert!(r2);
        // The token was never invalidated by appends or patches.
        assert_eq!(token(&conn, "k"), t);
    }

    #[test]
    fn hub_contacts_upsert_and_load_maps_prefixed_keys() {
        let conn = temp_db("contacts");
        ensure_history_schema(&conn).unwrap();
        assert!(load_contacts(&conn).unwrap().is_empty());
        upsert_contact(&conn, "u-1", "Ana").unwrap();
        upsert_contact(&conn, "u-2", "Beto").unwrap();
        let map = load_contacts(&conn).unwrap();
        assert_eq!(map.get("hub:u-1").map(String::as_str), Some("Ana"));
        assert_eq!(map.get("hub:u-2").map(String::as_str), Some("Beto"));
        // Re-upsert replaces the display name (metadata, not a message row).
        upsert_contact(&conn, "u-1", "Ana2").unwrap();
        let map = load_contacts(&conn).unwrap();
        assert_eq!(map.get("hub:u-1").map(String::as_str), Some("Ana2"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn file_paths_collects_distinct_non_null_paths() {
        let conn = temp_db("paths");
        ensure_history_schema(&conn).unwrap();
        assert!(
            file_paths(&conn).is_empty(),
            "empty DB yields an empty vec"
        );
        // Two rows share 'files/a.bin' (DISTINCT collapses them); NULL
        // file_path rows are skipped entirely.
        for (id, fp) in [
            ("m1", Some("files/a.bin")),
            ("m2", Some("files/a.bin")),
            ("m3", Some("files/b.bin")),
            ("m4", None),
        ] {
            conn.execute(
                "INSERT INTO messages (id, device_key, mine, text, at, state, file_path, read)
                 VALUES (?1, 'k', 1, 'x', 1, NULL, ?2, 0)",
                rusqlite::params![id, fp],
            )
            .unwrap();
        }
        let mut paths = file_paths(&conn);
        paths.sort();
        assert_eq!(paths, vec!["files/a.bin", "files/b.bin"]);
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
}
