// Regression coverage for the ephemeral client state module.
// Run: npm test (from web/)
import { test } from "node:test";
import assert from "node:assert/strict";
import { createSession, normalizeIncoming, sendFile, sendTextMessage, handlePageHide, handlePageShow } from "../public/client-state.js";

const T0 = 1_700_000_000_000;

function makeSession() {
  const revocations = [];
  const urls = {
    list: [],
    create() {
      const u = `blob:u${this.list.length + 1}`;
      this.list.push(u);
      return u;
    },
    revoke(u) {
      revocations.push(u);
    },
  };
  let i = 0;
  const session = createSession({
    now: () => T0,
    uid: () => `m${++i}`,
    urls,
  });
  session.setIdentity("me", "Yo");
  session.setSocketOpen(true);
  return { session, urls, revocations };
}

function peersOf(...list) {
  return list.map(([id, name, kind = "web"]) => ({ id, name, kind }));
}

test("conversations stay isolated across peer switches", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"], ["b1", "Beto"]));
  session.select("a1");

  session.addOutgoing("a1", { type: "text", text: "para Ana" });
  const inc = session.addIncoming("b1", "Beto", { type: "chat", text: "para Beto" });
  assert.ok(inc, "incoming should be accepted");

  // While chatting with Ana, Beto's message must not leak into Ana's thread
  assert.equal(session.conversation("a1").messages.length, 1);
  assert.equal(session.conversation("b1").messages.length, 1);
  assert.equal(session.conversation("b1").unread, 1);
  assert.equal(session.conversation("a1").unread, 0);

  // Selecting Beto clears his unread and shows only his conversation
  session.select("b1");
  assert.equal(session.conversation("b1").unread, 0);
  const b = session.conversation("b1").messages;
  assert.equal(b.length, 1);
  assert.equal(b[0].text, "para Beto");
  assert.equal(b[0].mine, false);

  // Switching back preserves both threads untouched
  session.select("a1");
  assert.equal(session.conversation("a1").messages[0].text, "para Ana");
  assert.equal(session.conversation("a1").messages[0].mine, true);
  assert.equal(session.conversation("b1").messages.length, 1);
});

test("incoming messages route by sender ID, not name", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["x1", "Ana"]));
  session.addIncoming("x1", "Ana", { type: "chat", text: "hola" });

  // Same ID, renamed between presence updates: same conversation, new name
  session.applyPeers(peersOf(["x1", "Ana dos"]));
  session.addIncoming("x1", "Ana dos", { type: "chat", text: "sigo siendo yo" });

  const conv = session.conversation("x1");
  assert.equal(conv.messages.length, 2);
  assert.equal(conv.name, "Ana dos");

  // A different ID never receives another peer's messages
  session.applyPeers(peersOf(["x1", "Ana dos"], ["y1", "Yago"]));
  assert.equal(session.conversation("y1").messages.length, 0);
});

test("outgoing is mine and duplicate display names don't merge", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"], ["a2", "Ana"]));
  session.select("a1");

  const out = session.addOutgoing("a1", { type: "text", text: "hola Ana" });
  assert.equal(out.mine, true);

  const inc = session.addIncoming("a2", "Ana", { type: "chat", text: "hola" });
  assert.equal(inc.mine, false);

  assert.equal(session.conversation("a1").messages.length, 1);
  assert.equal(session.conversation("a1").messages[0].mine, true);
  assert.equal(session.conversation("a2").messages.length, 1);
  assert.equal(session.conversation("a2").messages[0].mine, false);
});

test("disconnected peers cannot receive sends; same-name newcomer starts empty", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.select("a1");
  assert.equal(session.canSend("a1"), true);
  session.addOutgoing("a1", { type: "text", text: "antes de irte" });

  session.applyPeers([]);
  assert.equal(session.canSend("a1"), false);
  assert.equal(session.conversation("a1").online, false);
  // History is retained in memory for the session
  assert.equal(session.conversation("a1").messages.length, 1);
  assert.throws(() => session.addOutgoing("a1", { type: "text", text: "x" }), /peer-unavailable/);

  // A new peer with the same name must not inherit the old ID's chat
  session.applyPeers(peersOf(["a9", "Ana"]));
  assert.equal(session.conversation("a9").messages.length, 0);
  assert.equal(session.canSend("a9"), true);
  assert.equal(session.canSend("a1"), false);
});

test("socket closed gates every send", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.setSocketOpen(false);
  assert.equal(session.canSend("a1"), false);
  assert.throws(() => session.addOutgoing("a1", { type: "text", text: "x" }), /peer-unavailable/);
  assert.equal(session.conversation("a1").messages.length, 0);
  assert.equal(session.conversation("a1").online, false);
});

test("empty text is refused", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  assert.throws(() => session.addOutgoing("a1", { type: "text", text: "   " }), /empty/);
});

test("file recipient is captured before async preparation", async () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"], ["b1", "Beto"]));
  session.select("a1");

  let release;
  const gate = new Promise((r) => (release = r));
  const relayed = [];
  const pending = sendFile(
    session,
    "a1",
    { name: "f.txt", size: 3, prepare: () => gate.then(() => new Uint8Array([1, 2, 3])) },
    { relay: (to, payload) => relayed.push([to, payload]) },
  );

  // Selection changes to another peer while the file is being prepared
  session.select("b1");
  session.addOutgoing("b1", { type: "text", text: "otro chat" });
  release();

  const entry = await pending;
  assert.equal(entry.kind, "file");
  assert.equal(entry.mine, true);
  assert.equal(entry.name, "f.txt");

  const a = session.conversation("a1").messages;
  assert.equal(a.length, 1, "file must land in the captured recipient's thread");
  assert.equal(a[0].kind, "file");
  assert.equal(relayed.length, 1);
  assert.equal(relayed[0][0], "a1", "relay must target the captured recipient ID");
  assert.equal(relayed[0][1].name, "f.txt");
  assert.equal(session.conversation("b1").messages.length, 1, "only the text goes to Beto");
});

test("file to a peer who leaves during preparation is not sent or redirected", async () => {
  const { session, urls } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.select("a1");

  let release;
  const gate = new Promise((r) => (release = r));
  const relayed = [];
  const pending = sendFile(
    session,
    "a1",
    { name: "f.txt", size: 3, prepare: () => gate.then(() => new Uint8Array([1, 2, 3])) },
    { relay: (to) => relayed.push(to) },
  );

  session.applyPeers([]); // recipient leaves while preparing
  release();

  await assert.rejects(pending, /peer-unavailable/);
  assert.equal(relayed.length, 0, "must not relay to a departed peer");
  assert.equal(session.conversation("a1").messages.length, 0, "must not append a message");
  assert.equal(urls.list.length, 0, "must not create an object URL for a failed send");
});

test("sendFile refuses a disconnected recipient before preparing", async () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["b1", "Beto"]));
  let prepared = false;
  await assert.rejects(
    sendFile(
      session,
      "ghost",
      { name: "f", size: 1, prepare: () => ((prepared = true), Promise.resolve(new Uint8Array())) },
      { relay: () => {} },
    ),
    /peer-unavailable/,
  );
  assert.equal(prepared, false, "prepare must not run for an unavailable recipient");
});

test("object URLs are tracked and revoked only on explicit cleanup", async () => {
  const { session, urls, revocations } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"], ["b1", "Beto"]));
  session.select("a1");

  await sendFile(session, "a1", { name: "uno.txt", size: 1, prepare: () => new Uint8Array([1]) }, { relay: () => {} });
  await sendFile(session, "a1", { name: "dos.txt", size: 1, prepare: () => new Uint8Array([2]) }, { relay: () => {} });
  assert.equal(urls.list.length, 2);
  assert.equal(session.conversation("a1").messages[0].url, "blob:u1");
  assert.equal(session.conversation("a1").messages[1].url, "blob:u2");

  // Switching peers and rendering never revokes: download links stay usable
  session.select("b1");
  assert.equal(revocations.length, 0);

  session.revokeUrls();
  assert.deepEqual([...revocations].sort(), ["blob:u1", "blob:u2"]);

  // Idempotent: no double revoke
  session.revokeUrls();
  assert.equal(revocations.length, 2);
});

test("malformed incoming payloads are rejected without mutating state", () => {
  assert.equal(normalizeIncoming(null), null);
  assert.equal(normalizeIncoming(undefined), null);
  assert.equal(normalizeIncoming("chat"), null);
  assert.equal(normalizeIncoming({ type: "chat" }), null);
  assert.equal(normalizeIncoming({ type: "chat", text: 42 }), null);
  assert.equal(normalizeIncoming({ type: "file", name: "x" }), null);
  assert.equal(normalizeIncoming({ type: "file", data: "aaa" }), null);
  assert.equal(normalizeIncoming({ type: "nope" }), null);
  assert.deepEqual(normalizeIncoming({ type: "chat", text: "hola" }), { type: "chat", text: "hola" });
  const f = normalizeIncoming({ type: "file", name: "a.png", data: "aaa" });
  assert.deepEqual(f, { type: "file", name: "a.png", data: "aaa" });

  const { session } = makeSession();
  session.applyPeers(peersOf(["x1", "Xime"]));
  assert.equal(session.addIncoming("x1", "Xime", { type: "chat" }), null);
  assert.equal(session.conversation("x1").messages.length, 0);
});

test("drafts persist per peer across switches", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"], ["b1", "Beto"]));
  session.setDraft("a1", "hola");
  session.select("b1");
  session.setDraft("b1", "chau");
  assert.equal(session.getDraft("a1"), "hola");
  assert.equal(session.getDraft("b1"), "chau");
  assert.equal(session.getDraft("ghost"), "");
});

// ── Envío de archivos: fallas locales ─────────────────────

test("relay failure leaves no outgoing entry and no object URL", async () => {
  const { session, urls } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.select("a1");

  await assert.rejects(
    sendFile(
      session,
      "a1",
      { name: "f.txt", size: 3, prepare: () => new Uint8Array([1, 2, 3]) },
      {
        relay: () => {
          throw new Error("boom");
        },
      },
    ),
    /boom/,
  );
  assert.equal(session.conversation("a1").messages.length, 0, "no misleading entry");
  assert.equal(urls.list.length, 0, "no URL created for a failed enqueue");
});

test("prepare failure never calls relay and creates nothing", async () => {
  const { session, urls } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  let relayed = 0;
  await assert.rejects(
    sendFile(
      session,
      "a1",
      {
        name: "f.txt",
        size: 3,
        prepare: () => Promise.reject(new Error("disk gone")),
      },
      { relay: () => relayed++ },
    ),
    /disk gone/,
  );
  assert.equal(relayed, 0);
  assert.equal(session.conversation("a1").messages.length, 0);
  assert.equal(urls.list.length, 0);
});

test("successful file send uses one id on the wire and on the entry", async () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.select("a1");
  const relayed = [];
  const entry = await sendFile(
    session,
    "a1",
    { name: "f.txt", size: 3, prepare: () => new Uint8Array([1]) },
    { relay: (to, payload) => relayed.push([to, payload]) },
  );
  assert.equal(entry.url, "blob:u1");
  assert.equal(relayed.length, 1);
  assert.equal(relayed[0][1].id, entry.id, "wire id and entry id must match");
});

// ── Envío de texto: fallas locales ─────────────────────────

test("successful text send relays before appending the outgoing entry", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.select("a1");
  const relayed = [];

  const entry = sendTextMessage(session, "a1", "hola Ana", {
    relay: (to, payload) => relayed.push([to, payload]),
  });

  assert.equal(entry.mine, true);
  assert.equal(entry.text, "hola Ana");
  assert.equal(relayed.length, 1);
  assert.equal(relayed[0][0], "a1");
  assert.equal(relayed[0][1].type, "chat");
  assert.equal(relayed[0][1].text, "hola Ana");
  assert.equal(relayed[0][1].kind, "web");
  assert.equal(relayed[0][1].id, entry.id, "wire id and entry id must match");
  const a = session.conversation("a1").messages;
  assert.equal(a.length, 1);
  assert.equal(a[0].text, "hola Ana");
});

test("text entry without an explicit id keeps a generated nonempty unique id", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.select("a1");

  const first = session.addOutgoing("a1", { type: "text", text: "uno" });
  const second = session.addOutgoing("a1", { type: "text", text: "dos" });

  assert.ok(first.id, "generated id must be nonempty");
  assert.ok(second.id, "generated id must be nonempty");
  assert.notEqual(first.id, second.id, "generated ids must be unique");
});

test("relay returning false is a failed enqueue: no outgoing entry is created", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.select("a1");

  assert.throws(
    () => sendTextMessage(session, "a1", "hola", { relay: () => false }),
    /no-connection/,
  );
  assert.equal(session.conversation("a1").messages.length, 0, "nothing enqueued, nothing appended");
});

test("relay throwing propagates and creates no outgoing entry", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.select("a1");

  assert.throws(
    () =>
      sendTextMessage(session, "a1", "hola", {
        relay: () => {
          throw new Error("InvalidStateError");
        },
      }),
    /InvalidStateError/,
  );
  assert.equal(session.conversation("a1").messages.length, 0, "no misleading entry after a throw");
});

test("unavailable recipient refuses before relaying", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.setSocketOpen(false); // peer left / socket died: canSend false
  let relayed = 0;

  assert.throws(
    () => sendTextMessage(session, "a1", "hola", { relay: () => relayed++ }),
    /peer-unavailable/,
  );
  assert.equal(relayed, 0, "relay must not run for an unavailable recipient");
  assert.equal(session.conversation("a1").messages.length, 0);
});

// ── Ciclo de vida de la página (BFCache) ──────────────────

test("pagehide without persisted revokes URLs; with persisted (BFCache) keeps them", () => {
  const { session, urls, revocations } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  session.select("a1");
  session.addIncoming("a1", "Ana", { type: "file", name: "a.txt", data: "YQ==" });

  assert.equal(handlePageHide({ persisted: false }, session), true);
  assert.deepEqual(revocations, urls.list, "final hide cleans up");

  // Fresh session: a BFCache suspend must keep download links alive
  const again = makeSession();
  again.session.applyPeers(peersOf(["a1", "Ana"]));
  again.session.select("a1");
  again.session.addIncoming("a1", "Ana", { type: "file", name: "a.txt", data: "YQ==" });
  assert.equal(handlePageHide({ persisted: true }, again.session), false);
  assert.equal(again.revocations.length, 0, "BFCache restore must keep files downloadable");
  assert.ok(again.session.conversation("a1").messages[0].url);
});

test("pageshow with persisted and a dead connection marks the session disconnected", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  const conn = { isOpen: () => false };
  const notified = [];
  const handled = handlePageShow({ persisted: true }, {
    session,
    connection: conn,
    onDisconnected: () => notified.push(true),
  });
  assert.equal(handled, true);
  assert.deepEqual(notified, [true], "restored page must surface the recovery state");
  assert.equal(session.canSend("a1"), false);
  assert.equal(session.conversation("a1").online, false);
});

test("pageshow with persisted and a live connection changes nothing", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  const conn = { isOpen: () => true };
  const notified = [];
  const handled = handlePageShow({ persisted: true }, {
    session,
    connection: conn,
    onDisconnected: () => notified.push(true),
  });
  assert.equal(handled, false);
  assert.deepEqual(notified, []);
  assert.equal(session.canSend("a1"), true);
});

test("pageshow without persisted (normal load) does nothing", () => {
  const { session } = makeSession();
  session.applyPeers(peersOf(["a1", "Ana"]));
  const notified = [];
  const conn = { isOpen: () => false };
  assert.equal(handlePageShow({ persisted: false }, {
    session,
    connection: conn,
    onDisconnected: () => notified.push(true),
  }), false);
  assert.deepEqual(notified, []);
  // unchanged: presence was never reconciled by a pageshow of a fresh load
  assert.equal(session.canSend("a1"), true);
});
