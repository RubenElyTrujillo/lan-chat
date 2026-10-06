// Hub inbox: inbound chat authorization + acceptance (Slice 4b).
//
// Contract:
// - A chat relay frame is accepted ONLY when the payload is
//   `{type:"chat", text:<string>, id?:<string>}`, the sender is a LIVE web
//   peer of the CURRENT presence snapshot, the server-stamped `from_sid` is
//   PRESENT and matches the registered sid for that conn, a grant for the
//   conn is live under the CURRENT hub session, and the conn has a minted
//   contact key (`hub:<native-uuid>`).
// - Anything else is dropped: no row, no event, no reply.
// - The payload id is reused only when it is a non-empty string of at most
//   MAX_MSG_ID_LEN chars; anything else is minted via the injected
//   generator (never trusted wire data).
// - Acceptance goes through `history::accept_text` (INSERT-only, dedup by
//   id + sha256 of the text bytes). `New` -> the row is committed BEFORE
//   the emit hook runs (the hook receives the same connection so callers
//   can prove persist-before-emit). `Duplicate` -> no event, no row.
//   `Conflict` -> the caller logs it; the original row stays intact.

use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use super::presence::HubPeer;

/// Max accepted wire message id length; anything longer is minted fresh.
pub const MAX_MSG_ID_LEN: usize = 128;

/// Max accepted chat text length in chars; longer payloads are not chats.
pub const MAX_CHAT_TEXT_LEN: usize = 4000;

/// Max accepted inbound file size (decoded bytes). Anything above is
/// refused with a `hub-file-error` event: no row, no staged file.
pub const MAX_FILE_BYTES: usize = 25 * 1024 * 1024;

/// Max accepted file display name length in chars; longer names are not
/// file payloads.
pub const MAX_FILE_NAME_CHARS: usize = 255;

/// Payload-level parse: `{type:"chat", text:<string>, id?:<string>}`.
/// `None` means "not a chat payload". A missing, empty or overlong `id`
/// parses to `None` and the caller mints a fresh one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedChat {
    pub text: String,
    pub id: Option<String>,
}

pub fn parse_chat(payload: &serde_json::Value) -> Option<ParsedChat> {
    if payload.get("type")?.as_str()? != "chat" {
        return None;
    }
    let text = payload.get("text")?.as_str()?;
    let count = text.chars().count();
    if count == 0 || count > MAX_CHAT_TEXT_LEN {
        return None;
    }
    let id = match payload.get("id").and_then(serde_json::Value::as_str) {
        Some(id) if !id.is_empty() && id.len() <= MAX_MSG_ID_LEN => Some(id.to_string()),
        _ => None,
    };
    Some(ParsedChat {
        text: text.to_string(),
        id,
    })
}

/// sha256 hex of the payload text bytes (the stored `content_hash`).
pub fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Authorization + routing context for one inbound chat frame, resolved by
/// the caller from the current shared state (presence, pairing, contact
/// keys). Pure data: no locks are held while this module runs.
pub struct ChatContext {
    pub session_id: String,
    pub peer: Option<HubPeer>,
    pub authorized: bool,
    pub contact_key: Option<String>,
}

/// Persisted + emitted chat message (the `hub-message-received` payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReceivedChat {
    pub key: String,
    pub name: String,
    pub text: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatOutcome {
    /// Persisted (and emitted): the row was committed before the hook ran.
    Accepted(ReceivedChat),
    /// Same id + same content already stored: no second row, no event.
    Duplicate,
    /// Same id with different content or under another key: rejected,
    /// original row intact. The caller logs it and moves on.
    Conflict,
    /// Dropped without any side effect; the string is the log reason.
    Dropped(&'static str),
}

/// Authorizes, hashes, persists and (on `New` only) emits one inbound chat
/// frame. `mint` is the id generator (production: `identity::new_uuid`);
/// `emit` runs AFTER `accept_text` committed, with the same connection.
pub fn handle_chat_frame(
    from_sid: Option<&str>,
    payload: &serde_json::Value,
    ctx: ChatContext,
    mint: &dyn Fn() -> Result<String, String>,
    now: i64,
    conn: &mut Connection,
    emit: &dyn Fn(&ReceivedChat, &Connection),
) -> ChatOutcome {
    // Authorization gate: fail closed on any missing piece.
    let Some(peer) = ctx.peer else {
        return ChatOutcome::Dropped("unknown peer");
    };
    if peer.kind != "web" {
        return ChatOutcome::Dropped("peer is not web");
    }
    let Some(stamped) = from_sid else {
        return ChatOutcome::Dropped("missing from_sid");
    };
    if peer.sid.as_deref() != Some(stamped) {
        return ChatOutcome::Dropped("from_sid mismatch");
    }
    if !ctx.authorized {
        return ChatOutcome::Dropped("no live grant");
    }
    let Some(key) = ctx.contact_key else {
        return ChatOutcome::Dropped("no contact key");
    };
    // Payload gate: only bounded chat payloads are accepted.
    let Some(parsed) = parse_chat(payload) else {
        return ChatOutcome::Dropped("not a chat payload");
    };
    // The wire id is reused only when valid; anything else is minted here
    // (never trust wire data as a storage key).
    let id = match parsed.id {
        Some(id) => id,
        None => match mint() {
            Ok(id) => id,
            Err(_) => return ChatOutcome::Dropped("id mint failed"),
        },
    };
    let hash = sha256_hex(&parsed.text);
    match crate::history::accept_text(conn, &key, &id, &parsed.text, now, Some(&hash)) {
        Ok(crate::history::Accept::New) => {
            let msg = ReceivedChat {
                key,
                name: peer.name,
                text: parsed.text,
                id,
            };
            // Persist-before-emit: the row is already committed (accept_text
            // owns its transaction), so the hook can prove visibility.
            emit(&msg, conn);
            ChatOutcome::Accepted(msg)
        }
        Ok(crate::history::Accept::Duplicate) => ChatOutcome::Duplicate,
        Err(_) => ChatOutcome::Conflict,
    }
}

// ---- Clipboard (shared clipboard over the hub) ----

/// Persisted + emitted clipboard share (the `hub-clipboard-received`
/// payload). Same shape as `ReceivedChat`; the frontend owns the single
/// clipboard entry rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReceivedClipboard {
    pub key: String,
    pub name: String,
    pub text: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardOutcome {
    /// Persisted (and emitted): the row was committed before the hook ran.
    Accepted(ReceivedClipboard),
    /// Same id + same content already stored: no second row, no event.
    Duplicate,
    /// Same id with different content or under another key: rejected.
    Conflict,
    /// Dropped without any side effect; the string is the log reason.
    Dropped(&'static str),
}

/// Payload-level parse: `{type:"clipboard", text:<string>, id?:<string>}`.
/// Same id rules as chat; the text bound is the clipboard one (chars).
pub fn parse_clipboard(payload: &serde_json::Value) -> Option<ParsedChat> {
    if payload.get("type")?.as_str()? != "clipboard" {
        return None;
    }
    let text = payload.get("text")?.as_str()?;
    let count = text.chars().count();
    if count == 0 || count > crate::clipboard::MAX_CLIPBOARD_TEXT {
        return None;
    }
    let id = match payload.get("id").and_then(serde_json::Value::as_str) {
        Some(id) if !id.is_empty() && id.len() <= MAX_MSG_ID_LEN => Some(id.to_string()),
        _ => None,
    };
    Some(ParsedChat {
        text: text.to_string(),
        id,
    })
}

/// Authorizes, persists and (on `New` only) emits one inbound clipboard
/// frame — the clipboard twin of `handle_chat_frame`: SAME authorization
/// gates and SAME `accept_text` persist path (a normal text row), but a
/// 64_000-char text bound and its own emit hook. Duplicates stay silent.
pub fn handle_clipboard_frame(
    from_sid: Option<&str>,
    payload: &serde_json::Value,
    ctx: ChatContext,
    mint: &dyn Fn() -> Result<String, String>,
    now: i64,
    conn: &mut Connection,
    emit: &dyn Fn(&ReceivedClipboard, &Connection),
) -> ClipboardOutcome {
    // Authorization gate: identical to chat, fail closed.
    let (peer, key) = match authorize(ctx, from_sid) {
        Ok(ok) => ok,
        Err(reason) => return ClipboardOutcome::Dropped(reason),
    };
    // Payload gate: only bounded clipboard payloads are accepted.
    let Some(parsed) = parse_clipboard(payload) else {
        return ClipboardOutcome::Dropped("not a clipboard payload");
    };
    // The wire id is reused only when valid; anything else is minted here.
    let id = match parsed.id {
        Some(id) => id,
        None => match mint() {
            Ok(id) => id,
            Err(_) => return ClipboardOutcome::Dropped("id mint failed"),
        },
    };
    let hash = sha256_hex(&parsed.text);
    match crate::history::accept_text(conn, &key, &id, &parsed.text, now, Some(&hash)) {
        Ok(crate::history::Accept::New) => {
            let msg = ReceivedClipboard {
                key,
                name: peer.name,
                text: parsed.text,
                id,
            };
            // Persist-before-emit: the row is already committed (accept_text
            // owns its transaction), so the hook can prove visibility.
            emit(&msg, conn);
            ClipboardOutcome::Accepted(msg)
        }
        Ok(crate::history::Accept::Duplicate) => ClipboardOutcome::Duplicate,
        Err(_) => ClipboardOutcome::Conflict,
    }
}

// ---- Files (Slice 5a): inbound staging pipeline ----

/// Persisted + emitted received file (the `hub-file-received` payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceivedFile {
    pub key: String,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub id: String,
}

/// Emitted when an authorized inbound file is refused (`hub-file-error`
/// payload): oversized payload or disk failure. No row exists when it fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileError {
    pub key: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOutcome {
    /// Persisted, finalized and emitted: the row was committed AND the file
    /// renamed into the download dir before the hook ran.
    Accepted(ReceivedFile),
    /// Same id + same content already stored: no second row, no file, no event.
    Duplicate,
    /// Same id with different content or under another key: rejected,
    /// original row intact. The caller logs it and moves on.
    Conflict,
    /// Decoded payload above `MAX_FILE_BYTES`: no row, no staged file,
    /// `hub-file-error` already emitted.
    TooLarge { name: String },
    /// Dropped without any side effect; the string is the log reason.
    Dropped(&'static str),
}

/// Payload-level parse result for a file frame: sanitized display name,
/// decoded bytes, and the wire id when it is valid (same rules as chat).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFile {
    pub name: String,
    pub bytes: Vec<u8>,
    pub id: Option<String>,
}

pub fn sanitize_name(raw: &str) -> String {
    // Strip every path component (both separators) and dot segments so the
    // display name can never carry traversal on any platform.
    let last = raw
        .split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != "." && *c != "..")
        .last()
        .unwrap_or("file");
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_FILE_NAME_CHARS)
        .collect();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

/// Payload-level parse: `{type:"file", name:<string>, data:<base64>,
/// size?:<number>, id?:<string>}`. `None` means "not a file payload":
/// wrong type, invalid name, invalid or empty base64, a declared size that
/// mismatches the decoded bytes, or a non-number size. The size BOUND is
/// the handler's decision (`TooLarge`), not a parse failure.
pub fn parse_file(payload: &serde_json::Value) -> Option<ParsedFile> {
    if payload.get("type")?.as_str()? != "file" {
        return None;
    }
    let raw = payload.get("name")?.as_str()?;
    if raw.is_empty() || raw.chars().count() > MAX_FILE_NAME_CHARS {
        return None;
    }
    let name = sanitize_name(raw);
    let bytes = super::b64::decode(payload.get("data")?.as_str()?).ok()?;
    if bytes.is_empty() {
        return None;
    }
    // An optional declared size is honored only when it tells the truth.
    if let Some(size) = payload.get("size") {
        match size.as_u64() {
            Some(n) if n as usize == bytes.len() => {}
            _ => return None,
        }
    }
    let id = match payload.get("id").and_then(serde_json::Value::as_str) {
        Some(id) if !id.is_empty() && id.len() <= MAX_MSG_ID_LEN => Some(id.to_string()),
        _ => None,
    };
    Some(ParsedFile { name, bytes, id })
}

/// sha256 hex over the sanitized name + NUL + file bytes (the stored
/// `content_hash` for file rows; domain-separated from the chat text hash).
pub fn file_hash_hex(name: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"file:");
    hasher.update(name.as_bytes());
    hasher.update([0u8]);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Maps a wire id to the staged file name: a plain safe single component
/// is reused; anything else (empty, `.`, `..`, separators) is hex-encoded,
/// so the staged path always stays inside `.hub-stage` and duplicate
/// cleanup keeps working deterministically.
fn staged_name(id: &str) -> String {
    if !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        id.to_string()
    } else {
        id.bytes().map(|b| format!("{b:02x}")).collect()
    }
}

/// Writes the decoded bytes to `<dir>/.hub-stage/<id>` (creating the stage
/// dir on demand) and returns the staged path. A wire id that is not a
/// plain single-component file name is hex-encoded, so the staged file is
/// always safely inside `.hub-stage`.
pub fn stage_file(dir: &Path, id: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let safe = staged_name(id);
    let stage = dir.join(".hub-stage");
    std::fs::create_dir_all(&stage).map_err(|e| format!("stage dir: {e}"))?;
    let path = stage.join(safe);
    std::fs::write(&path, bytes).map_err(|e| format!("stage write: {e}"))?;
    Ok(path)
}

/// Inbound file acceptance: one transaction, INSERT only — never
/// `INSERT OR REPLACE`. Same rules as `accept_text`: same id under a
/// different key → conflict; same id + same key + hash match → `Duplicate`;
/// legacy NULL-hash rows fall back to name equality + file-path presence
/// (a file row always carries a path). On `New` the row is inserted with
/// `file_path = stage_path` INSIDE the transaction, so the row and the
/// staged file become visible together. The size is not persisted: the
/// schema has no column, and the stored file itself is the source of truth.
pub fn accept_file(
    conn: &mut Connection,
    device_key: &str,
    id: &str,
    name: &str,
    stage_path: &Path,
    at: i64,
    hash: Option<&str>,
) -> Result<crate::history::Accept, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let existing: Option<(String, String, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT device_key, text, content_hash, file_path FROM messages WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other.to_string()),
        })?;
    if let Some((key0, text0, hash0, file0)) = existing {
        tx.commit().map_err(|e| e.to_string())?; // read-only branch
        if key0 != device_key {
            return Err("conflict".into()); // same id, different conversation
        }
        let same = match (hash0.as_deref(), hash) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            // Legacy fallback: name equality + file-path presence.
            _ => text0 == name && file0.is_some(),
        };
        if !same {
            return Err("conflict".into()); // same id, different payload
        }
        return Ok(crate::history::Accept::Duplicate);
    }
    tx.execute(
        "INSERT INTO messages
         (id, device_key, mine, text, at, state, file_path, read, content_hash)
         VALUES (?1, ?2, 0, ?3, ?4, NULL, ?5, 0, ?6)",
        rusqlite::params![id, device_key, name, at, stage_path.to_string_lossy(), hash],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(crate::history::Accept::New)
}

/// Collision-free destination inside `dir` for a display name: the name
/// itself, then `stem (1).ext`, `stem (2).ext`, ...
fn collision_free_target(dir: &Path, display: &str) -> PathBuf {
    let candidate = dir.join(display);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(display);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(display);
    let ext = p.extension().and_then(|s| s.to_str());
    for n in 1u32.. {
        let name = match ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("collision loop is unbounded over u32")
}

/// Moves a committed row's staged file into the download dir: sanitizes the
/// stored display name, resolves collisions, renames the staged file and
/// points the row at the final path. Failure never loses data: the row
/// keeps pointing at the (still valid) staged file and the caller logs.
pub fn finalize_staged_file(conn: &Connection, id: &str, dir: &Path) -> Result<PathBuf, String> {
    let (name, staged): (String, Option<String>) = conn
        .query_row(
            "SELECT text, file_path FROM messages WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let Some(staged) = staged else {
        return Err("finalize: row has no staged file".into());
    };
    let display = sanitize_name(&name);
    let target = collision_free_target(dir, &display);
    std::fs::rename(&staged, &target).map_err(|e| format!("finalize rename: {e}"))?;
    if let Err(e) = conn.execute(
        "UPDATE messages SET file_path = ?1 WHERE id = ?2",
        rusqlite::params![target.to_string_lossy(), id],
    ) {
        // Roll the file back so the row's staged path stays valid.
        let _ = std::fs::rename(&target, &staged);
        return Err(format!("finalize update: {e}"));
    }
    Ok(target)
}

/// Startup sweep: deletes `.hub-stage` entries that no row references
/// anymore (crash windows converge: unreferenced → swept, referenced →
/// kept). Returns the swept count. `fs::remove_file` in non-test code is
/// confined to: this sweep, the staged-file cleanups inside
/// `handle_file_frame` (stage failure, duplicate, tx failure), and the
/// pre-existing LAN transfer cleanup in `lib.rs`. None of them runs on a
/// user-facing deletion path: deletes are DB-only and referenced files
/// stay byte-identical.
pub fn sweep_stage_dir(conn: &Connection, dir: &Path) -> usize {
    let stage = dir.join(".hub-stage");
    let Ok(entries) = std::fs::read_dir(&stage) else {
        return 0; // no stage dir (or unreadable): nothing to sweep
    };
    let mut swept = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let referenced = conn
            .query_row(
                "SELECT 1 FROM messages WHERE file_path = ?1 LIMIT 1",
                [path.to_string_lossy()],
                |r| r.get::<_, i64>(0),
            )
            .is_ok();
        if !referenced && std::fs::remove_file(&path).is_ok() {
            swept += 1;
        }
    }
    swept
}

/// Authorization gate shared by chat and file frames: fail closed on any
/// missing piece. Returns the live peer and the minted contact key.
fn authorize(ctx: ChatContext, from_sid: Option<&str>) -> Result<(HubPeer, String), &'static str> {
    let peer = ctx.peer.ok_or("unknown peer")?;
    if peer.kind != "web" {
        return Err("peer is not web");
    }
    let stamped = from_sid.ok_or("missing from_sid")?;
    if peer.sid.as_deref() != Some(stamped) {
        return Err("from_sid mismatch");
    }
    if !ctx.authorized {
        return Err("no live grant");
    }
    let key = ctx.contact_key.ok_or("no contact key")?;
    Ok((peer, key))
}

/// Authorizes, stages, persists, finalizes and (on `New` only) emits one
/// inbound file frame — the file twin of `handle_chat_frame`. `emit` runs
/// AFTER the row commit AND the finalize rename, with the same connection.
/// Refusals after authorization (oversized payload, disk failure) emit
/// `on_error` (`hub-file-error`): no row, no staged file left behind.
#[allow(clippy::too_many_arguments)]
pub fn handle_file_frame(
    from_sid: Option<&str>,
    payload: &serde_json::Value,
    ctx: ChatContext,
    download_dir: &Path,
    mint: &dyn Fn() -> Result<String, String>,
    now: i64,
    conn: &mut Connection,
    emit: &dyn Fn(&ReceivedFile, &Connection),
    on_error: &dyn Fn(&FileError),
) -> FileOutcome {
    // Authorization gate: identical to chat, fail closed. The peer display
    // name lives in the payload; authorization itself only needs the key.
    let (_, key) = match authorize(ctx, from_sid) {
        Ok(ok) => ok,
        Err(reason) => return FileOutcome::Dropped(reason),
    };
    // Payload gate: only well-formed file payloads are accepted.
    let Some(parsed) = parse_file(payload) else {
        return FileOutcome::Dropped("not a file payload");
    };
    if parsed.bytes.len() > MAX_FILE_BYTES {
        on_error(&FileError {
            key: key.clone(),
            name: parsed.name.clone(),
            reason: "file-too-large".to_string(),
        });
        return FileOutcome::TooLarge { name: parsed.name };
    }
    // The wire id is reused only when valid; anything else is minted here.
    let id = match parsed.id {
        Some(id) => id,
        None => match mint() {
            Ok(id) => id,
            Err(_) => return FileOutcome::Dropped("id mint failed"),
        },
    };
    let staged = match stage_file(download_dir, &id, &parsed.bytes) {
        Ok(path) => path,
        Err(_) => {
            on_error(&FileError {
                key: key.clone(),
                name: parsed.name.clone(),
                reason: "stage-failed".to_string(),
            });
            let stage = download_dir.join(".hub-stage");
            let _ = std::fs::remove_file(stage.join(staged_name(&id)));
            return FileOutcome::Dropped("stage failed");
        }
    };
    let hash = file_hash_hex(&parsed.name, &parsed.bytes);
    match accept_file(conn, &key, &id, &parsed.name, &staged, now, Some(&hash)) {
        Ok(crate::history::Accept::New) => {
            match finalize_staged_file(conn, &id, download_dir) {
                Ok(final_path) => {
                    let file = ReceivedFile {
                        key,
                        name: parsed.name,
                        path: final_path.to_string_lossy().into_owned(),
                        size: parsed.bytes.len() as u64,
                        id,
                    };
                    // Persist + finalize BEFORE emit: the hook can trust both.
                    emit(&file, conn);
                    FileOutcome::Accepted(file)
                }
                Err(_) => {
                    // The row still points at the valid staged file; nothing
                    // is lost. The caller logs it and the next startup sweep
                    // keeps the referenced file.
                    FileOutcome::Dropped("finalize failed")
                }
            }
        }
        Ok(crate::history::Accept::Duplicate) => {
            // The row already exists: the staged duplicate is just garbage.
            let _ = std::fs::remove_file(&staged);
            FileOutcome::Duplicate
        }
        Err(_) => {
            // Tx failure (conflict or db error): the staged file is owned
            // by nobody — clean it up.
            let _ = std::fs::remove_file(&staged);
            FileOutcome::Conflict
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::history;

    const SESS: &str = "sess-1";

    /// Real SQLite file in the OS temp dir (never in-memory), unique per call.
    fn temp_db(label: &str) -> Connection {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let _ = fs::remove_file(std::env::temp_dir().join(format!(
            "lan-chat-inbox-{label}-{}-{n}.db",
            std::process::id()
        )));
        let path = std::env::temp_dir().join(format!(
            "lan-chat-inbox-{label}-{}-{n}.db",
            std::process::id()
        ));
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
        history::ensure_history_schema(&conn).unwrap();
        conn
    }

    fn web_peer() -> HubPeer {
        HubPeer {
            conn_id: "w1".into(),
            name: "Ana".into(),
            kind: "web".into(),
            sid: Some("sid-1".into()),
            caps: vec![],
        }
    }

    fn ctx(peer: Option<HubPeer>, authorized: bool, key: Option<&str>) -> ChatContext {
        ChatContext {
            session_id: SESS.into(),
            peer,
            authorized,
            contact_key: key.map(str::to_string),
        }
    }

    fn chat_payload(text: &str, id: &str) -> serde_json::Value {
        serde_json::json!({ "type": "chat", "text": text, "id": id })
    }

    fn counter_mint(prefix: &'static str) -> impl Fn() -> Result<String, String> {
        let n = Cell::new(0u32);
        move || {
            n.set(n.get() + 1);
            Ok(format!("{prefix}-{}", n.get()))
        }
    }

    fn row_count(conn: &Connection, id: &str) -> usize {
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE id = ?1 AND device_key = 'hub:k1'",
            [id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as usize)
        .unwrap()
    }

    const OK_SID: Option<&str> = Some("sid-1");

    fn accept_ok(
        payload: &serde_json::Value,
        ctx: ChatContext,
        conn: &mut Connection,
    ) -> (ChatOutcome, Vec<ReceivedChat>) {
        let mint = counter_mint("minted");
        let seen = std::cell::RefCell::new(Vec::new());
        let emit = |msg: &ReceivedChat, c: &Connection| {
            seen.borrow_mut().push(msg.clone());
            let _ = c; // row visibility asserted by dedicated tests
        };
        let outcome = handle_chat_frame(OK_SID, payload, ctx, &mint, 1_000, conn, &emit);
        (outcome, seen.into_inner())
    }

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn parse_chat_accepts_valid_payload_and_keeps_valid_ids() {
        let parsed = parse_chat(&chat_payload("hola", "m-1")).unwrap();
        assert_eq!(parsed.text, "hola");
        assert_eq!(parsed.id.as_deref(), Some("m-1"));
        assert!(parse_chat(&serde_json::json!({ "type": "chat" })).is_none());
        assert!(parse_chat(&serde_json::json!({ "type": "chat", "text": 42 })).is_none());
        assert!(parse_chat(&serde_json::json!({ "type": "pair-request" })).is_none());
    }

    #[test]
    fn parse_chat_rejects_empty_and_oversized_ids() {
        assert_eq!(parse_chat(&chat_payload("x", "")).unwrap().id, None);
        let long = "a".repeat(MAX_MSG_ID_LEN + 1);
        assert_eq!(parse_chat(&chat_payload("x", &long)).unwrap().id, None);
        let exact = "b".repeat(MAX_MSG_ID_LEN);
        assert_eq!(
            parse_chat(&chat_payload("x", &exact)).unwrap().id,
            Some(exact)
        );
    }

    #[test]
    fn unauthorized_chat_without_grant_is_dropped_without_row_or_event() {
        let mut conn = temp_db("no-grant");
        let (outcome, seen) = accept_ok(
            &chat_payload("hola", "m-1"),
            ctx(Some(web_peer()), false, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(outcome, ChatOutcome::Dropped(_)));
        assert!(seen.is_empty(), "dropped frames never emit");
        assert_eq!(row_count(&conn, "m-1"), 0, "dropped frames never persist");
    }

    #[test]
    fn unknown_from_id_is_dropped() {
        let mut conn = temp_db("unknown-peer");
        let (outcome, seen) = accept_ok(
            &chat_payload("hola", "m-1"),
            ctx(None, true, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(outcome, ChatOutcome::Dropped(_)));
        assert!(seen.is_empty());
        assert_eq!(row_count(&conn, "m-1"), 0);
    }

    #[test]
    fn missing_from_sid_is_dropped() {
        let mut conn = temp_db("missing-sid");
        let mint = counter_mint("minted");
        let seen = std::cell::RefCell::new(Vec::new());
        let emit = |msg: &ReceivedChat, _c: &Connection| seen.borrow_mut().push(msg.clone());
        let outcome = handle_chat_frame(
            None,
            &chat_payload("hola", "m-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mint,
            1_000,
            &mut conn,
            &emit,
        );
        assert!(matches!(outcome, ChatOutcome::Dropped(_)));
        assert!(seen.borrow().is_empty());
        assert_eq!(row_count(&conn, "m-1"), 0);
    }

    #[test]
    fn mismatched_from_sid_is_dropped() {
        let mut conn = temp_db("bad-sid");
        let mint = counter_mint("minted");
        let seen = std::cell::RefCell::new(Vec::new());
        let emit = |msg: &ReceivedChat, _c: &Connection| seen.borrow_mut().push(msg.clone());
        let outcome = handle_chat_frame(
            Some("other-sid"),
            &chat_payload("hola", "m-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mint,
            1_000,
            &mut conn,
            &emit,
        );
        assert!(matches!(outcome, ChatOutcome::Dropped(_)));
        assert!(seen.borrow().is_empty());
        assert_eq!(row_count(&conn, "m-1"), 0);
    }

    #[test]
    fn non_web_peer_is_dropped() {
        let mut conn = temp_db("non-web");
        let mut app_peer = web_peer();
        app_peer.kind = "app".into();
        let (outcome, _) = accept_ok(
            &chat_payload("hola", "m-1"),
            ctx(Some(app_peer), true, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(outcome, ChatOutcome::Dropped(_)));
        assert_eq!(row_count(&conn, "m-1"), 0);
    }

    #[test]
    fn authorized_chat_persists_before_emit() {
        let mut conn = temp_db("persist-before-emit");
        let mint = counter_mint("minted");
        // The emit hook queries the SAME connection: if the row were not
        // committed yet, this assertion fails — persist-before-emit, proven.
        let emit = |msg: &ReceivedChat, c: &Connection| {
            assert_eq!(row_count(c, &msg.id), 1, "row must be committed first");
        };
        let outcome = handle_chat_frame(
            OK_SID,
            &chat_payload("hola", "m-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mint,
            1_000,
            &mut conn,
            &emit,
        );
        match outcome {
            ChatOutcome::Accepted(msg) => {
                assert_eq!(msg.key, "hub:k1");
                assert_eq!(msg.name, "Ana");
                assert_eq!(msg.text, "hola");
                assert_eq!(msg.id, "m-1");
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
        assert_eq!(row_count(&conn, "m-1"), 1);
    }

    #[test]
    fn duplicate_inbound_skips_event_and_row() {
        let mut conn = temp_db("duplicate");
        let frame = chat_payload("hola", "m-1");
        let (first, seen1) = accept_ok(
            &frame,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(first, ChatOutcome::Accepted(_)));
        assert_eq!(seen1.len(), 1);
        let (second, seen2) = accept_ok(
            &frame,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert_eq!(second, ChatOutcome::Duplicate);
        assert!(seen2.is_empty(), "duplicates never emit");
        assert_eq!(row_count(&conn, "m-1"), 1, "no second row");
    }

    #[test]
    fn conflicting_inbound_payload_is_rejected_and_original_intact() {
        let mut conn = temp_db("conflict");
        let (first, seen1) = accept_ok(
            &chat_payload("hola", "m-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(first, ChatOutcome::Accepted(_)));
        assert_eq!(seen1.len(), 1);
        let (second, seen2) = accept_ok(
            &chat_payload("OTRA cosa", "m-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert_eq!(second, ChatOutcome::Conflict);
        assert!(seen2.is_empty(), "conflicts never emit");
        let (text, hash): (String, Option<String>) = conn
            .query_row(
                "SELECT text, content_hash FROM messages WHERE id = 'm-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(text, "hola", "original row intact");
        assert_eq!(hash, Some(sha256_hex("hola")));
    }

    #[test]
    fn empty_and_oversized_payload_ids_are_minted_fresh() {
        for bad in ["", "a".repeat(MAX_MSG_ID_LEN + 1).as_str()] {
            let mut conn = temp_db("mint");
            let (outcome, seen) = accept_ok(
                &chat_payload("hola", bad),
                ctx(Some(web_peer()), true, Some("hub:k1")),
                &mut conn,
            );
            let ChatOutcome::Accepted(msg) = outcome else {
                panic!("expected Accepted for id {bad:?}");
            };
            assert!(
                msg.id.starts_with("minted-"),
                "wire id {bad:?} must be minted"
            );
            assert_eq!(seen.len(), 1);
            assert_eq!(row_count(&conn, &msg.id), 1);
        }
    }

    #[test]
    fn missing_contact_key_drops_even_when_authorized() {
        let mut conn = temp_db("no-key");
        let (outcome, seen) = accept_ok(
            &chat_payload("hola", "m-1"),
            ctx(Some(web_peer()), true, None),
            &mut conn,
        );
        assert!(matches!(outcome, ChatOutcome::Dropped(_)));
        assert!(seen.is_empty());
        assert_eq!(row_count(&conn, "m-1"), 0);
    }

    #[test]
    fn accepted_row_is_mine_zero_with_hash_and_state_null() {
        let mut conn = temp_db("row-shape");
        let (outcome, _) = accept_ok(
            &chat_payload("hola", "m-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(outcome, ChatOutcome::Accepted(_)));
        let (mine, state, hash): (i64, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT mine, state, content_hash FROM messages WHERE id = 'm-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(mine, 0);
        assert_eq!(state, None);
        assert_eq!(hash, Some(sha256_hex("hola")));
    }

    // ---- Clipboard ----

    fn clipboard_payload(text: &str, id: &str) -> serde_json::Value {
        serde_json::json!({ "type": "clipboard", "text": text, "id": id })
    }

    fn clipboard_ok(
        payload: &serde_json::Value,
        ctx: ChatContext,
        conn: &mut Connection,
    ) -> (ClipboardOutcome, Vec<ReceivedClipboard>) {
        let mint = counter_mint("cmint");
        let seen = std::cell::RefCell::new(Vec::new());
        let emit = |msg: &ReceivedClipboard, _c: &Connection| {
            seen.borrow_mut().push(msg.clone());
        };
        let outcome = handle_clipboard_frame(OK_SID, payload, ctx, &mint, 1_000, conn, &emit);
        (outcome, seen.into_inner())
    }

    #[test]
    fn parse_clipboard_accepts_valid_payload_and_validates_fields() {
        let parsed = parse_clipboard(&clipboard_payload("hola", "c-1")).unwrap();
        assert_eq!(parsed.text, "hola");
        assert_eq!(parsed.id.as_deref(), Some("c-1"));
        assert!(parse_clipboard(&serde_json::json!({ "type": "chat", "text": "x" })).is_none());
        assert!(parse_clipboard(&serde_json::json!({ "type": "clipboard" })).is_none());
        assert!(parse_clipboard(&serde_json::json!({ "type": "clipboard", "text": 42 })).is_none());
        assert_eq!(
            parse_clipboard(&clipboard_payload("x", "")).unwrap().id,
            None
        );
    }

    #[test]
    fn parse_clipboard_bounds_text_in_chars() {
        let exact = "é".repeat(crate::clipboard::MAX_CLIPBOARD_TEXT);
        assert!(parse_clipboard(&clipboard_payload(&exact, "c")).is_some());
        let over = "é".repeat(crate::clipboard::MAX_CLIPBOARD_TEXT + 1);
        assert!(parse_clipboard(&clipboard_payload(&over, "c")).is_none());
        assert!(parse_clipboard(&clipboard_payload("", "c")).is_none());
    }

    #[test]
    fn unauthorized_clipboard_is_dropped_without_row_or_event() {
        let mut conn = temp_db("clip-no-grant");
        let (outcome, seen) = clipboard_ok(
            &clipboard_payload("hola", "c-1"),
            ctx(Some(web_peer()), false, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(outcome, ClipboardOutcome::Dropped(_)));
        assert!(seen.is_empty());
        assert_eq!(row_count(&conn, "c-1"), 0);
    }

    #[test]
    fn authorized_clipboard_persists_normal_text_row_and_emits() {
        let mut conn = temp_db("clip-happy");
        let (outcome, seen) = clipboard_ok(
            &clipboard_payload("contenido", "c-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        let ClipboardOutcome::Accepted(msg) = outcome else {
            panic!("expected Accepted, got {outcome:?}");
        };
        assert_eq!(msg.key, "hub:k1");
        assert_eq!(msg.name, "Ana");
        assert_eq!(msg.text, "contenido");
        assert_eq!(msg.id, "c-1");
        assert_eq!(seen, vec![msg.clone()]);
        // Persist-before-emit + row shape: a NORMAL text row (mine=0,
        // NULL state, text hash) exactly like inbound chat.
        assert_eq!(row_count(&conn, "c-1"), 1);
        let (text, mine, state, hash): (String, i64, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT text, mine, state, content_hash FROM messages WHERE id = 'c-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(text, "contenido");
        assert_eq!(mine, 0);
        assert_eq!(state, None);
        assert_eq!(hash, Some(sha256_hex("contenido")));
    }

    #[test]
    fn duplicate_clipboard_is_silent_no_second_row() {
        let mut conn = temp_db("clip-dup");
        let frame = clipboard_payload("hola", "c-1");
        let (first, seen1) = clipboard_ok(
            &frame,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(first, ClipboardOutcome::Accepted(_)));
        assert_eq!(seen1.len(), 1);
        let (second, seen2) = clipboard_ok(
            &frame,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert_eq!(second, ClipboardOutcome::Duplicate);
        assert!(seen2.is_empty(), "duplicates never emit");
        assert_eq!(row_count(&conn, "c-1"), 1);
    }

    #[test]
    fn oversized_clipboard_is_dropped_without_row() {
        let over = "x".repeat(crate::clipboard::MAX_CLIPBOARD_TEXT + 1);
        let mut conn = temp_db("clip-over");
        let (outcome, seen) = clipboard_ok(
            &clipboard_payload(&over, "c-big"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(outcome, ClipboardOutcome::Dropped(_)));
        assert!(seen.is_empty());
        assert_eq!(row_count(&conn, "c-big"), 0);
    }

    #[test]
    fn conflicting_clipboard_id_is_rejected() {
        let mut conn = temp_db("clip-conflict");
        let (first, _) = clipboard_ok(
            &clipboard_payload("hola", "c-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert!(matches!(first, ClipboardOutcome::Accepted(_)));
        let (second, seen2) = clipboard_ok(
            &clipboard_payload("OTRA", "c-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &mut conn,
        );
        assert_eq!(second, ClipboardOutcome::Conflict);
        assert!(seen2.is_empty());
        assert_eq!(row_count(&conn, "c-1"), 1);
    }

    // ---- Files (Slice 5a) ----

    use std::os::unix::fs::PermissionsExt;

    /// Unique temp download dir per call; stale ones from prior runs of the
    /// same label are removed first (test hygiene only).
    fn temp_dir(label: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "lan-chat-inbox-dl-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    /// Test-only RFC 4648 encoder (the production encoder ships in 5b).
    fn b64_of(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(T[(n >> 18) as usize & 63] as char);
            out.push(T[(n >> 12) as usize & 63] as char);
            if chunk.len() > 1 {
                out.push(T[(n >> 6) as usize & 63] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(T[n as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    fn file_payload(name: &str, data: &str, size: Option<u64>, id: &str) -> serde_json::Value {
        let mut v = serde_json::json!({ "type": "file", "name": name, "data": data });
        if let Some(size) = size {
            v["size"] = serde_json::json!(size);
        }
        if !id.is_empty() {
            v["id"] = serde_json::json!(id);
        }
        v
    }

    fn insert_row(
        conn: &Connection,
        id: &str,
        key: &str,
        text: &str,
        file_path: Option<&str>,
        hash: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO messages
             (id, device_key, mine, text, at, state, file_path, read, content_hash)
             VALUES (?1, ?2, 0, ?3, 1, NULL, ?4, 0, ?5)",
            rusqlite::params![id, key, text, file_path, hash],
        )
        .unwrap();
    }

    /// Runs `handle_file_frame` with a counter mint, recording file emits
    /// (with row visibility + on-disk presence AT EMIT TIME) and errors.
    fn handle_file_ok(
        payload: &serde_json::Value,
        ctx: ChatContext,
        dir: &Path,
        conn: &mut Connection,
    ) -> (FileOutcome, Vec<ReceivedFile>, Vec<FileError>) {
        let mint = counter_mint("fmint");
        let seen = std::cell::RefCell::new(Vec::new());
        let errors = std::cell::RefCell::new(Vec::new());
        let emit = |f: &ReceivedFile, c: &Connection| {
            let fp: Option<String> = c
                .query_row(
                    "SELECT file_path FROM messages WHERE id = ?1",
                    [&f.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                fp.as_deref(),
                Some(f.path.as_str()),
                "row must point at the final path when emitted"
            );
            assert!(
                Path::new(&f.path).exists(),
                "file must be finalized before emit"
            );
            seen.borrow_mut().push(f.clone());
        };
        let on_err = |e: &FileError| errors.borrow_mut().push(e.clone());
        let outcome = handle_file_frame(
            OK_SID, payload, ctx, dir, &mint, 1_000, conn, &emit, &on_err,
        );
        (outcome, seen.into_inner(), errors.into_inner())
    }

    fn stage_entries(dir: &Path) -> Vec<String> {
        let stage = dir.join(".hub-stage");
        let Ok(entries) = fs::read_dir(&stage) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn sanitize_name_strips_traversal_separators_and_bounds_length() {
        assert_eq!(sanitize_name("../../evil.txt"), "evil.txt");
        assert_eq!(sanitize_name("a\\b\\c.txt"), "c.txt");
        assert_eq!(sanitize_name("/abs/path/"), "path");
        assert_eq!(sanitize_name(".."), "file");
        assert_eq!(sanitize_name(""), "file");
        assert_eq!(sanitize_name("nice file.txt"), "nice file.txt");
        let long: String = "a".repeat(300);
        assert_eq!(sanitize_name(&long).chars().count(), MAX_FILE_NAME_CHARS);
        assert_eq!(sanitize_name("bad\nname.txt"), "badname.txt");
    }

    #[test]
    fn parse_file_accepts_valid_payloads_and_validates_fields() {
        let parsed = parse_file(&file_payload("nota.txt", "aG9sYQ==", Some(4), "f-1")).unwrap();
        assert_eq!(parsed.name, "nota.txt");
        assert_eq!(parsed.bytes, b"hola");
        assert_eq!(parsed.id.as_deref(), Some("f-1"));
        // Optional size may be absent; id rules match chat (missing/empty/
        // overlong → minted by the caller).
        let bare = parse_file(&file_payload("a.bin", "QQ==", None, "")).unwrap();
        assert_eq!(bare.id, None);
        let long = "i".repeat(MAX_MSG_ID_LEN + 1);
        assert_eq!(
            parse_file(&file_payload("a.bin", "QQ==", None, &long))
                .unwrap()
                .id,
            None
        );
    }

    #[test]
    fn parse_file_rejects_malformed_payloads() {
        assert!(parse_file(&serde_json::json!({ "type": "chat" })).is_none());
        assert!(
            parse_file(&file_payload("", "QQ==", None, "f")).is_none(),
            "empty name"
        );
        let long: String = "a".repeat(MAX_FILE_NAME_CHARS + 1);
        assert!(
            parse_file(&file_payload(&long, "QQ==", None, "f")).is_none(),
            "overlong name"
        );
        assert!(
            parse_file(&serde_json::json!({ "type": "file", "name": "a", "data": 42 })).is_none()
        );
        assert!(
            parse_file(&file_payload("a", "!!", None, "f")).is_none(),
            "invalid base64"
        );
        assert!(
            parse_file(&file_payload("a", "", None, "f")).is_none(),
            "empty file"
        );
        assert!(
            parse_file(&file_payload("a", "QQ==", Some(5), "f")).is_none(),
            "declared size must match decoded bytes"
        );
        assert!(
            parse_file(&file_payload("a", "aG9sYQ==", Some(1), "f")).is_none(),
            "declared size must match decoded bytes (other side)"
        );
        assert!(
            parse_file(
                &serde_json::json!({ "type": "file", "name": "a", "data": "QQ==", "size": "4" })
            )
            .is_none(),
            "non-number size"
        );
    }

    #[test]
    fn file_hash_is_domain_separated_and_covers_name_and_bytes() {
        assert_ne!(file_hash_hex("a.txt", b"x"), file_hash_hex("a.txt", b"y"));
        assert_ne!(file_hash_hex("a.txt", b"x"), file_hash_hex("b.txt", b"x"));
        assert_ne!(file_hash_hex("a.txt", b"x"), sha256_hex("x"));
    }

    #[test]
    fn stage_file_writes_bytes_and_hex_encodes_unsafe_ids() {
        let dir = temp_dir("stage");
        let p = stage_file(&dir, "f-1", b"hola").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"hola");
        assert_eq!(p.parent().unwrap(), dir.join(".hub-stage"));
        let evil = stage_file(&dir, "../../x", b"y").unwrap();
        assert_eq!(
            evil.parent().unwrap(),
            dir.join(".hub-stage"),
            "never escapes the stage dir"
        );
        assert!(evil
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unauthorized_file_is_dropped_without_row_stage_or_error() {
        let dir = temp_dir("drop");
        let mut conn = temp_db("file-drop");
        let (outcome, seen, errors) = handle_file_ok(
            &file_payload("nota.txt", "aG9sYQ==", Some(4), "f-1"),
            ctx(Some(web_peer()), false, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        assert!(matches!(outcome, FileOutcome::Dropped(_)));
        assert!(seen.is_empty());
        assert!(errors.is_empty(), "dropped frames never emit errors");
        assert_eq!(row_count(&conn, "f-1"), 0);
        assert!(!dir.join(".hub-stage").exists());
    }

    #[test]
    fn happy_path_persists_finalizes_then_emits_with_final_path() {
        let dir = temp_dir("happy");
        let mut conn = temp_db("file-happy");
        let (outcome, seen, errors) = handle_file_ok(
            &file_payload("nota.txt", "aG9sYQ==", Some(4), "f-1"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        let FileOutcome::Accepted(f) = outcome else {
            panic!("expected Accepted, got {outcome:?}");
        };
        assert_eq!(f.key, "hub:k1");
        assert_eq!(f.name, "nota.txt");
        assert_eq!(f.size, 4);
        assert_eq!(f.id, "f-1");
        assert_eq!(f.path, dir.join("nota.txt").to_string_lossy().into_owned());
        assert_eq!(fs::read(&f.path).unwrap(), b"hola");
        assert_eq!(seen, vec![f.clone()], "exactly one emit");
        assert!(errors.is_empty());
        let (text, mine, hash, fp): (String, i64, Option<String>, String) = conn
            .query_row(
                "SELECT text, mine, content_hash, file_path FROM messages WHERE id = 'f-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(text, "nota.txt");
        assert_eq!(mine, 0);
        assert_eq!(hash, Some(file_hash_hex("nota.txt", b"hola")));
        assert_eq!(fp, f.path, "row points at the final path");
        assert_eq!(stage_entries(&dir), Vec::<String>::new(), "stage drained");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_file_is_rejected_with_error_no_row_no_stage() {
        let dir = temp_dir("oversize");
        let mut conn = temp_db("file-oversize");
        let bytes = vec![0u8; MAX_FILE_BYTES + 1];
        let payload = file_payload(
            "big.bin",
            &b64_of(&bytes),
            Some(bytes.len() as u64),
            "f-big",
        );
        let (outcome, seen, errors) = handle_file_ok(
            &payload,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        assert_eq!(
            outcome,
            FileOutcome::TooLarge {
                name: "big.bin".into()
            }
        );
        assert!(seen.is_empty(), "oversized never emits the accept event");
        assert_eq!(errors.len(), 1, "oversized emits exactly one file error");
        assert_eq!(errors[0].key, "hub:k1");
        assert_eq!(errors[0].name, "big.bin");
        assert_eq!(errors[0].reason, "file-too-large");
        assert_eq!(row_count(&conn, "f-big"), 0, "no row for oversized");
        assert!(!dir.join(".hub-stage").exists(), "nothing staged");
    }

    #[cfg(unix)]
    #[test]
    fn stage_failure_cleanup_stays_inside_the_stage_dir() {
        // A traversal-shaped wire id must never let the failure cleanup
        // touch anything outside `.hub-stage`, even when the stage write
        // itself fails.
        let base = temp_dir("stage-safety");
        let dir = base.join("downloads");
        fs::create_dir_all(&dir).unwrap();
        let mut conn = temp_db("file-stage-safety");
        // The sentinel sits exactly where `stage.join("../../x")` would
        // resolve (dir/../x == base/x): the buggy cleanup deleted it.
        fs::write(base.join("x"), b"sentinel").unwrap();
        // Force the stage write to fail deterministically: an existing
        // read-only `.hub-stage` lets `create_dir_all` succeed but the
        // write fail with EACCES.
        let stage = dir.join(".hub-stage");
        fs::create_dir(&stage).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let readonly = fs::Permissions::from_mode(0o555);
        fs::set_permissions(&stage, readonly.clone()).unwrap();
        let (outcome, seen, errors) = handle_file_ok(
            &file_payload("nota.txt", "aG9sYQ==", Some(4), "../../x"),
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(outcome, FileOutcome::Dropped("stage failed".into()));
        assert!(seen.is_empty());
        assert_eq!(errors.len(), 1, "stage failure emits exactly one error");
        assert_eq!(errors[0].name, "nota.txt");
        assert_eq!(errors[0].reason, "stage-failed");
        assert_eq!(row_count(&conn, "../../x"), 0, "no row for staged failure");
        assert!(
            base.join("x").exists(),
            "sentinel outside the stage must survive the cleanup"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn boundary_file_of_exactly_max_bytes_is_accepted() {
        let dir = temp_dir("boundary");
        let mut conn = temp_db("file-boundary");
        let bytes = vec![0u8; MAX_FILE_BYTES];
        let payload = file_payload(
            "max.bin",
            &b64_of(&bytes),
            Some(bytes.len() as u64),
            "f-max",
        );
        let (outcome, seen, _) = handle_file_ok(
            &payload,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        assert!(matches!(outcome, FileOutcome::Accepted(_)));
        assert_eq!(seen.len(), 1);
        assert_eq!(fs::read(dir.join("max.bin")).unwrap().len(), MAX_FILE_BYTES);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_file_frame_is_one_row_one_file_no_event() {
        let dir = temp_dir("dup");
        let mut conn = temp_db("file-dup");
        let payload = file_payload("nota.txt", "aG9sYQ==", Some(4), "f-1");
        let (first, seen1, _) = handle_file_ok(
            &payload,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        assert!(matches!(first, FileOutcome::Accepted(_)));
        assert_eq!(seen1.len(), 1);
        let (second, seen2, _) = handle_file_ok(
            &payload,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        assert_eq!(second, FileOutcome::Duplicate);
        assert!(seen2.is_empty(), "duplicates never emit");
        assert_eq!(row_count(&conn, "f-1"), 1, "no second row");
        assert!(dir.join("nota.txt").exists());
        assert!(!dir.join("nota (1).txt").exists(), "no second file");
        assert_eq!(
            stage_entries(&dir),
            Vec::<String>::new(),
            "staged duplicate cleaned up"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tx_failure_conflict_cleans_staged_file_and_keeps_original() {
        let dir = temp_dir("conflict");
        let mut conn = temp_db("file-conflict");
        // A row with the same id under ANOTHER key forces the insert to fail
        // (scoped uniqueness / global id PK) — the forced tx failure path.
        insert_row(
            &conn,
            "f-1",
            "hub:other",
            "otra cosa",
            Some("x/y.bin"),
            None,
        );
        let payload = file_payload("nota.txt", "aG9sYQ==", Some(4), "f-1");
        let (outcome, seen, _) = handle_file_ok(
            &payload,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        assert_eq!(outcome, FileOutcome::Conflict);
        assert!(seen.is_empty(), "conflicts never emit");
        assert_eq!(
            stage_entries(&dir),
            Vec::<String>::new(),
            "staged file cleaned up"
        );
        let (key, text): (String, String) = conn
            .query_row(
                "SELECT device_key, text FROM messages WHERE id = 'f-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(key, "hub:other");
        assert_eq!(text, "otra cosa", "original row intact");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_null_hash_row_dedupes_by_name_and_path_presence() {
        let dir = temp_dir("legacy");
        let mut conn = temp_db("file-legacy");
        // Legacy row with a path: same name + path presence → duplicate.
        insert_row(
            &conn,
            "f-1",
            "hub:k1",
            "nota.txt",
            Some("old/nota.txt"),
            None,
        );
        let payload = file_payload("nota.txt", "aG9sYQ==", Some(4), "f-1");
        let (outcome, seen, _) = handle_file_ok(
            &payload,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        assert_eq!(outcome, FileOutcome::Duplicate);
        assert!(seen.is_empty());
        // Legacy row WITHOUT a path: a file always carries one → conflict.
        insert_row(&conn, "f-2", "hub:k1", "vacio.txt", None, None);
        let payload2 = file_payload("vacio.txt", "aG9sYQ==", Some(4), "f-2");
        let (outcome2, _, _) = handle_file_ok(
            &payload2,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        assert_eq!(outcome2, FileOutcome::Conflict);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_finalize_of_same_name_gets_collision_suffix() {
        let dir = temp_dir("collision");
        let mut conn = temp_db("file-collision");
        let payload = file_payload("nota.txt", "aG9sYQ==", Some(4), "f-1");
        let (first, _, _) = handle_file_ok(
            &payload,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        let FileOutcome::Accepted(f1) = first else {
            panic!()
        };
        let payload2 = file_payload("nota.txt", "Y2hhdQ==", Some(4), "f-2");
        let (second, _, _) = handle_file_ok(
            &payload2,
            ctx(Some(web_peer()), true, Some("hub:k1")),
            &dir,
            &mut conn,
        );
        let FileOutcome::Accepted(f2) = second else {
            panic!()
        };
        assert_eq!(f1.path, dir.join("nota.txt").to_string_lossy().into_owned());
        assert_eq!(
            f2.path,
            dir.join("nota (1).txt").to_string_lossy().into_owned()
        );
        assert_eq!(fs::read(&f1.path).unwrap(), b"hola");
        assert_eq!(fs::read(&f2.path).unwrap(), b"chau");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_sanitizes_display_name_into_a_bare_file_name() {
        let dir = temp_dir("finalize-sanitize");
        let conn = temp_db("file-finalize-sanitize");
        let staged = stage_file(&dir, "f-1", b"hola").unwrap();
        insert_row(
            &conn,
            "f-1",
            "hub:k1",
            "../../evil.txt",
            staged.to_str(),
            Some("aa"),
        );
        let final_path = finalize_staged_file(&conn, "f-1", &dir).unwrap();
        assert_eq!(final_path, dir.join("evil.txt"));
        assert!(final_path.exists());
        let fp: String = conn
            .query_row("SELECT file_path FROM messages WHERE id = 'f-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fp, final_path.to_string_lossy().into_owned());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_failure_keeps_row_pointing_at_valid_staged_file() {
        let dir = temp_dir("finalize-fail");
        let conn = temp_db("file-finalize-fail");
        let staged = stage_file(&dir, "f-1", b"hola").unwrap();
        insert_row(
            &conn,
            "f-1",
            "hub:k1",
            "nota.txt",
            staged.to_str(),
            Some("aa"),
        );
        // Read-only download dir: the rename must fail...
        let mut perms = fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&dir, perms).unwrap();
        let res = finalize_staged_file(&conn, "f-1", &dir);
        let mut perms = fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dir, perms).unwrap();
        assert!(res.is_err(), "rename into a read-only dir must fail");
        // ...and the row still points at the (still valid) staged file.
        let fp: String = conn
            .query_row("SELECT file_path FROM messages WHERE id = 'f-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fp, staged.to_string_lossy().into_owned());
        assert!(staged.exists(), "nothing lost, nothing faked");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_removes_only_unreferenced_stage_files() {
        let dir = temp_dir("sweep");
        let conn = temp_db("file-sweep");
        fs::create_dir_all(dir.join(".hub-stage")).unwrap();
        let orphan = dir.join(".hub-stage").join("orphan.bin");
        let referenced = dir.join(".hub-stage").join("kept.bin");
        fs::write(&orphan, b"o").unwrap();
        fs::write(&referenced, b"k").unwrap();
        insert_row(
            &conn,
            "f-1",
            "hub:k1",
            "kept.bin",
            referenced.to_str(),
            None,
        );
        assert_eq!(
            sweep_stage_dir(&conn, &dir),
            1,
            "exactly the orphan is swept"
        );
        assert!(!orphan.exists());
        assert!(referenced.exists(), "referenced files are kept");
        assert_eq!(fs::read(&referenced).unwrap(), b"k");
        assert_eq!(sweep_stage_dir(&conn, &dir), 0, "second sweep is a no-op");
        // A missing stage dir is an honest zero, not an error.
        let empty = temp_dir("sweep-empty");
        assert_eq!(sweep_stage_dir(&conn, &empty), 0);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&empty);
    }
}
