// LAN-Chat — motor: descubrimiento mDNS, transferencia de texto y archivos por TCP local.
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

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

#[tauri::command]
async fn discover_devices() -> Vec<DiscoveredDevice> {
    let mdns = ServiceDaemon::new().expect("No se pudo crear el daemon mDNS");
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
async fn send_text(
    ip: String,
    pin: String,
    texto: String,
    id: String,
) -> Result<String, String> {
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
async fn send_ack(ip: String, payload: String) -> Result<(), String> {    use std::io::Write;
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

#[tauri::command]
fn load_history(
    state: tauri::State<Arc<AppState>>,
) -> Result<HashMap<String, Vec<HistoryEntry>>, String> {
    let conn = state.db.lock().unwrap();
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

#[tauri::command]
fn save_history(
    state: tauri::State<Arc<AppState>>,
    history: HashMap<String, Vec<HistoryEntry>>,
) -> Result<(), String> {
    let mut conn = state.db.lock().unwrap();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM messages", [])
        .map_err(|e| e.to_string())?;
    for (device_key, entries) in &history {
        for e in entries {
            tx.execute(
                "INSERT OR REPLACE INTO messages
                 (id, device_key, mine, text, at, state, file_path, read)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![e.id, device_key, e.mine, e.text, e.at, e.state, e.file_path, e.read],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
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
    app: tauri::AppHandle,
    state: tauri::State<Arc<AppState>>,
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
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Carpeta de descargas por defecto: ~/Downloads/lan-chat
            let default_dir = app
                .path()
                .download_dir()
                .map(|p| p.join("lan-chat"))
                .unwrap_or_else(|_| std::env::temp_dir().join("lan-chat"));
            let _ = std::fs::create_dir_all(&default_dir);

            let conn = open_history_db(app.handle())?;
            let own_pin = read_or_create_pin(&conn)?;

            let settings = Arc::new(AppState {
                download_dir: Mutex::new(default_dir.to_string_lossy().to_string()),
                db: Mutex::new(conn),
                own_pin: Mutex::new(own_pin),
                session_pin: Mutex::new(None),
            });
            app.manage(settings.clone());

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
                        if (kind == "text" || kind == "file") && sent_pin != own_pin {
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
            send_ack,
            probe_port,
            get_download_folder,
            set_download_folder,
            load_history,
            save_history,
            regenerate_own_pin,
            pair_verify
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
