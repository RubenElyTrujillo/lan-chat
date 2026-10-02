// LAN-Chat — motor: descubrimiento mDNS, transferencia de texto y archivos por TCP local.
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
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
}

#[derive(Serialize, Clone)]
struct FileNotice {
    from: String,
    name: String,
    path: String,
    size: u64,
}

/// Ajustes compartidos de la app (vivos mientras la app viva).
struct AppState {
    download_dir: Mutex<String>,
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
async fn send_text(ip: String, texto: String) -> Result<(), String> {
    use std::io::Write;

    let payload = serde_json::json!({ "kind": "text", "from": device_name(), "text": texto });

    let mut stream = std::net::TcpStream::connect((ip.as_str(), 8787))
        .map_err(|e| format!("No se pudo conectar: {e}"))?;
    stream
        .write_all(format!("{payload}\n").as_bytes())
        .map_err(|e| format!("No se pudo enviar: {e}"))?;
    Ok(())
}

#[tauri::command]
async fn send_file(app: tauri::AppHandle, ip: String, path: String) -> Result<(), String> {
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
        "from": device_name(),
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
    Ok(())
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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

            let settings = Arc::new(AppState {
                download_dir: Mutex::new(default_dir.to_string_lossy().to_string()),
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
                                    },
                                );
                                continue;
                            }
                        };

                        let from = parsed["from"].as_str().unwrap_or("?").to_string();

                        match parsed["kind"].as_str().unwrap_or("text") {
                            "text" => {
                                let msg = ChatMessage {
                                    from: from.clone(),
                                    text: parsed["text"].as_str().unwrap_or("").to_string(),
                                };
                                println!("{} dice: {}", msg.from, msg.text);
                                let _ = handle.emit("message-received", msg);
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
                                    let _ = handle.emit(
                                        "file-received",
                                        FileNotice {
                                            from,
                                            name,
                                            path: dest.to_string_lossy().to_string(),
                                            size,
                                        },
                                    );
                                } else {
                                    eprintln!("Transferencia incompleta: {}", dest.display());
                                    let _ = std::fs::remove_file(&dest);
                                }
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
            get_download_folder,
            set_download_folder
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
