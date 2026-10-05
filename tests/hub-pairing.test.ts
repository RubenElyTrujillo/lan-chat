// Hub browser-pairing UI tests (pure, DOM-free, run with: node --test tests/hub-pairing.test.ts)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  HUB_PAIRED_LABEL,
  MAX_PAIRED,
  MAX_PENDING,
  applyCancelResult,
  formatCountdown,
  isExpired,
  pairedConnIds,
  parseHubPairing,
  receiptDeadline,
  remainingMs,
  trackHubPairing,
  type HubPairingAdapter,
  type HubPairingSnapshot,
} from "../src/lib/hub-pairing.ts";

const flush = () => new Promise((r) => setTimeout(r, 0));

const PENDING = (over: Record<string, unknown> = {}) => ({
  conn_id: "c1",
  name: "Navegador de salón",
  req_id: "r1",
  code: "12345678",
  expires_in_ms: 120000,
  ...over,
});

const SNAPSHOT = (over: Partial<HubPairingSnapshot> = {}): HubPairingSnapshot => ({
  pending: [PENDING()],
  paired: [],
  ...over,
});

// ── Parser ───────────────────────────────────────────────────────────────────

test("parseHubPairing accepts a valid snapshot", () => {
  const snap = parseHubPairing({
    pending: [PENDING()],
    paired: [{ conn_id: "c1", req_id: "r1" }],
  });
  assert.deepEqual(snap, {
    pending: [PENDING()],
    paired: [{ conn_id: "c1", req_id: "r1" }],
  });
});

test("parseHubPairing rejects garbage payloads", () => {
  assert.equal(parseHubPairing(null), null);
  assert.equal(parseHubPairing("snapshot"), null);
  assert.equal(parseHubPairing({ pending: "[]" }), null);
  assert.equal(parseHubPairing({ paired: [] }), null);
});

test("invalid pending entries are ignored, valid ones survive", () => {
  const snap = parseHubPairing({
    pending: [
      PENDING(),
      PENDING({ conn_id: "" }),
      PENDING({ code: "1234" }),
      PENDING({ code: "123456789" }),
      PENDING({ code: "abcd5678" }),
      PENDING({ expires_in_ms: -1 }),
      PENDING({ expires_in_ms: "soon" }),
      PENDING({ req_id: "" }),
      PENDING({ req_id: "bad id!" }),
      PENDING({ conn_id: 12 }),
      null,
    ],
    paired: [],
  });
  assert.deepEqual(snap?.pending, [PENDING()]);
});

test("invalid paired entries are ignored", () => {
  const snap = parseHubPairing({
    pending: [],
    paired: [
      { conn_id: "c1", req_id: "r1" },
      { conn_id: "", req_id: "r2" },
      { conn_id: "c3", req_id: "" },
      { conn_id: 9, req_id: "r4" },
      "c5",
    ],
  });
  assert.deepEqual(snap?.paired, [{ conn_id: "c1", req_id: "r1" }]);
});

test(`pending is capped at ${MAX_PENDING} and paired at ${MAX_PAIRED}`, () => {
  const pendings = [];
  for (let i = 0; i < 5; i++) pendings.push(PENDING({ conn_id: `c${i}`, req_id: `r${i}` }));
  const paired = [];
  for (let i = 0; i < 40; i++) paired.push({ conn_id: `p${i}`, req_id: `r${i}` });
  const snap = parseHubPairing({ pending: pendings, paired });
  assert.equal(snap?.pending.length, MAX_PENDING);
  assert.equal(snap?.paired.length, MAX_PAIRED);
});

test("duplicate peer names stay separate entries keyed by conn id", () => {
  const snap = parseHubPairing({
    pending: [PENDING({ conn_id: "a" }), PENDING({ conn_id: "b" })],
    paired: [],
  });
  assert.equal(snap?.pending.length, 2);
  assert.equal(snap?.pending[0].name, snap?.pending[1].name);
});

test("overlong names truncate; overlong conn/req ids drop the entry (presence rule)", () => {
  const snap = parseHubPairing({
    pending: [PENDING({ name: "x".repeat(60) })],
    paired: [],
  });
  assert.ok(snap?.pending[0].name.length && snap?.pending[0].name.length <= 48);
  assert.equal(
    parseHubPairing({ pending: [PENDING({ conn_id: "c".repeat(80) })], paired: [] })?.pending.length,
    0,
  );
  assert.equal(
    parseHubPairing({ pending: [PENDING({ req_id: "r".repeat(80) })], paired: [] })?.pending.length,
    0,
  );
});

test("expires_in_ms zero is valid (already expired at the source)", () => {
  const snap = parseHubPairing({ pending: [PENDING({ expires_in_ms: 0 })], paired: [] });
  assert.equal(snap?.pending.length, 1);
  assert.equal(snap?.pending[0].expires_in_ms, 0);
});

// ── Countdown helpers ────────────────────────────────────────────────────────

test("receiptDeadline anchors expiry on the local monotonic receipt moment", () => {
  assert.equal(receiptDeadline(120000, 1000), 121000);
  assert.equal(receiptDeadline(0, 500), 500);
});

test("remainingMs never goes negative", () => {
  assert.equal(remainingMs(10000, 9000), 1000);
  assert.equal(remainingMs(10000, 10000), 0);
  assert.equal(remainingMs(10000, 20000), 0);
});

test("isExpired is true at local zero and after", () => {
  assert.equal(isExpired(10000, 9999), false);
  assert.equal(isExpired(10000, 10000), true);
  assert.equal(isExpired(10000, 10001), true);
});

test("formatCountdown renders m:ss, rounding up", () => {
  assert.equal(formatCountdown(120000), "2:00");
  assert.equal(formatCountdown(61000), "1:01");
  assert.equal(formatCountdown(1000), "0:01");
  assert.equal(formatCountdown(1), "0:01");
  assert.equal(formatCountdown(0), "0:00");
  assert.equal(formatCountdown(-5), "0:00");
});

// ── Cancellation: captured ids, late results never hit a new request ────────

test("applyCancelResult removes only the exact (conn_id, req_id) entry", () => {
  const list = [
    PENDING({ conn_id: "c1", req_id: "r1" }),
    PENDING({ conn_id: "c2", req_id: "r2" }),
  ];
  const next = applyCancelResult(list, "c1", "r1");
  assert.deepEqual(next, [PENDING({ conn_id: "c2", req_id: "r2" })]);
});

test("a replacement request on the same conn survives a late cancel", () => {
  const list = [
    PENDING({ conn_id: "c1", req_id: "r-old" }),
    PENDING({ conn_id: "c1", req_id: "r-new" }),
  ];
  const next = applyCancelResult(list, "c1", "r-old");
  assert.deepEqual(next, [PENDING({ conn_id: "c1", req_id: "r-new" })]);
});

test("applyCancelResult on unknown ids changes nothing", () => {
  const list = [PENDING()];
  assert.deepEqual(applyCancelResult(list, "zz", "zz"), list);
  assert.deepEqual(applyCancelResult(list, "c1", "nope"), list);
});

// ── Paired view ──────────────────────────────────────────────────────────────

test("pairedConnIds is keyed by conn id only (never sid or name)", () => {
  const ids = pairedConnIds(SNAPSHOT({ paired: [{ conn_id: "c1", req_id: "r1" }] }));
  assert.deepEqual([...ids], ["c1"]);
  assert.equal(pairedConnIds(SNAPSHOT({ paired: [] })).size, 0);
});

test("the paired label is the short honest tag now that hub chat exists", () => {
  assert.equal(HUB_PAIRED_LABEL, "Vinculado");
});

// ── Lifecycle (same race-safe contract as trackHubStatus/trackHubPresence) ──

function capture() {
  const seen: HubPairingSnapshot[] = [];
  let unavailable = 0;
  return {
    seen,
    unavailable: () => unavailable,
    handlers: {
      onSnapshot: (s: HubPairingSnapshot) => seen.push(s),
      onUnavailable: () => {
        unavailable++;
      },
    },
  };
}

test("applies the initial query snapshot when no events arrive", async () => {
  const cap = capture();
  const adapter: HubPairingAdapter = {
    on: async () => () => {},
    get: async () => SNAPSHOT({ pending: [] }),
  };
  const stop = trackHubPairing(adapter, cap.handlers);
  await flush();
  assert.deepEqual(cap.seen, [SNAPSHOT({ pending: [] })]);
  stop();
});

test("a stale query snapshot never overrides an event that arrived first", async () => {
  const cap = capture();
  let fire: ((s: HubPairingSnapshot) => void) | null = null;
  const adapter: HubPairingAdapter = {
    on: async (h) => {
      fire = h;
      return () => {};
    },
    get: async () => {
      fire!(SNAPSHOT({ pending: [] }));
      return SNAPSHOT();
    },
  };
  const stop = trackHubPairing(adapter, cap.handlers);
  await flush();
  assert.deepEqual(cap.seen, [SNAPSHOT({ pending: [] })]);
  stop();
});

test("unparsable events are ignored and do not invalidate the in-flight snapshot", async () => {
  const cap = capture();
  let fire: ((raw: unknown) => void) | null = null;
  const adapter: HubPairingAdapter = {
    on: async (h) => {
      fire = h;
      return () => {};
    },
    get: async () => {
      fire!({ nonsense: true });
      return SNAPSHOT();
    },
  };
  const stop = trackHubPairing(adapter, cap.handlers);
  await flush();
  assert.deepEqual(cap.seen, [SNAPSHOT()]);
  stop();
});

test("reports unavailable and shows nothing when the query fails", async () => {
  const cap = capture();
  const adapter: HubPairingAdapter = {
    on: async () => () => {},
    get: async () => {
      throw new Error("hub down");
    },
  };
  const stop = trackHubPairing(adapter, cap.handlers);
  await flush();
  assert.equal(cap.unavailable(), 1);
  assert.equal(cap.seen.length, 0);
  stop();
});

test("dispose before the subscription resolves unlistens and never queries", async () => {
  let offCalls = 0;
  let gets = 0;
  let resolveOn: (off: () => void) => void = () => {};
  const adapter: HubPairingAdapter = {
    on: () =>
      new Promise((resolve) => {
        resolveOn = resolve;
      }),
    get: async () => {
      gets++;
      return SNAPSHOT();
    },
  };
  const stop = trackHubPairing(adapter, {
    onSnapshot: () => assert.fail("no snapshot after dispose"),
  });
  stop();
  resolveOn(() => {
    offCalls++;
  });
  await flush();
  assert.equal(offCalls, 1);
  assert.equal(gets, 0);
});

test("StrictMode remount: first mount cleans up, second stays live", async () => {
  let firstOff = 0;
  let secondFire: ((s: HubPairingSnapshot) => void) | null = null;
  let n = 0;
  const cap = capture();
  const adapter: HubPairingAdapter = {
    on: async (h) => {
      n++;
      if (n === 1) {
        return () => {
          firstOff++;
        };
      }
      secondFire = h;
      return () => {};
    },
    get: async () => SNAPSHOT(),
  };
  const stopFirst = trackHubPairing(adapter, cap.handlers);
  stopFirst();
  const stopSecond = trackHubPairing(adapter, cap.handlers);
  await flush();
  assert.equal(firstOff, 1);
  secondFire!(SNAPSHOT({ pending: [] }));
  assert.deepEqual(cap.seen.at(-1), SNAPSHOT({ pending: [] }));
  stopSecond();
});

test("events after dispose are ignored", async () => {
  const cap = capture();
  let fire: ((s: HubPairingSnapshot) => void) | null = null;
  const adapter: HubPairingAdapter = {
    on: async (h) => {
      fire = h;
      return () => {};
    },
    get: async () => SNAPSHOT(),
  };
  const stop = trackHubPairing(adapter, cap.handlers);
  await flush();
  stop();
  fire!(SNAPSHOT({ pending: [] }));
  assert.deepEqual(cap.seen, [SNAPSHOT()]);
});
