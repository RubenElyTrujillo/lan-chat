// Session id efímero del navegador: uno por carga de página, viaja en el
// hello y nunca se persiste (recargar o duplicar la pestaña acuña uno nuevo).
// Mismo patrón seguro que client-state.js: randomUUID cuando existe,
// getRandomValues como resguardo para contextos inseguros.
export function createSessionId({ cryptoObj = globalThis.crypto } = {}) {
  if (cryptoObj && typeof cryptoObj.randomUUID === "function") {
    return cryptoObj.randomUUID();
  }
  const bytes = new Uint32Array(4);
  cryptoObj.getRandomValues(bytes);
  return Array.from(bytes).join("-");
}
