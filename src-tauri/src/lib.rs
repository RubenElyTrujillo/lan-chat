// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;

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

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn discover_devices() -> Vec<DiscoveredDevice> {
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
fn send_text(ip: String, texto: String) -> Result<(), String> {
    use std::io::Write;

    let mut stream = std::net::TcpStream::connect((ip.as_str(), 8787))
        .map_err(|e| format!("No se pudo conectar: {e}"))?;

    let mensaje = format!("{texto}\n");
    stream
        .write_all(mensaje.as_bytes())
        .map_err(|e| format!("No se pudo enviar: {e}"))?;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|_app| {
            let mdns = ServiceDaemon::new().expect("No se pudo crear el daemon mDNS");
            let name = device_name();

            let service_info = ServiceInfo::new(
                "_lanchat._tcp.local.",
                &name,
                &format!("{name}.local."), // ← el host EXIGE el sufijo .local.
                "",
                8787,
                None,
            )
            .expect("Info de servicio inválida")
            .enable_addr_auto();

            mdns.register(service_info).expect("No se pudo anunciar");

            std::thread::spawn(|| {
                let listener = std::net::TcpListener::bind("0.0.0.0:8787")
                    .expect("No se pudo abrir el puerto 8787");
                println!("LAN-Chat escuchando en el puerto 8787");

                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let reader = std::io::BufReader::new(stream);
                    use std::io::BufRead;
                    for line in reader.lines() {
                        match line {
                            Ok(texto) => println!("Mensaje recibido: {texto}"),
                            Err(_) => break,
                        }
                    }
                }
            });

            std::mem::forget(mdns);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet, discover_devices, send_text])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
