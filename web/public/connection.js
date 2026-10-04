// Socket ownership for the LAN-Chat web client.
// One active connection; callbacks are bound to the socket they belong to;
// stale events from a superseded socket are ignored.

export function createConnection({ url, WebSocketImpl = WebSocket, onOpen, onMessage, onClose } = {}) {
  let active = null;

  return {
    get socket() {
      return active;
    },

    isOpen() {
      return !!active && active.readyState === 1;
    },

    /** No-op while a connection attempt is in flight or open: no duplicates. */
    connect() {
      if (active && (active.readyState === 0 || active.readyState === 1)) return active;
      const ws = new WebSocketImpl(url);
      active = ws;
      ws.onopen = (ev) => {
        if (active !== ws) return;
        onOpen?.(ws, ev);
      };
      ws.onmessage = (ev) => {
        if (active !== ws) return;
        onMessage?.(ws, ev);
      };
      ws.onclose = (ev) => {
        // Only the active socket's close is "connection lost"; closes from a
        // detached socket (client close, stale duplicate) are swallowed.
        if (active !== ws) return;
        active = null;
        onClose?.(ws, ev);
      };
      return ws;
    },

    /** JSON-encodes and sends on the active socket; false if not open. */
    send(payload) {
      if (!this.isOpen()) return false;
      active.send(typeof payload === "string" ? payload : JSON.stringify(payload));
      return true;
    },

    /** Client-initiated close: detaches first so onclose is not "loss". */
    close() {
      const s = active;
      active = null;
      if (s && (s.readyState === 0 || s.readyState === 1)) s.close();
    },
  };
}
