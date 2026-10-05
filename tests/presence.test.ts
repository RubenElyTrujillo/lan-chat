// Hub presence tests (pure, DOM-free, run with: node --test tests/presence.test.ts)
// Mirrors hub-status.test.ts: parser bounds, race-safe query/listen lifecycle
// and the honest UI vocabulary for hub browser peers.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  HUB_PEER_PREVIEW,
  hubConnIdFromKey,
  hubDeviceFromPeer,
  hubPeerKey,
  hubPeerName,
  isHubKey,
  isValidSid,
  mergeDevices,
  MAX_CAPS,
  MAX_PEER_NAME_LEN,
  parseHubPeer,
  parseHubPresence,
  trackHubPresence,
  type HubPeer,
} from "../src/lib/hub-presence.ts";
import type { DeviceState } from "../src/types.ts";

const flush = () => new Promise((r) => setTimeout(r, 0));

const PEER = (over: Partial<HubPeer> = {}): HubPeer => ({
  conn_id: "abc123",
  name: "Ana",
  kind: "web",
  sid: "0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f",
  caps: ["chat-v1"],
  ...over,
});

const SNAPSHOT = (peers: HubPeer[]) => ({ peers });

function capture() {
  const seen: HubPeer[][] = [];
  let unavailable = 0;
  return {
    seen,
    unavailable: () => unavailable,
    handlers: {
      onPeers: (peers: HubPeer[]) => seen.push(peers),
      onUnavailable: () => {
        unavailable++;
      },
    },
  };
}

// ── Parser: one peer ────────────────────────────────────────────────────────

test("parseHubPeer accepts a valid entry with optional sid and caps", () => {
  const peer = parseHubPeer({
    conn_id: "abc123def456",
    name: "Ana",
    kind: "web",
    sid: "0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f",
    caps: ["chat-v1"],
  });
  assert.deepEqual(peer, {
    conn_id: "abc123def456",
    name: "Ana",
    kind: "web",
    sid: "0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f",
    caps: ["chat-v1"],
  });
  const minimal = parseHubPeer({ conn_id: "w1", name: "Beto", kind: "web" });
  assert.equal(minimal?.conn_id, "w1");
  assert.equal(minimal && "sid" in minimal, false, "missing sid stays absent, never null");
  assert.deepEqual(minimal?.caps, [], "missing caps degrade to empty list");
});

test("parseHubPeer rejects garbage entries", () => {
  assert.equal(parseHubPeer(null), null);
  assert.equal(parseHubPeer(42), null);
  assert.equal(parseHubPeer("peer"), null);
  assert.equal(parseHubPeer({}), null);
  assert.equal(parseHubPeer({ name: "Ana", kind: "web" }), null, "conn_id is required");
  assert.equal(parseHubPeer({ conn_id: "", kind: "web" }), null);
  assert.equal(parseHubPeer({ conn_id: "x".repeat(65), kind: "web" }), null);
  assert.equal(parseHubPeer({ conn_id: 7, kind: "web" }), null);
  assert.equal(parseHubPeer({ conn_id: "w1", name: "App", kind: "app" }), null, "only web kind is surfaced");
  assert.equal(parseHubPeer({ conn_id: "r1", kind: "relay" }), null);
});

test("parseHubPeer truncates long names and tolerates missing names", () => {
  const long = parseHubPeer({ conn_id: "n1", name: "ñ".repeat(60), kind: "web" });
  assert.equal(long?.name.length, MAX_PEER_NAME_LEN);
  const missing = parseHubPeer({ conn_id: "n2", kind: "web" });
  assert.equal(missing?.name, "");
});

test("parseHubPeer keeps an untrusted sid only when well-formed", () => {
  assert.equal(isValidSid("a".repeat(64)), true, "64 ASCII chars is valid");
  assert.equal(isValidSid("0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f"), true, "UUID hyphens are valid");
  assert.equal(isValidSid(""), false);
  assert.equal(isValidSid("a".repeat(65)), false, "65 chars is too long");
  assert.equal(isValidSid("bad sid!"), false, "chars outside [A-Za-z0-9_-] are rejected");
  assert.equal(isValidSid("áéí"), false, "non-ASCII is rejected");

  const omitted = parseHubPeer({ conn_id: "r", name: "R", kind: "web", sid: "bad sid!" });
  assert.equal(omitted && "sid" in omitted, false, "invalid sid is omitted, entry stays");
  const mistyped = parseHubPeer({ conn_id: "t", name: "T", kind: "web", sid: 123 });
  assert.equal(mistyped && "sid" in mistyped, false);
});

test("parseHubPeer caps are bounded and sanitized", () => {
  const peer = parseHubPeer({
    conn_id: "c",
    name: "C",
    kind: "web",
    caps: ["a", 42, "", "   ", "x".repeat(33), "b", "c", "d", "e", "f", "g", "h", "i"],
  });
  assert.deepEqual(peer?.caps, ["a", "b", "c", "d", "e", "f", "g", "h"]);
  assert.equal(peer!.caps.length <= MAX_CAPS, true);

  const malformed = parseHubPeer({ conn_id: "s", name: "S", kind: "web", caps: "nope" });
  assert.deepEqual(malformed?.caps, []);
});

// ── Parser: full snapshot ───────────────────────────────────────────────────

test("parseHubPresence accepts a snapshot and an empty reset", () => {
  const snap = parseHubPresence({ peers: [PEER(), PEER({ conn_id: "w2", sid: undefined })] });
  assert.equal(snap?.peers.length, 2);
  assert.deepEqual(parseHubPresence({ peers: [] }), { peers: [] }, "disconnect/reset is an empty snapshot");
});

test("parseHubPresence rejects garbage payloads", () => {
  assert.equal(parseHubPresence(null), null);
  assert.equal(parseHubPresence("peers"), null);
  assert.equal(parseHubPresence({}), null);
  assert.equal(parseHubPresence({ peers: 42 }), null);
  assert.equal(parseHubPresence({ peers: "x" }), null);
});

test("parseHubPresence skips invalid entries and caps the list at 32 peers", () => {
  const snap = parseHubPresence({
    peers: [42, {}, { conn_id: "good1", kind: "web" }, { conn_id: "a1", kind: "app" }],
  });
  assert.deepEqual(
    snap?.peers.map((p) => p.conn_id),
    ["good1"],
  );

  const many = Array.from({ length: 40 }, (_, i) => ({ conn_id: `p${i}`, kind: "web" }));
  assert.equal(parseHubPresence({ peers: many })?.peers.length, 32);
});

test("duplicate sids and duplicate names stay separate peers, never merged", () => {
  const snap = parseHubPresence({
    peers: [
      { conn_id: "one", name: "Navegador", kind: "web", sid: "same" },
      { conn_id: "two", name: "Navegador", kind: "web", sid: "same" },
    ],
  });
  assert.equal(snap?.peers.length, 2);
  assert.notEqual(snap?.peers[0].conn_id, snap?.peers[1].conn_id);
});

// ── Key vocabulary ──────────────────────────────────────────────────────────

test("hub keys are per connection, never per persisted sid", () => {
  assert.equal(hubPeerKey("abc123"), "hub-session:abc123");
  assert.equal(isHubKey("hub-session:abc123"), true);
  assert.equal(isHubKey("Ana"), false, "LAN keys are plain names");
  assert.equal(isHubKey("hub:same-sid"), false, "the persisted-sid prefix is not a hub key");
  assert.equal(hubConnIdFromKey("hub-session:abc123"), "abc123");
  assert.equal(hubConnIdFromKey("Ana"), null);
});

// ── Lifecycle (mirrors trackHubStatus races) ───────────────────────────────

test("applies the initial query snapshot when no events arrive", async () => {
  const cap = capture();
  const adapter = {
    on: async () => () => {},
    get: async () => SNAPSHOT([PEER({ name: "Beto" })]),
  };
  const stop = trackHubPresence(adapter, cap.handlers);
  await flush();
  assert.deepEqual(cap.seen.map((peers) => peers.map((p) => p.name)), [["Beto"]]);
  stop();
});

test("ignores a stale snapshot when an event arrives before the query resolves", async () => {
  const cap = capture();
  let fire: ((s: unknown) => void) | null = null;
  const adapter = {
    on: async (h: (s: unknown) => void) => {
      fire = h;
      return () => {
        fire = null;
      };
    },
    get: async () => {
      fire!(SNAPSHOT([PEER({ name: "Nueva" })]));
      return SNAPSHOT([PEER({ name: "Vieja" })]);
    },
  };
  const stop = trackHubPresence(adapter, cap.handlers);
  await flush();
  assert.deepEqual(cap.seen.map((peers) => peers.map((p) => p.name)), [["Nueva"]]);
  stop();
});

test("a newer event wins over a snapshot still in flight; later events keep applying", async () => {
  const cap = capture();
  let fire: ((s: unknown) => void) | null = null;
  let resolveGet: (s: unknown) => void = () => {};
  const adapter = {
    on: async (h: (s: unknown) => void) => {
      fire = h;
      return () => {};
    },
    get: () =>
      new Promise((resolve) => {
        resolveGet = resolve;
      }),
  };
  const stop = trackHubPresence(adapter, cap.handlers);
  await flush();
  assert.equal(cap.seen.length, 0);
  fire!(SNAPSHOT([PEER({ name: "Nueva" })]));
  assert.deepEqual(cap.seen.map((peers) => peers.map((p) => p.name)), [["Nueva"]]);
  resolveGet(SNAPSHOT([PEER({ name: "Vieja" })]));
  await flush();
  assert.equal(cap.seen.length, 1);
  fire!(SNAPSHOT([]));
  assert.deepEqual(cap.seen.at(-1), [], "an empty snapshot (reset) still applies");
  stop();
});

test("reports unavailable and shows nothing stale when the query fails", async () => {
  const cap = capture();
  const adapter = {
    on: async () => () => {},
    get: async () => {
      throw new Error("hub down");
    },
  };
  const stop = trackHubPresence(adapter, cap.handlers);
  await flush();
  assert.equal(cap.unavailable(), 1);
  assert.equal(cap.seen.length, 0);
  stop();
});

test("reports unavailable when the subscription itself fails", async () => {
  const cap = capture();
  let gets = 0;
  const adapter = {
    on: async () => {
      throw new Error("listen failed");
    },
    get: async () => {
      gets++;
      return SNAPSHOT([]);
    },
  };
  const stop = trackHubPresence(adapter, cap.handlers);
  await flush();
  assert.equal(cap.unavailable(), 1);
  assert.equal(gets, 0);
  stop();
});

test("dispose before the subscription resolves unlistens and never queries", async () => {
  let offCalls = 0;
  let gets = 0;
  let resolveOn: (off: () => void) => void = () => {};
  const adapter = {
    on: () =>
      new Promise((resolve) => {
        resolveOn = resolve;
      }),
    get: async () => {
      gets++;
      return SNAPSHOT([]);
    },
  };
  const stop = trackHubPresence(adapter, {
    onPeers: () => assert.fail("no peers after dispose"),
  });
  stop();
  resolveOn(() => {
    offCalls++;
  });
  await flush();
  assert.equal(offCalls, 1);
  assert.equal(gets, 0);
});

test("double mount (StrictMode): the first mount cleans up, the second stays live", async () => {
  let firstOff = 0;
  let secondFire: ((s: unknown) => void) | null = null;
  let n = 0;
  const cap = capture();
  const adapter = {
    on: async (h: (s: unknown) => void) => {
      n++;
      if (n === 1) {
        return () => {
          firstOff++;
        };
      }
      secondFire = h;
      return () => {};
    },
    get: async () => SNAPSHOT([PEER()]),
  };
  const stopFirst = trackHubPresence(adapter, cap.handlers);
  stopFirst();
  const stopSecond = trackHubPresence(adapter, cap.handlers);
  await flush();
  assert.equal(firstOff, 1);
  assert.equal(cap.seen.at(-1)?.length, 1);
  secondFire!(SNAPSHOT([PEER({ conn_id: "w2", name: "Beto" })]));
  assert.deepEqual(cap.seen.at(-1)?.map((p) => p.name), ["Beto"]);
  stopSecond();
});

test("events arriving after dispose are ignored", async () => {
  const cap = capture();
  let fire: ((s: unknown) => void) | null = null;
  const adapter = {
    on: async (h: (s: unknown) => void) => {
      fire = h;
      return () => {};
    },
    get: async () => SNAPSHOT([PEER()]),
  };
  const stop = trackHubPresence(adapter, cap.handlers);
  await flush();
  const before = cap.seen.length;
  stop();
  fire!(SNAPSHOT([PEER({ conn_id: "late" })]));
  assert.equal(cap.seen.length, before);
});

test("unparsable events are ignored and do not invalidate the in-flight snapshot", async () => {
  const cap = capture();
  let fire: ((raw: unknown) => void) | null = null;
  const adapter = {
    on: async (h: (raw: unknown) => void) => {
      fire = h;
      return () => {};
    },
    get: async () => {
      fire!({ nonsense: true });
      return SNAPSHOT([PEER()]);
    },
  };
  const stop = trackHubPresence(adapter, cap.handlers);
  await flush();
  assert.equal(cap.seen.length, 1);
  stop();
});

// ── UI helpers ──────────────────────────────────────────────────────────────

test("hubPeerName falls back to an honest browser label when the name is empty", () => {
  assert.equal(hubPeerName(PEER({ name: "Ana" })), "Ana");
  assert.equal(hubPeerName(PEER({ name: "" })), "Navegador");
  assert.equal(hubPeerName(PEER({ name: "   " })), "Navegador");
});

test("hubDeviceFromPeer maps to the hub route discriminator", () => {
  const device = hubDeviceFromPeer(PEER({ conn_id: "abc123", name: "Ana" }));
  assert.equal(device.key, "hub-session:abc123");
  assert.equal(device.name, "Ana");
  assert.equal(device.kind, "hub");
  assert.equal(device.online, true);
  assert.equal(device.ip, undefined, "hub peers never carry a LAN ip");
});

test("hubDeviceFromPeer keeps the raw name so the UI can label anonymous peers once", () => {
  assert.equal(hubDeviceFromPeer(PEER({ name: "" })).name, "");
  assert.equal(hubDeviceFromPeer(PEER({ name: "   " })).name, "");
});

test("mergeDevices appends hub peers after LAN devices without mutating inputs", () => {
  const lan: DeviceState[] = [
    { key: "Ana", name: "Ana", ip: "192.168.1.5", online: true },
    { key: "Beto", name: "Beto", ip: "192.168.1.6", online: false },
  ];
  const hub: DeviceState[] = [
    hubDeviceFromPeer(PEER({ conn_id: "w2", name: "Zulu" })),
    hubDeviceFromPeer(PEER({ conn_id: "w1", name: "Ana Web" })),
  ];
  const merged = mergeDevices(lan, hub);
  assert.deepEqual(
    merged.map((d) => d.key),
    ["Ana", "Beto", "hub-session:w1", "hub-session:w2"],
    "LAN keeps its order; hub peers follow sorted by name",
  );
  assert.equal(lan.length, 2, "inputs are not mutated");
  assert.equal(hub.length, 2);
});

test("the hub row preview is the honest pending-linking notice", () => {
  assert.equal(HUB_PEER_PREVIEW, "Vinculación web pendiente");
});
