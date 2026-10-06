// LAN-Chat — motor: descubrimiento mDNS, transferencia de texto y archivos por TCP local.
pub mod clipboard;
pub mod history;
pub mod hub;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path as AxPath, Query, State as AxState,
    },
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};

const LAN_CLIENT: &str = include_str!("lan_client.html");

fn device_name() -> String {
    std::env::var("DEVICE_NAME") // override para pruebas: DEVICE_NAME=xxx al lanzar
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "lan-chat".to_string())
}

#[derive(Serialize, Clone)]
struct DiscoveredDevice {
    name: String,
    ip: String,
    service: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct ChatMessage {
    from: String,
    text: String,
    #[serde(default)]
    id: String,
}

#[derive(Serialize, Clone)]
struct FileNotice {
    from: String,
    name: String,
    path: String,
    size: u64,
    #[serde(default)]
    id: String,
}

/// Ajustes compartidos de la app (vivos mientras la app viva).
struct AppState {
    download_dir: Mutex<String>,
    db: Mutex<Connection>,
    own_pin: Mutex<String>,
    session_pin: Mutex<Option<(String, std::time::Instant)>>,
    handle: tauri::AppHandle,
    web_tx: tokio::sync::broadcast::Sender<String>,
    web_files: Mutex<HashMap<String, (String, Vec<u8>)>>,
    web_sessions: Mutex<Vec<String>>,
    web_paired: Mutex<bool>,
    hub: Arc<hub::HubShared>,
    /// Owner half of the hub shutdown signal: `RunEvent::Exit` fires it so the
    /// session loop stops even while stalled in connect or backoff.
    hub_shutdown: tokio::sync::watch::Sender<bool>,
}

/// Espejo Rust de una entrada del historial (mismos campos que Entry en TS).
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct HistoryEntry {
    id: String,
    mine: bool,
    text: String,
    at: i64,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    read: bool,
}

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

fn scan_lan_devices() -> Vec<DiscoveredDevice> {
    let Ok(mdns) = ServiceDaemon::new() else {
        return Vec::new();
    };
    let service_types = ["_lanchat._tcp.local."];

    let mut devices: Vec<DiscoveredDevice> = Vec::new();
    let my_fullname = format!("{}._lanchat._tcp.local.", device_name());

    for service_type in service_types {
        let receiver = mdns.browse(service_type).expect("Fallo el browse");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);

        while std::time::Instant::now() < deadline {
            if let Ok(event) = receiver.recv_timeout(std::time::Duration::from_millis(200)) {
                if let ServiceEvent::ServiceResolved(info) = event {
                    if info.get_fullname() != my_fullname {
                        if let Some(ip) = info
                            .get_addresses()
                            .iter()
                            .find(|ip| ip.is_ipv4() && !ip.is_loopback())
                        {
                            let device = DiscoveredDevice {
                                name: info.get_fullname().to_string(),
                                ip: ip.to_string(),
                                service: service_type.to_string(),
                            };

                            if !devices.iter().any(|d| d.name == device.name) {
                                devices.push(device);
                            }
                        }
                    }
                }
            }
        }
        let _ = mdns.stop_browse(service_type);
    }
    devices
}

#[tauri::command]
async fn discover_devices() -> Vec<DiscoveredDevice> {
    scan_lan_devices()
}

#[tauri::command]
async fn send_text(ip: String, pin: String, texto: String, id: String) -> Result<String, String> {
    use std::io::Write;

    let payload = serde_json::json!({
        "kind": "text",
        "id": id,
        "from": device_name(),
        "pin": pin,
        "text": texto
    });

    let mut stream = std::net::TcpStream::connect((ip.as_str(), 8787))
        .map_err(|e| format!("No se pudo conectar: {e}"))?;
    stream
        .write_all(format!("{payload}\n").as_bytes())
        .map_err(|e| format!("No se pudo enviar: {e}"))?;

    Ok(wait_delivery_ack(&mut stream)?)
}

/// Frame de salida para el portapapeles por LAN: mismo sobre que `text`,
/// con `kind: "clipboard"` para que el receptor lo distinga.
fn clipboard_wire_payload(pin: &str, id: &str, from: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "clipboard",
        "id": id,
        "from": from,
        "pin": pin,
        "text": text
    })
}

/// Envía el portapapeles a un dispositivo LAN pin-eado: mismas puertas y
/// acuse que `send_text`, con `kind: "clipboard"`. El texto pasa por el
/// mismo tope nativo (64_000 chars). Ok siempre responde "sent" para que el
/// frontend trate ambas rutas igual; los Err pasan honestos (incluye
/// "PIN_REQUERIDO" si el receptor rechazó por pin).
#[tauri::command]
async fn lan_send_clipboard(
    ip: String,
    pin: String,
    id: String,
    texto: String,
) -> Result<String, String> {
    use std::io::Write;

    let texto = clipboard::bound_clipboard_text(&texto)?;
    let payload = clipboard_wire_payload(&pin, &id, &device_name(), &texto);

    let mut stream = std::net::TcpStream::connect((ip.as_str(), 8787))
        .map_err(|e| format!("No se pudo conectar: {e}"))?;
    stream
        .write_all(format!("{payload}\n").as_bytes())
        .map_err(|e| format!("No se pudo enviar: {e}"))?;

    Ok(wait_delivery_ack(&mut stream).map(|_| "sent".to_string())?)
}

/// Espera (con timeout) el acuse del receptor en el mismo socket.
/// "delivered" si llegó el ack; "sent" si no respondió;
/// Err("PIN_REQUERIDO") si el receptor rechazó por PIN.
fn wait_delivery_ack(stream: &mut std::net::TcpStream) -> Result<String, String> {
    use std::io::{BufRead, BufReader};
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(_) if line.contains("\"rejected\"") => Err("PIN_REQUERIDO".into()),
        Ok(_) if line.contains("\"ack\"") => Ok("delivered".into()),
        _ => Ok("sent".into()),
    }
}

#[tauri::command]
async fn send_file(
    app: tauri::AppHandle,
    ip: String,
    pin: String,
    path: String,
    id: String,
) -> Result<String, String> {
    use std::io::{Read, Write};

    let src = std::path::Path::new(&path);
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| "Ruta inválida".to_string())?;
    let size = std::fs::metadata(src)
        .map_err(|e| format!("No se pudo leer el archivo: {e}"))?
        .len();

    let payload = serde_json::json!({
        "kind": "file",
        "id": id,
        "from": device_name(),
        "pin": pin,
        "name": name,
        "size": size
    });

    let mut stream = std::net::TcpStream::connect((ip.as_str(), 8787))
        .map_err(|e| format!("No se pudo conectar: {e}"))?;

    stream
        .write_all(format!("{payload}\n").as_bytes())
        .map_err(|e| format!("No se pudo enviar el encabezado: {e}"))?;

    let mut file = std::fs::File::open(src).map_err(|e| format!("No se pudo abrir: {e}"))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut left = size;
    while left > 0 {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("Lectura fallida: {e}"))?;
        if n == 0 {
            return Err("El archivo se acortó durante el envío".into());
        }
        stream
            .write_all(&buf[..n])
            .map_err(|e| format!("Envío fallido: {e}"))?;
        left -= n as u64;
    }

    // Permitir que el webview muestre este archivo como previsualización.
    let _ = app.asset_protocol_scope().allow_file(src);
    Ok(wait_delivery_ack(&mut stream)?)
}

/// Acuse de recibo: el receptor responde por el mismo socket.
#[tauri::command]
async fn send_ack(ip: String, payload: String) -> Result<(), String> {
    use std::io::Write;
    let mut stream = std::net::TcpStream::connect((ip.as_str(), 8787))
        .map_err(|e| format!("No se pudo conectar: {e}"))?;
    stream
        .write_all(format!("{payload}\n").as_bytes())
        .map_err(|e| format!("No se pudo enviar: {e}"))?;
    Ok(())
}

/// Sondeo rápido: ¿el dispositivo tiene su puerta abierta?
#[tauri::command]
async fn probe_port(ip: String, port: u16) -> bool {
    let targets = match (ip.as_str(), port).to_socket_addrs() {
        Ok(i) => i.collect::<Vec<_>>(),
        Err(_) => return false,
    };
    for addr in targets {
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(800))
            .is_ok()
        {
            return true;
        }
    }
    false
}

#[tauri::command]
fn get_download_folder(state: tauri::State<Arc<AppState>>) -> String {
    state.download_dir.lock().unwrap().clone()
}

#[tauri::command]
fn set_download_folder(path: String, state: tauri::State<Arc<AppState>>) -> Result<(), String> {
    std::fs::create_dir_all(&path).map_err(|e| format!("No se pudo crear la carpeta: {e}"))?;
    *state.download_dir.lock().unwrap() = path;
    Ok(())
}

/// Token de vigencia que viaja con cada append/patch (espejo DTO de
/// `history::HistToken`, que permanece sin serde).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistTokenDto {
    epoch: i64,
    rev: i64,
}

impl From<history::HistToken> for HistTokenDto {
    fn from(t: history::HistToken) -> Self {
        Self {
            epoch: t.epoch,
            rev: t.rev,
        }
    }
}

impl From<HistTokenDto> for history::HistToken {
    fn from(t: HistTokenDto) -> Self {
        Self {
            epoch: t.epoch,
            rev: t.rev,
        }
    }
}

impl From<&HistoryEntry> for history::HistoryEntry {
    fn from(e: &HistoryEntry) -> Self {
        Self {
            id: e.id.clone(),
            mine: e.mine,
            text: e.text.clone(),
            at: e.at,
            state: e.state.clone(),
            file_path: e.file_path.clone(),
            read: e.read,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryLoad {
    history: HashMap<String, Vec<HistoryEntry>>,
    epoch: i64,
    revs: HashMap<String, i64>,
    legacy_imported: bool,
    contacts: HashMap<String, String>,
}

fn read_history_map(conn: &Connection) -> Result<HashMap<String, Vec<HistoryEntry>>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, device_key, mine, text, at, state, file_path, read
             FROM messages ORDER BY at ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                HistoryEntry {
                    id: row.get(0)?,
                    mine: row.get(2)?,
                    text: row.get(3)?,
                    at: row.get(4)?,
                    state: row.get(5)?,
                    file_path: row.get(6)?,
                    read: row.get::<_, i64>(7)? != 0,
                },
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut map: HashMap<String, Vec<HistoryEntry>> = HashMap::new();
    for row in rows {
        let (key, entry) = row.map_err(|e| e.to_string())?;
        map.entry(key).or_default().push(entry);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_load_shape_includes_upserted_contacts() {
        let conn = Connection::open_in_memory()
            .map_err(|e| e.to_string())
            .unwrap();
        // Base tables: in production `open_history_db` creates them before
        // `ensure_history_schema` runs its ALTER/INDEX on `messages`.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (
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
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )
        .unwrap();
        history::ensure_history_schema(&conn).unwrap();
        history::upsert_contact(&conn, "u-1", "Ana").unwrap();
        history::upsert_contact(&conn, "u-2", "Beto").unwrap();

        let load = HistoryLoad {
            history: HashMap::new(),
            epoch: 0,
            revs: HashMap::new(),
            legacy_imported: false,
            contacts: history::load_contacts(&conn).unwrap(),
        };

        assert_eq!(
            load.contacts.get("hub:u-1").map(String::as_str),
            Some("Ana")
        );
        assert_eq!(
            load.contacts.get("hub:u-2").map(String::as_str),
            Some("Beto")
        );
    }

    #[test]
    fn clipboard_wire_frame_carries_kind_pin_id_and_text() {
        let payload = clipboard_wire_payload("4321", "id-1", "Emisor", "hola");
        assert_eq!(payload["kind"], "clipboard");
        assert_eq!(payload["pin"], "4321", "el pin pasa tal cual al wire");
        assert_eq!(payload["id"], "id-1");
        assert_eq!(payload["from"], "Emisor");
        assert_eq!(payload["text"], "hola");
    }

    #[tokio::test]
    async fn lan_send_clipboard_rejects_out_of_bound_text_before_connecting() {
        let over = "x".repeat(clipboard::MAX_CLIPBOARD_TEXT + 1);
        let err = lan_send_clipboard("127.0.0.1".into(), "1234".into(), "id-1".into(), over).await;
        assert_eq!(err, Err("invalid-text".to_string()));
        let blank = lan_send_clipboard(
            "127.0.0.1".into(),
            "1234".into(),
            "id-1".into(),
            "  \n ".into(),
        )
        .await;
        assert_eq!(blank, Err("invalid-text".to_string()));
    }

    #[test]
    fn ingest_lan_clipboard_emits_history_and_notify_and_writes_once() {
        let writes: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let (msg, notify) = ingest_lan_clipboard("Beto", "hola lan", |t| {
            writes.borrow_mut().push(t.to_string());
        });
        assert_eq!(msg.from, "Beto");
        assert_eq!(msg.text, "hola lan");
        assert_eq!(msg.id, "", "el id del wire lo agrega el caller");
        assert_eq!(
            notify,
            serde_json::json!({ "from": "Beto", "text": "hola lan" })
        );
        assert_eq!(*writes.borrow(), vec!["hola lan".to_string()]);
    }
}

#[tauri::command]
fn history_load(state: tauri::State<Arc<AppState>>) -> Result<HistoryLoad, String> {
    let conn = state.db.lock().unwrap();
    let history = read_history_map(&conn)?;
    let epoch = history::hist_epoch(&conn)?;
    let mut revs = HashMap::new();
    let rows = conn
        .prepare("SELECT key, value FROM settings WHERE key LIKE 'hist_rev:%'")
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map(|it| it.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    for (raw, value) in rows.map_err(|e| e.to_string())? {
        if let Some(key) = raw.strip_prefix(history::REV_PREFIX) {
            revs.insert(key.to_string(), value.parse::<i64>().unwrap_or(0));
        }
    }
    let legacy_imported = history::has_flag(&conn, history::LEGACY_FLAG)?;
    let contacts = history::load_contacts(&conn)?;
    Ok(HistoryLoad {
        history,
        epoch,
        revs,
        legacy_imported,
        contacts,
    })
}

/// Append de un mensaje propio: se invoca una sola vez por mensaje, con el
/// token vigente en el momento de la llamada.
#[tauri::command]
fn history_append(
    state: tauri::State<Arc<AppState>>,
    key: String,
    id: String,
    entry: HistoryEntry,
    token: HistTokenDto,
) -> Result<(), String> {
    if entry.id != id {
        return Err("id-mismatch".into());
    }
    let mut conn = state.db.lock().unwrap();
    let domain = history::HistoryEntry::from(&entry);
    history::append_message(&mut conn, &domain, &key, token.into())
}

/// Cambio de estado/lectura: nunca re-inserta; devuelve `false` si la fila
/// no existe (no-op honesto).
#[tauri::command]
fn history_patch_state(
    state: tauri::State<Arc<AppState>>,
    key: String,
    id: String,
    new_state: Option<String>,
    read: bool,
    token: HistTokenDto,
) -> Result<bool, String> {
    let mut conn = state.db.lock().unwrap();
    history::patch_message_state(
        &mut conn,
        &key,
        &id,
        new_state.as_deref(),
        read,
        token.into(),
    )
}

#[tauri::command]
fn history_delete_conversation(
    state: tauri::State<Arc<AppState>>,
    key: String,
) -> Result<HistTokenDto, String> {
    let mut conn = state.db.lock().unwrap();
    history::delete_conversation(&mut conn, &key).map(HistTokenDto::from)
}

#[tauri::command]
fn history_delete_all(state: tauri::State<Arc<AppState>>) -> Result<i64, String> {
    let mut conn = state.db.lock().unwrap();
    history::delete_all_history(&mut conn)
}

/// Importación única del snapshot de localStorage: flag + migración + inserts
/// + flag, todo en una sola transacción (ver `history::import_legacy`); si el
/// flag ya está, es un no-op que devuelve `false`. El frontend es el único
/// que toca localStorage: Rust no lo lee jamás.
#[tauri::command]
fn history_import_legacy(
    state: tauri::State<Arc<AppState>>,
    history: HashMap<String, Vec<HistoryEntry>>,
) -> Result<bool, String> {
    let mut conn = state.db.lock().unwrap();
    let domain: HashMap<String, Vec<history::HistoryEntry>> = history
        .into_iter()
        .map(|(key, entries)| {
            (
                key,
                entries.iter().map(history::HistoryEntry::from).collect(),
            )
        })
        .collect();
    history::import_legacy(&mut conn, &domain)
}

/// Abre (y crea) la base de datos del historial en la carpeta de datos de la app.
fn open_history_db(app: &tauri::AppHandle) -> Result<Connection, String> {
    let db_path = app
        .path()
        .app_data_dir()
        .map(|p| p.join("lanchat.db"))
        .unwrap_or_else(|_| std::env::temp_dir().join("lanchat.db"));
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(&db_path).map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS messages (
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
    .map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    // Migración idempotente del modelo append: columna content_hash + índice
    // de unicidad acotado. Se ejecuta en cada arranque y nunca escribe datos
    // ni flags: el flag de importación legacy vive solo en la importación.
    history::ensure_history_schema(&conn)?;
    Ok(conn)
}

/// Lee el PIN propio de la app; si no existe, genera uno de 6 dígitos y lo persiste.
fn read_or_create_pin(conn: &Connection) -> Result<String, String> {
    let existing: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'pin'", [], |r| {
            r.get(0)
        })
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(e.to_string())
            }
        })
        .map_err(|e| e.to_string())?;
    if let Some(pin) = existing {
        return Ok(pin);
    }
    let pin = new_random_pin();
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('pin', ?1)",
        rusqlite::params![pin],
    )
    .map_err(|e| e.to_string())?;
    Ok(pin)
}

fn new_random_pin() -> String {
    // MVP: pseudo-aleatorio a partir del reloj del sistema.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(482913);
    format!("{:06}", nanos % 1_000_000)
}

#[tauri::command]
fn regenerate_own_pin(state: tauri::State<Arc<AppState>>) -> Result<String, String> {
    let pin = new_random_pin();
    {
        let conn = state.db.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('pin', ?1)",
            rusqlite::params![pin],
        )
        .map_err(|e| e.to_string())?;
    }
    *state.own_pin.lock().unwrap() = pin.clone();
    *state.session_pin.lock().unwrap() = None;
    Ok(pin)
}

/// El código verificado se vuelve el pin propio del dispositivo (persistido).
fn adopt_pin(state: &AppState, pin: &str) -> Result<(), String> {
    {
        let conn = state.db.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('pin', ?1)",
            rusqlite::params![pin],
        )
        .map_err(|e| e.to_string())?;
    }
    *state.own_pin.lock().unwrap() = pin.to_string();
    *state.session_pin.lock().unwrap() = None;
    Ok(())
}

/// Handshake de emparejamiento: verifica que el PIN del otro dispositivo sea correcto.
#[tauri::command]
async fn pair_verify(
    _app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    ip: String,
    pin: String,
) -> Result<(), String> {
    use std::io::Write;

    let payload = serde_json::json!({ "kind": "pair-verify", "from": device_name(), "pin": pin });
    let mut stream = std::net::TcpStream::connect((ip.as_str(), 8787))
        .map_err(|e| format!("No se pudo conectar: {e}"))?;
    stream
        .write_all(format!("{payload}\n").as_bytes())
        .map_err(|e| format!("No se pudo enviar: {e}"))?;

    match wait_delivery_ack(&mut stream) {
        Ok(s) if s == "delivered" => {
            // Simetría: el iniciador también adopta el código como pin propio.
            adopt_pin(&state, &pin)?;
            Ok(())
        }
        Ok(_) => Err("No se pudo verificar el emparejamiento".into()),
        Err(e) => {
            if e.contains("PIN_REQUERIDO") {
                Err("PIN incorrecto".into())
            } else {
                Err(e)
            }
        }
    }
}
#[tauri::command]
fn send_web(state: tauri::State<Arc<AppState>>, name: String, text: String) -> Result<(), String> {
    let payload = serde_json::json!({
        "type": "chat",
        "from": device_name(),
        "to": name,
        "text": text
    });
    state
        .web_tx
        .send(payload.to_string())
        .map_err(|_| "No hay navegadores conectados".to_string())?;
    Ok(())
}

#[tauri::command]
fn send_web_file(
    state: tauri::State<Arc<AppState>>,
    name: String,
    path: String,
) -> Result<(), String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("No se pudo leer: {e}"))?;
    if bytes.len() > 25 * 1024 * 1024 {
        return Err("Para web el máximo es 25 MB".into());
    }
    let fname = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or("Ruta inválida")?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let fid = format!("d{nanos:x}");
    state
        .web_files
        .lock()
        .unwrap()
        .insert(fid.clone(), (fname.clone(), bytes));
    let payload = serde_json::json!({
        "type": "file",
        "from": device_name(),
        "to": name,
        "name": fname,
        "url": format!("/f/{fid}"),
        "size": state.web_files.lock().unwrap().get(&fid).map(|(_, b)| b.len()).unwrap_or(0)
    });
    state
        .web_tx
        .send(payload.to_string())
        .map_err(|_| "No hay navegadores conectados".to_string())?;
    Ok(())
}

#[tauri::command]
fn get_lan_url() -> String {
    let ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|s| {
            s.connect("8.8.8.8:80").ok()?;
            s.local_addr().ok()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "127.0.0.1".into());
    format!("http://{ip}:8789")
}

/// Estado completo del enlace hub (poll inicial + luego `hub-state`).
#[tauri::command]
fn hub_status(state: tauri::State<Arc<AppState>>) -> hub::HubStatus {
    state.hub.status()
}

/// Snapshot actual de pares del hub (poll inicial + luego `hub-presence`).
#[tauri::command]
fn hub_presence(state: tauri::State<Arc<AppState>>) -> hub::presence::HubPresenceSnapshot {
    state.hub.presence()
}

/// Snapshot de emparejamiento del hub (poll inicial + luego `hub-pairing`).
/// Los códigos viven SOLO aquí y en el evento: nunca viajan por el relay.
#[tauri::command]
fn hub_pairing(state: tauri::State<Arc<AppState>>) -> hub::pairing_wire::PairingSnapshot {
    state
        .hub
        .pairing
        .lock()
        .map(|pairing| pairing.snapshot())
        .unwrap_or_default()
}

/// Envía un texto de chat a un contacto del hub vía relay con acuse de
/// entrega acotado. Sin cola ni reintento: si falla, el frontend es dueño.
#[tauri::command]
async fn hub_send_text(
    state: tauri::State<'_, Arc<AppState>>,
    key: String,
    id: String,
    text: String,
) -> Result<String, String> {
    state.hub.send_text_to_contact(&key, &id, &text).await
}

/// Envía el portapapeles a un contacto del hub: mismas puertas y cola de
/// entrega que `hub_send_text`, con payload de tipo `clipboard`.
#[tauri::command]
async fn hub_send_clipboard(
    state: tauri::State<'_, Arc<AppState>>,
    key: String,
    id: String,
    text: String,
) -> Result<String, String> {
    state.hub.send_clipboard_to_contact(&key, &id, &text).await
}

/// Lee el texto del portapapeles del sistema con los límites compartidos:
/// trim, vacío rechazado, >64_000 chars rechazado. Solo texto plano.
#[tauri::command]
fn read_clipboard(app: tauri::AppHandle) -> Result<String, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let raw = app.clipboard().read_text().map_err(|e| e.to_string())?;
    clipboard::bound_clipboard_text(&raw)
}

/// Escribe texto plano en el portapapeles del sistema con los mismos
/// límites que la lectura.
#[tauri::command]
fn write_clipboard(app: tauri::AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let text = clipboard::bound_clipboard_text(&text)?;
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

/// Portapapeles entrante por LAN: misma fila de historial que un texto (el
/// caller emite `message-received` con el mensaje devuelto y el id del wire)
/// MÁS la copia local vía el seam `write` y el payload de
/// `lan-clipboard-received` para el toast del frontend. La entrada la agrega
/// SOLO message-received; el evento lan es solo aviso (append:false).
/// Fallo de copia es log-only: el mensaje queda tocable.
fn ingest_lan_clipboard<F: FnMut(&str)>(
    from: &str,
    text: &str,
    mut write: F,
) -> (ChatMessage, serde_json::Value) {
    write(text);
    (
        ChatMessage {
            from: from.to_string(),
            text: text.to_string(),
            id: String::new(),
        },
        serde_json::json!({ "from": from, "text": text }),
    )
}

/// Envía un archivo a un contacto del hub vía relay (base64, acote de
/// entrega acotado). Sin cola ni reintento: si falla, el frontend es dueño.
#[tauri::command]
async fn hub_send_file(
    state: tauri::State<'_, Arc<AppState>>,
    key: String,
    id: String,
    path: String,
) -> Result<String, String> {
    state.hub.send_file_to_contact(&key, &id, &path).await
}

/// Cancel nativo (botón UI): limpia SOLO el pending propio indicado; no envía
/// nada por la red. Devuelve si había un pending coincidente.
#[tauri::command]
fn hub_cancel_pairing(state: tauri::State<Arc<AppState>>, conn_id: String, req_id: String) -> bool {
    state
        .hub
        .pairing
        .lock()
        .map(|mut pairing| pairing.cancel(&conn_id, &req_id))
        .unwrap_or(false)
}

// ── Handlers del server LAN (axum) ─────────────────────────

async fn lan_index() -> Html<&'static str> {
    Html(LAN_CLIENT)
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    AxState(state): AxState<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(socket, state))
}

async fn ws_loop(socket: WebSocket, state: Arc<AppState>) {
    use futures_util::{SinkExt, StreamExt};

    let (mut sink, mut stream) = socket.split();

    // Hello: el navegador se presenta.
    let name = match stream.next().await {
        Some(Ok(Message::Text(t))) => {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap_or(serde_json::json!({}));
            v["name"].as_str().unwrap_or("navegador").to_string()
        }
        _ => return,
    };
    let name = {
        let mut sessions = state.web_sessions.lock().unwrap();
        let mut candidate = name.clone();
        let mut n = 1;
        while sessions.iter().any(|x| x == &candidate) {
            n += 1;
            candidate = format!("{name} ({n})");
        }
        sessions.push(candidate.clone());
        candidate
    };
    let _ = state.handle.emit(
        "web-sessions",
        serde_json::json!({ "list": state.web_sessions.lock().unwrap().clone() }),
    );

    let welcome = serde_json::json!({ "type": "welcome", "name": name });
    if sink
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Presentar: la lista de sesiones va a todos los navegadores.
    {
        let list = state.web_sessions.lock().unwrap().clone();
        let _ = state
            .web_tx
            .send(serde_json::json!({ "type": "sessions", "list": list }).to_string());
    }

    let mut rx = state.web_tx.subscribe();
    loop {
        tokio::select! {
            out = rx.recv() => {
                match out {
                    Ok(text) => {
                        let v: serde_json::Value = serde_json::from_str(&text)
                            .unwrap_or(serde_json::json!({}));
                        let to = v["to"].as_str();
                        if to.is_none() || to == Some(name.as_str()) {
                            if sink.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(t))) => {
                        let v: serde_json::Value = serde_json::from_str(&t)
                            .unwrap_or(serde_json::json!({}));
                        if v["type"] == "chat" {
                            if !*state.web_paired.lock().unwrap() {
                                continue;
                            }
                            let nanos = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_nanos())
                                .unwrap_or(0);
                            let msg = ChatMessage {
                                from: format!("web:{name}"),
                                text: v["text"].as_str().unwrap_or("").to_string(),
                                id: format!("web-{nanos:x}"),
                            };
                            // La app guarda el historial.
                            let _ = state.handle.emit("message-received", msg);
                            // Y el destino (o todos) lo recibe en su navegador.
                            let payload = serde_json::json!({
                                "type": "chat",
                                "from": name,
                                "to": v["to"],
                                "text": v["text"].as_str().unwrap_or("")
                            });
                            let _ = state.web_tx.send(payload.to_string());
                        }
                    }
                    _ => break,
                }
            }
        }
    }

    {
        let mut sessions = state.web_sessions.lock().unwrap();
        sessions.retain(|x| x != &name);
        let list = sessions.clone();
        drop(sessions);
        let _ = state
            .handle
            .emit("web-sessions", serde_json::json!({ "list": list }));
        let _ = state
            .web_tx
            .send(serde_json::json!({ "type": "sessions", "list": list }).to_string());
    }
    let _ = (&mut sink, &mut stream);
}

async fn upload(
    AxState(state): AxState<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !*state.web_paired.lock().unwrap() {
        return Json(serde_json::json!({ "error": "no emparejado" }));
    }
    let from = q.get("from").cloned().unwrap_or_else(|| "navegador".into());
    let raw_name = q.get("name").cloned().unwrap_or_else(|| "archivo".into());
    let name = std::path::Path::new(&raw_name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "archivo".into());

    let dir = state.download_dir.lock().unwrap().clone();
    let _ = std::fs::create_dir_all(&dir);
    let dest = std::path::Path::new(&dir).join(&name);
    if let Err(e) = std::fs::write(&dest, &body) {
        return Json(serde_json::json!({ "error": format!("{e}") }));
    }
    let _ = state.handle.asset_protocol_scope().allow_file(&dest);
    let _ = state.handle.emit(
        "file-received",
        serde_json::json!({
            "from": format!("web:{from}"),
            "name": name,
            "path": dest.to_string_lossy(),
            "size": body.len(),
            "id": ""
        }),
    );

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let fid = format!("u{nanos:x}");
    state
        .web_files
        .lock()
        .unwrap()
        .insert(fid.clone(), (name.clone(), body.to_vec()));

    let payload = serde_json::json!({
        "type": "file",
        "from": format!("web:{from}"),
        "name": name,
        "url": format!("/f/{fid}"),
        "size": body.len(),
        "at": nanos as u64 / 1_000_000
    });
    let _ = state.web_tx.send(payload.to_string());
    Json(serde_json::json!({ "ok": true, "url": format!("/f/{fid}") }))
}

async fn serve_file(
    AxState(state): AxState<Arc<AppState>>,
    AxPath(fid): AxPath<String>,
) -> impl IntoResponse {
    let file = state.web_files.lock().unwrap().get(&fid).cloned();
    match file {
        Some((name, bytes)) => {
            let headers = [(
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            )];
            (headers, bytes).into_response()
        }
        None => (
            [(axum::http::header::CONTENT_TYPE, "text/plain".to_string())],
            "expirado".to_string(),
        )
            .into_response(),
    }
}

async fn api_devices(AxState(_state): AxState<Arc<AppState>>) -> impl IntoResponse {
    let local_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|s| {
            s.connect("8.8.8.8:80").ok()?;
            s.local_addr().ok()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "127.0.0.1".into());

    let hub = serde_json::json!({
        "name": device_name(),
        "ip": local_ip,
        "hub": true
    });
    let mut devices = vec![hub];
    devices.extend(
        scan_lan_devices()
            .into_iter()
            .map(|d| serde_json::json!({ "name": d.name, "ip": d.ip, "service": d.service })),
    );
    Json(serde_json::json!({ "devices": devices }))
}

async fn api_pair_request(AxState(state): AxState<Arc<AppState>>) -> impl IntoResponse {
    let mut session = state.session_pin.lock().unwrap();
    let valid = session
        .as_ref()
        .map(|(_, t)| t.elapsed() < std::time::Duration::from_secs(120))
        .unwrap_or(false);
    if !valid {
        *session = Some((new_random_pin(), std::time::Instant::now()));
    }
    let _ = state.handle.emit(
        "pair-request",
        serde_json::json!({ "from": "navegador", "code": session.as_ref().unwrap().0.clone() }),
    );
    Json(serde_json::json!({ "ok": true }))
}

async fn api_pair(
    AxState(state): AxState<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let code = body["code"].as_str().unwrap_or("").to_string();
    let session_ok = state
        .session_pin
        .lock()
        .unwrap()
        .as_ref()
        .map(|(c, t)| c == &code && t.elapsed() < std::time::Duration::from_secs(120))
        .unwrap_or(false);
    let own_ok = code == state.own_pin.lock().unwrap().as_str();
    if !session_ok && !own_ok {
        return Json(serde_json::json!({ "ok": false, "error": "PIN incorrecto" }));
    }
    let _ = adopt_pin(&state, &code);
    *state.web_paired.lock().unwrap() = true;
    let _ = state.handle.emit(
        "pair-done",
        serde_json::json!({ "from": "navegador", "code": code }),
    );
    Json(serde_json::json!({ "ok": true }))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            // Carpeta de descargas por defecto: ~/Downloads/lan-chat
            let default_dir = app
                .path()
                .download_dir()
                .map(|p| p.join("lan-chat"))
                .unwrap_or_else(|_| std::env::temp_dir().join("lan-chat"));
            let _ = std::fs::create_dir_all(&default_dir);

            let conn = open_history_db(app.handle())?;
            // Barrido de arranque: staged files que ninguna fila referencia
            // (ventanas de crash) se eliminan; los referenciados quedan
            // intactos. Solo log: el conteo no es un error.
            let swept = hub::inbox::sweep_stage_dir(&conn, &default_dir);
            if swept > 0 {
                eprintln!("Hub: {swept} archivos staged huérfanos eliminados al arrancar");
            }
            // Re-habilitar en el scope de assets los archivos que el historial
            // referencia: los grants allow_file viven solo en memoria, así que
            // sin esto toda imagen del historial (LAN + hub) se rompe tras un
            // reinicio. Bounded loop sobre los paths DISTINCT de la DB; los
            // inexistentes se saltan en silencio. Solo log: best-effort.
            let regranted = history::file_paths(&conn)
                .into_iter()
                .filter(|p| std::path::Path::new(p).exists())
                .filter(|p| app.asset_protocol_scope().allow_file(p).is_ok())
                .count();
            if regranted > 0 {
                eprintln!(
                    "Historial: {regranted} archivos re-habilitados en el scope de assets"
                );
            }
            let own_pin = read_or_create_pin(&conn)?;
            let (web_tx, _) = tokio::sync::broadcast::channel::<String>(64);

            // Hub: identidad estable -> estado compartido -> loop del cliente.
            // Un fallo de identidad deshabilita SOLO el hub (estado + log);
            // el resto de la app (LAN, historial) sigue igual. Sin
            // LANCHAT_HUB_URL el hub arranca deshabilitado (hub-not-configured):
            // el default público es LAN-only.
            let hub_url =
                hub::identity::resolve_hub_url(std::env::var("LANCHAT_HUB_URL").ok().as_deref());
            let hub_identity = hub::decide_identity(hub::identity::read_or_create_device_id(&conn));
            if let hub::HubIdentity::Disabled { reason } = &hub_identity {
                eprintln!("Hub deshabilitado: {reason}");
            }
            let hub_state_app = app.handle().clone();
            let hub_listener_url = hub_url.clone().unwrap_or_default();
            let hub_listener: hub::HubListener = Arc::new(move |event| {
                match event {
                    // Presencia: el snapshot de pares va por `hub-presence`;
                    // nunca se reporta como estado del enlace.
                    hub::HubEvent::PresenceChanged { snapshot } => {
                        let _ = hub_state_app.emit("hub-presence", snapshot.clone());
                    }
                    hub::HubEvent::Connected { .. } | hub::HubEvent::Disconnected { .. } => {
                        if let Some(status) = hub::hub_event_payload(&hub_listener_url, event) {
                            let _ = hub_state_app.emit("hub-state", status);
                        }
                    }
                }
            });
            let hub_startup =
                hub::build_startup(hub_url, device_name(), hub_identity, Some(hub_listener));
            let hub_shared = Arc::new(hub_startup.shared);
            // Pairing snapshots surface to the frontend as `hub-pairing`.
            {
                let pairing_app = app.handle().clone();
                if let Ok(mut pairing) = hub_shared.pairing.lock() {
                    pairing.set_emitter(Arc::new(move |snapshot| {
                        let _ = pairing_app.emit("hub-pairing", snapshot);
                    }));
                }
            }
            let settings = Arc::new(AppState {
                download_dir: Mutex::new(default_dir.to_string_lossy().to_string()),
                db: Mutex::new(conn),
                own_pin: Mutex::new(own_pin),
                session_pin: Mutex::new(None),
                handle: app.handle().clone(),
                web_tx,
                web_files: Mutex::new(HashMap::new()),
                web_sessions: Mutex::new(Vec::new()),
                web_paired: Mutex::new(false),
                hub: hub_shared.clone(),
                hub_shutdown: hub_startup.shutdown,
            });
            app.manage(settings.clone());

            // Atajo global Cmd/Ctrl+Shift+V: pide al frontend compartir el
            // portapapeles vía `clipboard-send-requested`. Un fallo de
            // registro (OS lo niega, ya tomado por otra app) es solo log:
            // la app arranca igual sin el atajo.
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::GlobalShortcutExt;
                const CLIPBOARD_SHORTCUT: &str = "CmdOrCtrl+Shift+V";
                app.handle()
                    .plugin(tauri_plugin_global_shortcut::Builder::new().build())?;
                let gs = app.global_shortcut();
                if !gs.is_registered(CLIPBOARD_SHORTCUT) {
                    if let Err(e) = gs.on_shortcut(CLIPBOARD_SHORTCUT, |app, _shortcut, event| {
                        if clipboard::clipboard_event_should_emit(event.state) {
                            let _ = app.emit("clipboard-send-requested", serde_json::json!({}));
                        }
                    }) {
                        eprintln!(
                            "Atajo global {CLIPBOARD_SHORTCUT} no registrado (no fatal): {e}"
                        );
                    }
                } else {
                    eprintln!("Atajo global {CLIPBOARD_SHORTCUT} ya registrado; se omite");
                }
            }

            // Seams del hub: acceso corto a la DB, emisión de mensajes
            // aceptados y persistencia de contactos. Instalados UNA vez,
            // ANTES de arrancar el loop del cliente, así ningún frame se
            // procesa sin sus seams. `contact_persist` y `chat_emit` son
            // sync y de corta vida; el mutex de la DB nunca cruza un await.
            let hub_hooks = hub::HubHooks {
                with_db: {
                    let settings = settings.clone();
                    Arc::new(move |run: &mut dyn FnMut(&mut Connection)| {
                        let Ok(mut conn) = settings.db.lock() else {
                            return;
                        };
                        run(&mut conn);
                    })
                },
                chat_emit: {
                    let app = app.handle().clone();
                    Arc::new(move |msg: &hub::inbox::ReceivedChat| {
                        let _ = app.emit("hub-message-received", msg.clone());
                    })
                },
                clipboard_emit: {
                    let app = app.handle().clone();
                    Arc::new(move |msg: &hub::inbox::ReceivedClipboard| {
                        let _ = app.emit("hub-clipboard-received", msg.clone());
                    })
                },
                contact_persist: {
                    let settings = settings.clone();
                    Arc::new(move |key: &str, name: &str| {
                        let Some(uuid) = key.strip_prefix(history::HUB_KEY_PREFIX) else {
                            eprintln!("Contacto hub con clave inesperada: {key}");
                            return;
                        };
                        let Ok(conn) = settings.db.lock() else {
                            eprintln!("No se pudo acceder a la base para el contacto del hub");
                            return;
                        };
                        if let Err(e) = history::upsert_contact(&conn, uuid, name) {
                            eprintln!("No se pudo persistir el contacto del hub: {e}");
                        }
                    })
                },
                download_dir: {
                    let settings = settings.clone();
                    Arc::new(move || settings.download_dir.lock().unwrap().clone())
                },
                file_received: {
                    let app = app.handle().clone();
                    Arc::new(move |f: &hub::inbox::ReceivedFile| {
                        // Igual que la recepción LAN: sin este permiso el
                        // asset protocol le niega el archivo al webview y la
                        // imagen se renderiza rota.
                        let _ = app
                            .asset_protocol_scope()
                            .allow_file(std::path::Path::new(&f.path));
                        let _ = app.emit("hub-file-received", f.clone());
                    })
                },
                file_error: {
                    let app = app.handle().clone();
                    Arc::new(move |e: &hub::inbox::FileError| {
                        let _ = app.emit("hub-file-error", e.clone());
                    })
                },
            };
            // El primer `set_hooks` gana: en el arranque se instala una vez.
            let _ = hub_shared.set_hooks(hub_hooks);

            if let Some((hub_config, hub_shutdown_rx)) = hub_startup.client {
                // Runtime async propio de Tauri, sin bloquear la UI; el mutex
                // de HubShared nunca se cruza con un await.
                tauri::async_runtime::spawn(hub::client::run_hub_client(
                    hub_shared.clone(),
                    hub_config,
                    hub_shutdown_rx,
                ));
            }

            // Puente LAN: navegadores como terminales del escritorio.
            {
                let settings = settings.clone();
                tauri::async_runtime::spawn(async move {
                    let lan = Router::new()
                        .route("/", get(lan_index))
                        .route("/ws", get(ws_upgrade))
                        .route("/upload", post(upload))
                        .route("/f/{id}", get(serve_file))
                        .route("/api/devices", get(api_devices))
                        .route("/api/pair-request", post(api_pair_request))
                        .route("/api/pair", post(api_pair))
                        .layer(DefaultBodyLimit::max(26 * 1024 * 1024))
                        .with_state(settings);
                    if let Ok(listener) =
                        tokio::net::TcpListener::bind("0.0.0.0:8789").await
                    {
                        println!("LAN web en http://0.0.0.0:8789");
                        let _ = axum::serve(listener, lan).await;
                    }
                });
            }

            let mdns = ServiceDaemon::new().expect("No se pudo crear el daemon mDNS");
            let name = device_name();

            let service_info = ServiceInfo::new(
                "_lanchat._tcp.local.",
                &name,
                &format!("{name}.local."), // el host EXIGE el sufijo .local.
                "",
                8787,
                None,
            )
            .expect("Info de servicio inválida")
            .enable_addr_auto();

            mdns.register(service_info).expect("No se pudo anunciar");
            let handle = app.handle().clone();

            std::thread::spawn(move || {
                let listener = std::net::TcpListener::bind("0.0.0.0:8787")
                    .expect("No se pudo abrir el puerto 8787");
                println!("LAN-Chat escuchando en el puerto 8787");

                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let mut reader = std::io::BufReader::new(stream);
                    use std::io::{BufRead, Read, Write};

                    let mut header = String::new();
                    loop {
                        header.clear();
                        match reader.read_line(&mut header) {
                            Ok(0) => break, // conexión cerrada
                            Ok(_) => {}
                            Err(_) => break,
                        }
                        let line = header.trim_end_matches(['\n', '\r']);
                        if line.is_empty() {
                            continue;
                        }

                        let parsed: serde_json::Value = match serde_json::from_str(line) {
                            Ok(v) => v,
                            Err(_) => {
                                let _ = handle.emit(
                                    "message-received",
                                    ChatMessage {
                                        from: "?".into(),
                                        text: line.to_string(),
                                        id: String::new(),
                                    },
                                );
                                continue;
                            }
                        };

                        let from = parsed["from"].as_str().unwrap_or("?").to_string();
                        let id = parsed["id"].as_str().unwrap_or("").to_string();

                        // Candado: texto y archivos exigen un pin válido del receptor.
                        let kind = parsed["kind"].as_str().unwrap_or("text").to_string();
                        let sent_pin = parsed["pin"].as_str().unwrap_or("");
                        let own_pin = settings.own_pin.lock().unwrap().clone();
                        if (kind == "text" || kind == "file" || kind == "clipboard")
                            && sent_pin != own_pin
                        {
                            println!("⚠️ Conexión rechazada por PIN (de {from})");
                            let _ = reader
                                .get_ref()
                                .write_all(b"{\"kind\":\"rejected\",\"reason\":\"pin\"}\n");
                            continue;
                        }

                        match parsed["kind"].as_str().unwrap_or("text") {
                            "text" => {
                                let msg = ChatMessage {
                                    from: from.clone(),
                                    text: parsed["text"].as_str().unwrap_or("").to_string(),
                                    id: id.clone(),
                                };
                                println!("{} dice: {}", msg.from, msg.text);
                                let _ = handle.emit("message-received", msg);
                                if !id.is_empty() {
                                    let payload =
                                        serde_json::json!({ "kind": "ack", "id": id.clone() });
                                    let _ = reader
                                        .get_ref()
                                        .write_all(format!("{payload}\n").as_bytes());
                                }
                            }
                            "file" => {
                                let name =
                                    parsed["name"].as_str().unwrap_or("archivo").to_string();
                                let size = parsed["size"].as_u64().unwrap_or(0);
                                // Nunca confiar en rutas del exterior: solo el nombre final.
                                let safe_name = std::path::Path::new(&name)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "archivo".to_string());

                                let dir = settings.download_dir.lock().unwrap().clone();
                                if std::fs::create_dir_all(&dir).is_err() {
                                    eprintln!("No se pudo crear la carpeta de descargas");
                                    continue;
                                }
                                let dest = std::path::Path::new(&dir).join(safe_name);
                                let mut out = match std::fs::File::create(&dest) {
                                    Ok(f) => f,
                                    Err(e) => {
                                        eprintln!("No se pudo crear el archivo: {e}");
                                        continue;
                                    }
                                };

                                let mut buf = vec![0u8; 64 * 1024];
                                let mut left = size;
                                let mut ok = true;
                                while left > 0 {
                                    let chunk = (left as usize).min(buf.len());
                                    if reader.read_exact(&mut buf[..chunk]).is_err() {
                                        ok = false;
                                        break;
                                    }
                                    if out.write_all(&buf[..chunk]).is_err() {
                                        ok = false;
                                        break;
                                    }
                                    left -= chunk as u64;
                                }

                                if ok {
                                    println!("Archivo recibido: {}", dest.display());
                                    let _ = handle.asset_protocol_scope().allow_file(&dest);
                                    if !id.is_empty() {
                                        let payload =
                                            serde_json::json!({ "kind": "ack", "id": id });
                                        let _ = reader
                                            .get_ref()
                                            .write_all(format!("{payload}\n").as_bytes());
                                    }
                                    let _ = handle.emit(
                                        "file-received",
                                        FileNotice {
                                            from,
                                            name,
                                            path: dest.to_string_lossy().to_string(),
                                            size,
                                            id,
                                        },
                                    );
                                } else {
                                    eprintln!("Transferencia incompleta: {}", dest.display());
                                    let _ = std::fs::remove_file(&dest);
                                }
                            }
                            "clipboard" => {
                                let text = parsed["text"].as_str().unwrap_or("").to_string();
                                let (msg, notify) = ingest_lan_clipboard(&from, &text, |t| {
                                    // Copia local: write_clipboard ya acota y
                                    // rechaza texto inválido; falla log-only.
                                    if let Err(e) = write_clipboard(handle.clone(), t.to_string())
                                    {
                                        eprintln!("No se pudo copiar el portapapeles LAN: {e}");
                                    }
                                });
                                println!("{} compartió portapapeles", msg.from);
                                let _ = handle.emit(
                                    "message-received",
                                    ChatMessage {
                                        from: msg.from,
                                        text: msg.text,
                                        id: id.clone(),
                                    },
                                );
                                if !id.is_empty() {
                                    let payload =
                                        serde_json::json!({ "kind": "ack", "id": id.clone() });
                                    let _ = reader
                                        .get_ref()
                                        .write_all(format!("{payload}\n").as_bytes());
                                }
                                let _ = handle.emit("lan-clipboard-received", notify);
                            }
                            "read-ack" => {
                                let ids: Vec<String> = parsed["ids"]
                                    .as_array()
                                    .map(|a| {
                                        a.iter()
                                            .filter_map(|v| v.as_str().map(String::from))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                let peer = reader
                                    .get_ref()
                                    .peer_addr()
                                    .map(|a| a.ip().to_string())
                                    .unwrap_or_default();
                                println!(
                                    "read-ack de {peer}: {} ids",
                                    ids.len()
                                );
                                let _ = handle.emit(
                                    "read-ack",
                                    serde_json::json!({ "from": peer, "ids": ids }),
                                );
                            }
                            "pair-verify" => {
                                let session_ok = settings
                                    .session_pin
                                    .lock()
                                    .unwrap()
                                    .as_ref()
                                    .map(|(code, t)| {
                                        code == &sent_pin
                                            && t.elapsed() < std::time::Duration::from_secs(120)
                                    })
                                    .unwrap_or(false);
                                let own_ok = sent_pin == own_pin;

                                if !session_ok && !own_ok {
                                    let _ = reader.get_ref().write_all(
                                        b"{\"kind\":\"rejected\",\"reason\":\"pin\"}\n",
                                    );
                                    println!("⚠️ PIN incorrecto en pair-verify (de {from})");
                                    continue;
                                }

                                // El código de sesión se vuelve el pin de la pareja.
                                if session_ok {
                                    let _ = settings.db.lock().unwrap().execute(
                                        "INSERT OR REPLACE INTO settings (key, value) VALUES ('pin', ?1)",
                                        rusqlite::params![sent_pin],
                                    );
                                    *settings.own_pin.lock().unwrap() = sent_pin.to_string();
                                    *settings.session_pin.lock().unwrap() = None;
                                }

                                let payload = serde_json::json!({ "kind": "ack", "id": id });
                                let _ = reader
                                    .get_ref()
                                    .write_all(format!("{payload}\n").as_bytes());
                                let _ = handle.emit(
                                    "pair-done",
                                    serde_json::json!({ "from": from, "code": sent_pin }),
                                );
                                println!("🔗 Emparejamiento verificado desde {from}");
                            }

                            // Solicitud de vinculación: mostrar código de sesión (2 min).
                            "pair-request" => {
                                let mut session = settings.session_pin.lock().unwrap();
                                let valid = session
                                    .as_ref()
                                    .map(|(_, t)| {
                                        t.elapsed() < std::time::Duration::from_secs(120)
                                    })
                                    .unwrap_or(false);
                                if !valid {
                                    *session =
                                        Some((new_random_pin(), std::time::Instant::now()));
                                }
                                let code = session.as_ref().unwrap().0.clone();
                                drop(session);
                                println!("🔗 Solicitud de emparejamiento de {from}");
                                let _ = handle.emit(
                                    "pair-request",
                                    serde_json::json!({ "from": from, "code": code }),
                                );
                            }
                            _ => {}
                        }
                    }
                }
            });

            std::mem::forget(mdns);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            discover_devices,
            send_text,
            send_file,
            lan_send_clipboard,
            send_ack,
            probe_port,
            get_download_folder,
            set_download_folder,
            history_load,
            history_append,
            history_patch_state,
            history_delete_conversation,
            history_delete_all,
            history_import_legacy,            regenerate_own_pin,
            pair_verify,
            send_web,
            send_web_file,
            get_lan_url,
            hub_status,
            hub_presence,
            hub_pairing,
            hub_cancel_pairing,
            hub_send_text,
            hub_send_file,
            hub_send_clipboard,
            read_clipboard,
            write_clipboard
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Salida de la app: frenar el loop del hub aunque esté
                // esperando en connect o backoff.
                let _ = app_handle.state::<Arc<AppState>>().hub_shutdown.send(true);
            }
        });
}
