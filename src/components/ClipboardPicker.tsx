import { useEffect } from "react";
import type { ClipboardTargetOption } from "../lib/clipboard-core";

// Elegidor de destino del portapapeles: overlay liviano con los contactos
// online. Esc y click afuera cierran; no interrumpe nada más.
export function ClipboardPicker({
  options,
  onPick,
  onClose,
}: {
  options: ClipboardTargetOption[];
  onPick: (option: ClipboardTargetOption) => void;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="pin-overlay"
      role="dialog"
      aria-label="Elegir destino del portapapeles"
      onClick={onClose}
    >
      <div className="picker-card" onClick={(e) => e.stopPropagation()}>
        <h3>Enviar portapapeles a…</h3>
        <ul className="picker-list">
          {options.map((o) => (
            <li key={o.key}>
              <button
                type="button"
                className="picker-item"
                onClick={() => onPick(o)}
              >
                <span className="picker-dot" aria-hidden />
                {o.name}
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
