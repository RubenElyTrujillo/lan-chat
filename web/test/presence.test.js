// Integration coverage for hub presence metadata: valid/invalid sid and caps
// on hello, peer broadcasts carrying them, and relay attribution from the
// registered source (never from a payload claim). Runs against a real
// ephemeral server on 127.0.0.1 — nothing external, nothing left running.
import { test, beforeEach, after } from "node:test";
import assert from "node:assert/strict";
import WebSocket from "ws";

process.env.PORT = "0"; // ephemeral port; exported server exposes the address
const { server } = await import("../server.js");

await new Promise((resolve) => {
  if (server.listening) resolve();
  else server.once("listening", resolve);
});

const PORT = server.address().port;
const URL = `ws://127.0.0.1:${PORT}`;

const sockets = [];

const BOUND = 2000; // every await is bounded: a clear failure beats a hang

async function connect(name) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(URL);
    sockets.push(ws);
    const inbox = [];
    const waiters = [];
    ws.on("message", (raw) => {
      const msg = JSON.parse(raw.toString());
      const w = waiters.shift();
      if (w) w(msg);
      else inbox.push(msg);
    });
    ws.on("open", () => {
      const next = async () => {
        if (inbox.length) return inbox.shift();
        const timer = new Promise((_, rej) =>
          setTimeout(() => rej(new Error(`${name}: no frame within ${BOUND}ms`)), BOUND),
        );
        const got = new Promise((r) => waiters.push(r));
        return Promise.race([got, timer]);
      };
      resolve({
        ws,
        name,
        // Sends hello (with this client's name) and consumes its own welcome.
        hello: async (body) => {
          ws.send(JSON.stringify({ type: "hello", name, ...body }));
          await next();
        },
        relay: (to, payload) => ws.send(JSON.stringify({ type: "relay", to, payload })),
        next,
      });
    });
    ws.on("error", reject);
  });
}

beforeEach(() => {
  for (const ws of sockets.splice(0)) ws.terminate();
});

after(() => {
  for (const ws of sockets.splice(0)) ws.terminate();
  server.close();
});

test("valid sid and caps on hello are stored and broadcast to the group", async () => {
  const a = await connect("Ana");
  const b = await connect("Beto");
  a.hello({ kind: "web", sid: "0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f", caps: ["chat-v1"] });
  b.hello({ kind: "web" });

  const seenByB = await b.next();
  assert.equal(seenByB.type, "peers");
  const ana = seenByB.list.find((p) => p.name === "Ana");
  assert.ok(ana, "Ana must be in the peers list");
  assert.equal(ana.sid, "0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f", "UUID sid with hyphens must survive");
  assert.deepEqual(ana.caps, ["chat-v1"]);
  const beto = seenByB.list.find((p) => p.name === "Beto");
  assert.ok(beto, "Beto must be in the peers list");
  assert.ok(!("sid" in beto), "hello without sid must omit sid, not send null");
  assert.ok(!("caps" in beto), "hello without caps must omit caps, not send []");
  assert.ok(beto.id, "every peer keeps its conn id");
  assert.equal(beto.kind, "web");
});

test("malformed sid and caps are omitted without crashing the hub", async () => {
  const a = await connect("Malo");
  const b = await connect("Beto");
  a.hello({ kind: "web", sid: 123, caps: "not-an-array" });
  b.hello({ kind: "web" });

  const seenByB = await b.next();
  const malo = seenByB.list.find((p) => p.name === "Malo");
  assert.ok(malo, "the peer itself is still listed");
  assert.ok(!("sid" in malo), "non-string sid must be omitted");
  assert.ok(!("caps" in malo), "non-array caps must be omitted");

  // The hub is alive: Ceci's hello still triggers a group broadcast.
  const c = await connect("Ceci");
  c.hello({ kind: "web" });
  const seen = await c.next();
  assert.equal(seen.type, "peers");
  assert.ok(seen.list.find((p) => p.name === "Malo"), "hub still lists peers after malformed metadata");
});

test("sid bounds: over 64 chars or invalid characters are omitted", async () => {
  const a = await connect("Largo");
  const b = await connect("Raro");
  a.hello({ kind: "web", sid: "a".repeat(65) });
  b.hello({ kind: "web", sid: "bad sid!" });

  const seen = await b.next();
  assert.ok(!("sid" in seen.list.find((p) => p.name === "Largo")), "65-char sid is too long");
  assert.ok(!("sid" in seen.list.find((p) => p.name === "Raro")), "chars outside [A-Za-z0-9_-] are rejected");

  const ok = await connect("Justo");
  ok.hello({ kind: "web", sid: "a".repeat(64) });
  const seen2 = await b.next();
  assert.equal(seen2.list.find((p) => p.name === "Justo").sid, "a".repeat(64), "exactly 64 chars is valid");
});

test("caps bounds: at most 8 items, each <= 32 chars, bad items dropped", async () => {
  const a = await connect("ConCaps");
  const b = await connect("Nueve");
  a.hello({ kind: "web", caps: ["c1", 42, "", "x".repeat(33), "c2"] });
  b.hello({ kind: "web", caps: ["1", "2", "3", "4", "5", "6", "7", "8", "9"] });

  const seen = await b.next();
  assert.deepEqual(
    seen.list.find((p) => p.name === "ConCaps").caps,
    ["c1", "c2"],
    "non-strings, empties and over-long items are dropped",
  );
  const nueve = seen.list.find((p) => p.name === "Nueve").caps;
  assert.equal(nueve.length, 8, "caps are capped at 8");
  assert.deepEqual(nueve, ["1", "2", "3", "4", "5", "6", "7", "8"]);
});

test("relay stamps from_sid from the registered source, never a payload claim", async () => {
  const a = await connect("Ana");
  const b = await connect("Beto");
  const fake = await connect("Falso");
  a.hello({ kind: "web", sid: "source-sid-1", caps: ["chat-v1"] });
  b.hello({ kind: "web" });
  fake.hello({ kind: "web" });

  const seen = await b.next(); // broadcast [Ana, Beto]
  const bId = seen.list.find((p) => p.name === "Beto").id;
  await b.next(); // drain broadcast [Ana, Beto, Falso]

  a.relay(bId, { from_sid: "EVIL", body: "hola" });
  const relayed = await b.next();
  assert.equal(relayed.type, "relay");
  assert.equal(relayed.from_sid, "source-sid-1", "from_sid comes from the registered connection");
  assert.deepEqual(relayed.payload, { from_sid: "EVIL", body: "hola" }, "payload is forwarded untouched");
});

test("relay from a source without sid omits from_sid", async () => {
  const a = await connect("Anon");
  const b = await connect("Beto");
  a.hello({ kind: "web" });
  b.hello({ kind: "web" });

  const seen = await b.next();
  const bId = seen.list.find((p) => p.name === "Beto").id;

  a.relay(bId, { body: "hola" });
  const relayed = await b.next();
  assert.equal(relayed.type, "relay");
  assert.ok(!("from_sid" in relayed), "no registered sid means no from_sid key");
});

test("duplicate active sids are listed as separate peers, never merged", async () => {
  const a = await connect("Uno");
  const b = await connect("Dos");
  const c = await connect("Ojo");
  a.hello({ kind: "web", sid: "same-sid" });
  b.hello({ kind: "web", sid: "same-sid" });
  c.hello({ kind: "web" });

  const seen = await c.next();
  const withSame = seen.list.filter((p) => p.sid === "same-sid");
  assert.equal(withSame.length, 2, "both connections stay listed under their own conn id");
  assert.notEqual(withSame[0].id, withSame[1].id, "conn ids stay unique");
});
