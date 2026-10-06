import { useCallback, useRef, useState } from "react";
import { MoreHorizontal } from "lucide-react";
import { useDismiss } from "./useDismiss";

export interface MenuItem {
  label: string;
  danger?: boolean;
  action: () => void;
}

export function Menu({ items, label }: { items: MenuItem[]; label: string }) {
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState<number | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const close = useCallback(() => {
    setOpen(false);
    setConfirming(null);
  }, []);
  useDismiss(rootRef, open, close);

  return (
    <div className="menu" ref={rootRef}>
      <button
        type="button"
        className="icon-btn"
        aria-label={label}
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <MoreHorizontal size={18} />
      </button>
      {open && (
        <div className="menu-pop" role="menu">
          {items.map((item, i) =>
            confirming === i ? (
              <button
                key={item.label}
                type="button"
                role="menuitem"
                className={`menu-item is-danger ${item.danger ? "" : "is-plain"}`}
                onClick={() => {
                  item.action();
                  close();
                }}
              >
                Confirmar: {item.label.toLowerCase()}
              </button>
            ) : (
              <button
                key={item.label}
                type="button"
                role="menuitem"
                className={`menu-item ${item.danger ? "is-danger-soft" : ""}`}
                onClick={() => {
                  if (item.danger) {
                    setConfirming(i);
                  } else {
                    item.action();
                    close();
                  }
                }}
              >
                {item.label}
              </button>
            ),
          )}
        </div>
      )}
    </div>
  );
}
