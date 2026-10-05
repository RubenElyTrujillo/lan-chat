// Ephemeral client state for the LAN-Chat web client.
// No persistence: messages, files, drafts and unread counts live in memory
// for the current page lifetime only. Conversations are keyed by hub peer
// ID — never by display name.

export function normalizeIncoming(payload) {
  if (!payload || typeof payload !== "object") return null;
  if (payload.type === "chat") {
    if (typeof payload.text !== "string" || payload.text.length === 0) return null;
    return { type: "chat", text: payload.text };
  }
  if (payload.type === "file") {
    if (typeof payload.name !== "string" || typeof payload.data !== "string") return null;
    return { type: "file", name: payload.name, data: payload.data };
  }
  return null;
}

const defaultUrls = {
  create(bytes) {
    return URL.createObjectURL(new Blob([bytes]));
  },
  revoke(url) {
    URL.revokeObjectURL(url);
  },
};

/**
 * Inbound gate for relayed chat/file content. The verdict is sourced ONLY
 * from current hub presence for the sender's connection ID — never from
 * payload fields (kind, sid and name are claims any sender can forge):
 * unknown or departed senders fail closed; a known "app" sender must be
 * paired; a known web sender is accepted. `isPaired(senderId)` reports the
 * caller's current pairing state.
 */
export function canAcceptIncomingContent(session, senderId, isPaired) {
  if (!session?.isPresent(senderId)) return false;
  return session.presenceKind(senderId) === "app" ? isPaired?.(senderId) === true : true;
}

/**
 * Outgoing TEXT gate for relayed chat. A web peer is sendable while present;
 * an app peer needs its CURRENT pairing (`isPaired` reports it). Presence and
 * socket state come from the session, never from payload claims.
 */
export function canSendTextTo(session, peerId, isPaired) {
  if (!session?.canSend(peerId)) return false;
  const conv = session.conversation(peerId);
  if (!conv) return false;
  return conv.kind !== "app" || isPaired?.(peerId) === true;
}

/**
 * Outgoing FILE gate: files travel to web peers and to CURRENTLY PAIRED app
 * peers (same hub relay as text, capped at 25 MiB client-side). Verdict is
 * the text gate: presence + socket from the session, pairing from `isPaired`.
 */
export function canSendFileTo(session, peerId, isPaired) {
  return canSendTextTo(session, peerId, isPaired);
}

export function createSession({ now = Date.now, uid = defaultUid, urls = defaultUrls } = {}) {
  const conversations = new Map(); // peerId -> {id, name, kind, online, messages, unread, draft}
  const present = new Map(); // peerId -> {id, name, kind}
  const trackedUrls = new Set();
  let socketOpen = false;
  let selectedId = null;

  function conversation(id) {
    let conv = conversations.get(id);
    if (!conv) {
      conv = { id, name: "", kind: "web", online: false, messages: [], unread: 0, draft: "" };
      conversations.set(id, conv);
    }
    return conv;
  }

  function append(conv, entry) {
    conv.messages.push(entry);
    return entry;
  }

  function makeEntry(kind, mine, extra) {
    return { id: uid(), kind, mine, at: now(), ...extra };
  }

  return {
    myId: null,
    myName: null,
    urls,

    setIdentity(id, name) {
      this.myId = id;
      this.myName = name;
    },

    setSocketOpen(open) {
      socketOpen = open;
      if (!open) {
        for (const conv of conversations.values()) conv.online = false;
      }
    },

    /** Reconcile presence. `list` excludes self. Returns {joined, left}. */
    applyPeers(list) {
      const next = new Map();
      for (const p of list) next.set(p.id, p);
      const joined = [];
      const left = [];
      for (const p of list) {
        if (!present.has(p.id)) joined.push(p);
        const conv = conversation(p.id);
        conv.name = p.name;
        conv.kind = p.kind;
        conv.online = true;
      }
      for (const id of present.keys()) {
        if (!next.has(id)) {
          left.push(present.get(id));
          const conv = conversations.get(id);
          if (conv) conv.online = false;
        }
      }
      present.clear();
      for (const [id, p] of next) present.set(id, p);
      return { joined, left };
    },

    isPresent(id) {
      return present.has(id);
    },

    /** Current presence kind for a peer id, or null when unknown/departed. */
    presenceKind(id) {
      return present.get(id)?.kind ?? null;
    },

    presentPeers() {
      return [...present.values()];
    },

    canSend(id) {
      return socketOpen && present.has(id);
    },

    select(id) {
      selectedId = id;
      const conv = conversations.get(id);
      if (conv) conv.unread = 0;
      return conv;
    },

    selected() {
      return selectedId;
    },

    conversation,

    conversations() {
      return [...conversations.values()];
    },

    addOutgoing(peerId, { type, text, name, size, url, id }) {
      if (!this.canSend(peerId)) throw new Error("peer-unavailable");
      const conv = conversation(peerId);
      if (type === "text") {
        if (typeof text !== "string" || text.trim().length === 0) throw new Error("empty-message");
        return append(conv, makeEntry("text", true, id !== undefined ? { text, id } : { text }));
      }
      if (type === "file") {
        return append(conv, makeEntry("file", true, { text: `📎 ${name}`, name, size, url, id }));
      }
      throw new Error("unknown-entry-type");
    },

    /** Next entry id, so callers can reference an entry before it exists. */
    newId() {
      return uid();
    },

    /** Routes an incoming relay payload into the sender's conversation by ID. */
    addIncoming(peerId, fromName, payload) {
      const p = normalizeIncoming(payload);
      if (!p) return null;
      const conv = conversation(peerId);
      conv.online = true;
      if (fromName) conv.name = fromName;
      let entry;
      if (p.type === "chat") {
        entry = makeEntry("text", false, { text: p.text });
      } else {
        entry = makeEntry("file", false, {
          text: `📎 ${p.name}`,
          name: p.name,
          size: null,
          url: this.urls.create(base64ToBytes(p.data)),
        });
        trackedUrls.add(entry.url);
      }
      append(conv, entry);
      if (selectedId !== peerId) conv.unread += 1;
      return entry;
    },

    system(peerId, text) {
      const conv = conversation(peerId);
      return append(conv, makeEntry("system", false, { text }));
    },

    setDraft(peerId, text) {
      conversation(peerId).draft = text;
    },

    getDraft(peerId) {
      return conversations.get(peerId)?.draft ?? "";
    },

    /** Revoke every tracked object URL. Call only when links are no longer needed. */
    revokeUrls() {
      for (const url of trackedUrls) this.urls.revoke(url);
      trackedUrls.clear();
    },

    trackUrl(url) {
      trackedUrls.add(url);
    },
  };
}

// ── Ciclo de vida de la página ────────────────────────────

/**
 * Final page dismissal cleanup. With BFCache (event.persisted) the page can
 * be restored, so download links must stay usable: nothing is revoked.
 * Returns true only when URLs were actually revoked.
 */
export function handlePageHide(event, session) {
  if (event?.persisted) return false;
  session.revokeUrls();
  return true;
}

/**
 * BFCache restore. The socket never survives the freeze: if the connection is
 * gone, surface the recovery state instead of a silently dead UI. No message
 * is replayed automatically. Returns true when the page was marked
 * disconnected.
 */
export function handlePageShow(event, { session, connection, onDisconnected } = {}) {
  if (!event?.persisted) return false;
  if (connection?.isOpen?.()) return false;
  session.setSocketOpen(false);
  onDisconnected?.();
  return true;
}

function defaultUid() {
  return crypto.randomUUID
    ? crypto.randomUUID()
    : Array.from(crypto.getRandomValues(new Uint32Array(4))).join("-");
}

function base64ToBytes(b64) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

/**
 * Send a file to an explicit recipient. `peerId` is captured before any await:
 * changing the UI selection during preparation cannot redirect the file.
 * Rejects with "peer-unavailable" if the recipient is gone before or after
 * preparation, and with any relay error otherwise. Nothing is created for a
 * failed send: the relay runs before the entry, and the object URL is only
 * minted once the entry exists, so a failure never leaves misleading
 * state or a leaked URL behind.
 */
export async function sendFile(session, peerId, file, { relay = () => {} } = {}) {
  if (!session.canSend(peerId)) throw new Error("peer-unavailable");
  const bytes = await file.prepare();
  if (!session.canSend(peerId)) throw new Error("peer-unavailable");
  const id = session.newId();
  relay(peerId, { type: "file", name: file.name, bytes, kind: "web", id });
  const entry = session.addOutgoing(peerId, {
    type: "file",
    name: file.name,
    size: file.size,
    url: null,
    id,
  });
  entry.url = session.urls.create(bytes);
  session.trackUrl(entry.url);
  return entry;
}

/**
 * Send a text message to an explicit recipient captured by the caller.
 * The relay runs BEFORE the outgoing entry: a failed enqueue must not leave
 * a message behind. Rejects with "peer-unavailable" when the recipient is
 * gone, reports a `false` relay return as "no-connection", and rethrows any
 * relay error. None of these outcomes is a delivery acknowledgement; the
 * caller owns draft preservation and user-facing failure copy.
 */
export function sendTextMessage(session, peerId, text, { relay = () => {} } = {}) {
  if (!session.canSend(peerId)) throw new Error("peer-unavailable");
  const id = session.newId();
  const ok = relay(peerId, { type: "chat", text, kind: "web", id });
  if (ok === false) throw new Error("no-connection");
  return session.addOutgoing(peerId, { type: "text", text, id });
}
