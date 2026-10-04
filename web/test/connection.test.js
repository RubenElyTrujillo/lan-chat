// Wiring coverage for the socket connection manager: duplicate submissions,
// stale socket events, and callbacks bound to their own socket.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createConnection } from "../public/connection.js";

class FakeWebSocket {
  static created = [];
  constructor(url) {
    this.url = url;
    this.readyState = 0; // CONNECTING
    this.sent = [];
    this.closedByClient = false;
    FakeWebSocket.created.push(this);
  }
  send(data) {
    this.sent.push(data);
  }
  close() {
    this.closedByClient = true;
    this.readyState = 3; // CLOSED
    this.onclose?.({ type: "close" });
  }
  // test helpers
  open() {
    this.readyState = 1;
    this.onopen?.({ type: "open" });
  }
  message(data) {
    this.onmessage?.({ data, type: "message" });
  }
  drop() {
    this.readyState = 3;
    this.onclose?.({ type: "close" });
  }
}

function fresh() {
  FakeWebSocket.created = [];
  const calls = { open: [], message: [], close: [] };
  const conn = createConnection({
    url: "ws://hub/test",
    WebSocketImpl: FakeWebSocket,
    onOpen: (ws) => calls.open.push(ws),
    onMessage: (ws, ev) => calls.message.push([ws, ev.data]),
    onClose: (ws) => calls.close.push(ws),
  });
  return { conn, calls };
}

test("repeated connect while connecting or open reuses the same socket", () => {
  const { conn } = fresh();
  const a = conn.connect();
  assert.equal(conn.connect(), a, "second submit during CONNECTING must not open a new socket");
  a.open();
  assert.equal(conn.connect(), a, "third submit while OPEN must not open a new socket");
  assert.equal(FakeWebSocket.created.length, 1);
});

test("callbacks fire only for the active socket, with that socket as argument", () => {
  const { conn, calls } = fresh();
  const a = conn.connect();
  a.open();
  a.message(JSON.stringify({ type: "welcome", id: "a" }));
  assert.deepEqual(calls.open, [a]);
  assert.deepEqual(calls.message, [[a, JSON.stringify({ type: "welcome", id: "a" })]]);
});

test("an old socket's close and messages are ignored once a newer socket is active", () => {
  const { conn, calls } = fresh();
  const a = conn.connect();
  a.open();
  conn.close(); // manager detaches a
  const b = conn.connect();
  b.open();

  // Late close from the detached socket must not disable the new connection
  const closesBefore = calls.close.length;
  a.onclose({ type: "close" });
  assert.equal(calls.close.length, closesBefore, "stale close must be ignored");
  assert.equal(conn.isOpen(), true, "newer connection must stay open");

  // Late message from the detached socket must not reach the app
  a.message(JSON.stringify({ type: "welcome", id: "stale" }));
  assert.deepEqual(
    calls.message.filter(([, d]) => d.includes("stale")),
    [],
    "stale message must be ignored",
  );
});

test("connection loss on the active socket is reported exactly once", () => {
  const { conn, calls } = fresh();
  const a = conn.connect();
  a.open();
  a.drop();
  assert.deepEqual(calls.close, [a]);
  assert.equal(conn.isOpen(), false);
  assert.equal(conn.socket, null);
});

test("connect after a loss creates a fresh socket and the old close stays ignored", () => {
  const { conn, calls } = fresh();
  const a = conn.connect();
  a.open();
  a.drop();
  const b = conn.connect();
  b.open();
  assert.equal(FakeWebSocket.created.length, 2);
  a.onclose({ type: "close" }); // duplicate/late close of the old socket
  assert.equal(conn.isOpen(), true);
  assert.deepEqual(calls.close, [a], "no extra close was reported for the new socket");
});

test("send only dispatches on an open active socket", () => {
  const { conn } = fresh();
  assert.equal(conn.send({ type: "hello" }), false);
  const a = conn.connect();
  assert.equal(conn.send({ type: "hello" }), false, "CONNECTING socket must not send");
  a.open();
  assert.equal(conn.send({ hello: true }), true);
  assert.deepEqual(a.sent, [JSON.stringify({ hello: true })]);
  a.drop();
  assert.equal(conn.send("x"), false);
});

test("close() detaches the socket before its close event fires", () => {
  const { conn, calls } = fresh();
  const a = conn.connect();
  a.open();
  conn.close();
  assert.equal(a.closedByClient, true);
  assert.deepEqual(calls.close, [], "client-initiated close is not 'connection lost'");
  assert.equal(conn.socket, null);
});
