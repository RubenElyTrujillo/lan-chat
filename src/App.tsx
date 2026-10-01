import { useState } from "react";
import reactLogo from "./assets/react.svg";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

interface DiscoveredDevice {
  name: string;
  ip: string;
  service: string;
}

function App() {
  const [devices, setDevices] = useState<DiscoveredDevice[]>([]);
  const [selected, setSelected] = useState<DiscoveredDevice | null>(null);
  const [texto, setTexto] = useState("");

  async function discoverDevices() {
    const resultados = await invoke("discover_devices");
    setDevices(resultados as DiscoveredDevice[]);
  }

  async function sendText() {
    if (!selected) return;
    try {
      await invoke("send_text", { ip: selected.ip, texto });
      setTexto("");
    } catch (e) {
      console.error("Error al enviar:", e);
    }
  }

  return (
    <main className="container">
      <div className="card">
        <h1>LAN Chat</h1>
        <p>Descubre dispositivos en la red local usando mDNS.</p>
        <button onClick={discoverDevices}>Descubrir Dispositivos</button>
      </div>
      <ul>
        {devices.map((device) => (
          <li
            key={device.ip + device.name}
            onClick={() => setSelected(device)}
            style={{ cursor: "pointer", fontWeight: device === selected ? "bold" : "normal" }}
          >
            <div className="row">
              <input
                value={texto}
                onChange={(e) => setTexto(e.currentTarget.value)}
                placeholder="Mensaje..."
              />
              <button onClick={sendText} disabled={!selected}>
                Enviar a {selected ? selected.name : "—"}
              </button>
            </div>
          </li>
        ))}
      </ul>
    </main>
  );
}

export default App;
