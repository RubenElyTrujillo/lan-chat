// Toast: una línea efímera abajo al centro. Reemplaza al toast anterior
// (cola de uno) y se apaga sola a los 2.5s; aria-live para lectores.
export interface ToastState {
  id: number;
  text: string;
}

export function Toast({ toast }: { toast: ToastState | null }) {
  if (!toast) return null;
  return (
    <div className="toast" role="status" aria-live="polite" key={toast.id}>
      {toast.text}
    </div>
  );
}
