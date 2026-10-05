// Hub status chip lifecycle tests (pure, DOM-free, run with: node --test tests/hub-status.test.ts)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  hubStatusLabel,
  hubStatusTitle,
  hubStatusTone,
  parseHubStatus,
  trackHubStatus,
  type HubStatus,
  type HubStatusAdapter,
} from "../src/lib/hub-status.ts";

const flush = () => new Promise((r) => setTimeout(r, 0));

const STATUS = (over: Partial<HubStatus> = {}): HubStatus => ({
  url: "wss://hub.test/hub",
  connected: true,
  phase: "connected",
  session_id: "s1",
  ...over,
});

function capture() {
  const seen: HubStatus[] = [];
  let unavailable = 0;
  return {
    seen,
    unavailable: () => unavailable,
    handlers: {
      onStatus: (s: HubStatus) => seen.push(s),
      onUnavailable: () => {
        unavailable++;
      },
    },
  };
}

test("parseHubStatus accepts a valid status with optional fields", () => {
  const full = parseHubStatus({
    url: "wss://h/hub",
    connected: true,
    phase: "connected",
    session_id: "s9",
  });
  assert.deepEqual(full, {
    url: "wss://h/hub",
    connected: true,
    phase: "connected",
    session_id: "s9",
  });
  const minimal = parseHubStatus({ url: "wss://h/hub", connected: false, phase: "backoff" });
  assert.equal(minimal?.phase, "backoff");
  assert.equal(minimal && "session_id" in minimal, false);
  assert.equal(minimal && "reason" in minimal, false);
});

test("parseHubStatus rejects garbage payloads", () => {
  assert.equal(parseHubStatus(null), null);
  assert.equal(parseHubStatus("connected"), null);
  assert.equal(parseHubStatus({ connected: true, phase: "connected" }), null);
  assert.equal(parseHubStatus({ url: "wss://h", connected: "yes", phase: "connected" }), null);
  assert.equal(parseHubStatus({ url: "wss://h", connected: true, phase: "online" }), null);
});

test("applies the initial query snapshot when no events arrive", async () => {
  const cap = capture();
  const adapter: HubStatusAdapter = {
    on: async () => () => {},
    get: async () => STATUS({ connected: false, phase: "connecting", session_id: undefined }),
  };
  const stop = trackHubStatus(adapter, cap.handlers);
  await flush();
  assert.deepEqual(cap.seen.map((s) => s.phase), ["connecting"]);
  stop();
});

test("ignores a stale snapshot when an event arrives before the query resolves", async () => {
  const cap = capture();
  let fire: ((s: HubStatus) => void) | null = null;
  const adapter: HubStatusAdapter = {
    on: async (h) => {
      fire = h;
      return () => {
        fire = null;
      };
    },
    get: async () => {
      fire!(STATUS({ connected: true, phase: "connected" }));
      return STATUS({ connected: false, phase: "connecting", session_id: undefined });
    },
  };
  const stop = trackHubStatus(adapter, cap.handlers);
  await flush();
  assert.deepEqual(cap.seen.map((s) => s.phase), ["connected"]);
  stop();
});

test("a newer event wins over a snapshot still in flight; later events keep applying", async () => {
  const cap = capture();
  let fire: ((s: HubStatus) => void) | null = null;
  let resolveGet: ((s: HubStatus) => void) = () => {};
  const adapter: HubStatusAdapter = {
    on: async (h) => {
      fire = h;
      return () => {};
    },
    get: () =>
      new Promise<HubStatus>((resolve) => {
        resolveGet = resolve;
      }),
  };
  const stop = trackHubStatus(adapter, cap.handlers);
  await flush();
  assert.equal(cap.seen.length, 0);
  fire!(STATUS({ connected: true, phase: "connected", session_id: "new" }));
  assert.deepEqual(cap.seen.map((s) => s.phase), ["connected"]);
  resolveGet(STATUS({ connected: false, phase: "connecting", session_id: undefined }));
  await flush();
  assert.equal(cap.seen.length, 1);
  fire!(STATUS({ connected: false, phase: "backoff", session_id: undefined, reason: "closed" }));
  assert.deepEqual(cap.seen.map((s) => s.phase), ["connected", "backoff"]);
  stop();
});

test("reports unavailable and shows nothing stale when the query fails", async () => {
  const cap = capture();
  const adapter: HubStatusAdapter = {
    on: async () => () => {},
    get: async () => {
      throw new Error("hub down");
    },
  };
  const stop = trackHubStatus(adapter, cap.handlers);
  await flush();
  assert.equal(cap.unavailable(), 1);
  assert.equal(cap.seen.length, 0);
  stop();
});

test("reports unavailable when the subscription itself fails", async () => {
  const cap = capture();
  let gets = 0;
  const adapter: HubStatusAdapter = {
    on: async () => {
      throw new Error("listen failed");
    },
    get: async () => {
      gets++;
      return STATUS();
    },
  };
  const stop = trackHubStatus(adapter, cap.handlers);
  await flush();
  assert.equal(cap.unavailable(), 1);
  assert.equal(gets, 0);
  stop();
});

test("dispose before the subscription resolves unlistens and never queries", async () => {
  let offCalls = 0;
  let gets = 0;
  let resolveOn: (off: () => void) => void = () => {};
  const adapter: HubStatusAdapter = {
    on: () =>
      new Promise((resolve) => {
        resolveOn = resolve;
      }),
    get: async () => {
      gets++;
      return STATUS();
    },
  };
  const stop = trackHubStatus(adapter, {
    onStatus: () => assert.fail("no status after dispose"),
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
  let secondFire: ((s: HubStatus) => void) | null = null;
  let n = 0;
  const cap = capture();
  const adapter: HubStatusAdapter = {
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
    get: async () => STATUS(),
  };
  const stopFirst = trackHubStatus(adapter, cap.handlers);
  stopFirst();
  const stopSecond = trackHubStatus(adapter, cap.handlers);
  await flush();
  assert.equal(firstOff, 1);
  // The second mount's own initial query already landed; a live event updates it.
  assert.equal(cap.seen.at(-1)?.phase, "connected");
  secondFire!(STATUS({ connected: false, phase: "backoff", session_id: undefined }));
  assert.equal(cap.seen.at(-1)?.phase, "backoff");
  stopSecond();
});

test("events arriving after dispose are ignored", async () => {
  const cap = capture();
  let fire: ((s: HubStatus) => void) | null = null;
  const adapter: HubStatusAdapter = {
    on: async (h) => {
      fire = h;
      return () => {};
    },
    get: async () => STATUS(),
  };
  const stop = trackHubStatus(adapter, cap.handlers);
  await flush();
  const before = cap.seen.length;
  stop();
  fire!(STATUS({ phase: "backoff", connected: false }));
  assert.equal(cap.seen.length, before);
  assert.notEqual(cap.seen.at(-1)?.phase, "backoff");
});

test("unparsable events are ignored and do not invalidate the in-flight snapshot", async () => {
  const cap = capture();
  let fire: ((raw: unknown) => void) | null = null;
  const adapter: HubStatusAdapter = {
    on: async (h) => {
      fire = h;
      return () => {};
    },
    get: async () => {
      fire!({ nonsense: true });
      return STATUS({ phase: "backoff", connected: false });
    },
  };
  const stop = trackHubStatus(adapter, cap.handlers);
  await flush();
  assert.deepEqual(cap.seen.map((s) => s.phase), ["backoff"]);
  stop();
});

test("Spanish labels match the approved hub states", () => {
  assert.equal(
    hubStatusLabel({ kind: "status", status: STATUS({ phase: "connected" }) }),
    "Hub conectado",
  );
  assert.equal(
    hubStatusLabel({ kind: "status", status: STATUS({ phase: "connecting", connected: false }) }),
    "Conectando al hub",
  );
  assert.equal(
    hubStatusLabel({
      kind: "status",
      status: STATUS({ phase: "backoff", connected: false, session_id: undefined }),
    }),
    "Reintentando hub",
  );
  assert.equal(
    hubStatusLabel({ kind: "status", status: STATUS({ phase: "disabled", connected: false }) }),
    "Hub no disponible",
  );
  assert.equal(hubStatusLabel({ kind: "connecting" }), "Conectando al hub");
  assert.equal(hubStatusLabel({ kind: "unavailable" }), "Hub no disponible");
  assert.equal(hubStatusLabel({ kind: "demo" }), "Hub · demo");
});

test("tones keep the color-free state rule paired with the words", () => {
  assert.equal(hubStatusTone({ kind: "status", status: STATUS({ phase: "connected" }) }), "ok");
  assert.equal(hubStatusTone({ kind: "status", status: STATUS({ phase: "backoff", connected: false }) }), "retry");
  assert.equal(hubStatusTone({ kind: "unavailable" }), "off");
  assert.equal(hubStatusTone({ kind: "demo" }), "demo");
});

test("titles are honest and generic: active link, no chat over the hub, no raw errors", () => {
  const ok = hubStatusTitle({ kind: "status", status: STATUS({ phase: "connected" }) });
  assert.match(ok, /Enlace.*activo/);
  assert.match(ok, /local/i);
  assert.doesNotMatch(ok, /retransmi|transfiere/i);
  assert.doesNotMatch(ok, /chat por el hub/i);
  const unavailable = hubStatusTitle({ kind: "unavailable" });
  assert.doesNotMatch(unavailable, /hub down/);
  const demo = hubStatusTitle({ kind: "demo" });
  assert.match(demo, /demo/i);
});
