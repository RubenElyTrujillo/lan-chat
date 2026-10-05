// Strict outbound pairing lifecycle for the native desktop protocol.
// Every attempt carries a fresh reqId; only the CURRENT attempt (matching
// both peer id and reqId, within its TTLs) can complete. A cancelled,
// timed-out, superseded or exhausted attempt can never be completed by a
// late or forged pair-ok. Clock is injected: tests never use real timers.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createPairingAttempts } from "../public/pairing-attempts.js";

function clock() {
  let t = 1000;
  return {
    now: () => t,
    tick: (ms) => {
      t += ms;
    },
  };
}

function make(overrides = {}) {
  const c = clock();
  const attempts = createPairingAttempts({ now: c.now, ...overrides });
  return { attempts, tick: c.tick };
}

test("start records peer id and reqId; pending exposes both", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.deepEqual(attempts.pending(), { appId: "appA", reqId: "req-1" });
});

test("a new attempt supersedes the old one: old reqId can never complete", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  attempts.start("appA", "req-2");
  assert.equal(attempts.accept("appA", "req-1"), false, "stale reqId must not pair");
  assert.equal(attempts.accept("appA", "req-2"), true);
});

test("pair-ok from a different peer is rejected even with the current reqId", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.equal(attempts.accept("appB", "req-1"), false, "wrong peer must not pair");
  assert.equal(attempts.accept("appA", "req-1"), true);
});

test("malformed or missing reqId never completes an attempt", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.equal(attempts.accept("appA", undefined), false);
  assert.equal(attempts.accept("appA", null), false);
  assert.equal(attempts.accept("appA", ""), false);
});

test("cancel clears the attempt and returns its identity for pair-cancel", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.deepEqual(attempts.cancel(), { appId: "appA", reqId: "req-1" });
  assert.equal(attempts.pending(), null);
  assert.equal(attempts.accept("appA", "req-1"), false, "late ok must not navigate");
});

test("cancel with no pending attempt returns null", () => {
  const { attempts } = make();
  assert.equal(attempts.cancel(), null);
});

test("double accept completes only once", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.equal(attempts.accept("appA", "req-1"), true);
  assert.equal(attempts.accept("appA", "req-1"), false);
});

test("verify timeout: attempt stays live within TTL and may be retried", () => {
  const { attempts, tick } = make({ verifyTimeoutMs: 6000 });
  attempts.start("appA", "req-1");
  attempts.armVerify();
  tick(6001);
  assert.equal(attempts.verifyTimedOut(), true, "verify window elapsed");
  assert.deepEqual(attempts.pending(), { appId: "appA", reqId: "req-1" }, "attempt survives for retry");
  assert.equal(attempts.accept("appA", "req-1"), true, "late ok right after timeout is still valid within TTL");
});

test("verifyTimedOut is only true once per armed verify", () => {
  const { attempts, tick } = make({ verifyTimeoutMs: 6000 });
  attempts.start("appA", "req-1");
  attempts.armVerify();
  tick(6001);
  assert.equal(attempts.verifyTimedOut(), true);
  assert.equal(attempts.verifyTimedOut(), false, "disarmed after firing");
});

test("a fresh verify arm resets the verify window", () => {
  const { attempts, tick } = make({ verifyTimeoutMs: 6000 });
  attempts.start("appA", "req-1");
  attempts.armVerify();
  tick(3000);
  attempts.armVerify(); // retry same live attempt
  tick(5000);
  assert.equal(attempts.verifyTimedOut(), false, "not yet: only 5s since last arm");
  tick(1001);
  assert.equal(attempts.verifyTimedOut(), true);
});

test("awaitingVerify blocks duplicate verify sends until timeout or reply", () => {
  const { attempts, tick } = make({ verifyTimeoutMs: 6000 });
  attempts.start("appA", "req-1");
  assert.equal(attempts.awaitingVerify(), false);
  attempts.armVerify();
  assert.equal(attempts.awaitingVerify(), true);
  tick(6001);
  attempts.verifyTimedOut();
  assert.equal(attempts.awaitingVerify(), false, "retry allowed after verify timeout");
});

test("request expires after the 120s TTL: accept fails and attempt is gone", () => {
  const { attempts, tick } = make({ requestTtlMs: 120000 });
  attempts.start("appA", "req-1");
  tick(120001);
  assert.equal(attempts.expired(), true);
  assert.equal(attempts.pending(), null);
  assert.equal(attempts.accept("appA", "req-1"), false, "expired request cannot pair");
});

test("expired is only true once", () => {
  const { attempts, tick } = make({ requestTtlMs: 120000 });
  attempts.start("appA", "req-1");
  tick(120001);
  assert.equal(attempts.expired(), true);
  assert.equal(attempts.expired(), false);
});

test("cancel after TTL expiry returns null, nothing to correlate", () => {
  const { attempts, tick } = make({ requestTtlMs: 120000 });
  attempts.start("appA", "req-1");
  tick(120001);
  attempts.expired();
  assert.equal(attempts.cancel(), null);
});

test("wrong-code reply disarms the pending verify: immediate retry allowed", () => {
  const { attempts } = make({ verifyTimeoutMs: 6000 });
  attempts.start("appA", "req-1");
  attempts.armVerify();
  assert.equal(attempts.awaitingVerify(), true);
  assert.deepEqual(attempts.error("appA", "req-1", "code"), { outcome: "retry", strikes: 1 });
  assert.equal(attempts.awaitingVerify(), false, "reply received: no need to wait the window");
  assert.equal(attempts.verifyTimedOut(), false, "no spurious timeout after an explicit error");
});

test("wrong code keeps the attempt alive for retry (up to 3 strikes)", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.deepEqual(attempts.error("appA", "req-1", "code"), { outcome: "retry", strikes: 1 });
  assert.deepEqual(attempts.error("appA", "req-1", "code"), { outcome: "retry", strikes: 2 });
  assert.deepEqual(attempts.pending(), { appId: "appA", reqId: "req-1" }, "still live between strikes");
  assert.deepEqual(attempts.error("appA", "req-1", "code"), { outcome: "exhausted", strikes: 3 });
  assert.equal(attempts.pending(), null, "3 strikes kill the attempt: no fake success path");
  assert.equal(attempts.accept("appA", "req-1"), false);
});

test("busy clears the attempt: user must start a new one", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.deepEqual(attempts.error("appA", "req-1", "busy"), { outcome: "cleared" });
  assert.equal(attempts.pending(), null);
});

test("rate clears the attempt: user must start a new one", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.deepEqual(attempts.error("appA", "req-1", "rate"), { outcome: "cleared" });
  assert.equal(attempts.pending(), null);
});

test("pair-error with stale or malformed identity is ignored", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.deepEqual(attempts.error("appB", "req-1", "code").outcome, "stale");
  assert.deepEqual(attempts.error("appA", "req-9", "code").outcome, "stale");
  assert.deepEqual(attempts.error("appA", undefined, "code").outcome, "stale");
  assert.deepEqual(attempts.pending(), { appId: "appA", reqId: "req-1" }, "untouched by stale errors");
});

test("unknown error reason clears the attempt (safe default)", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  assert.deepEqual(attempts.error("appA", "req-1", "wat"), { outcome: "cleared" });
  assert.equal(attempts.pending(), null);
});

test("wrong-code strikes count only within the same attempt", () => {
  const { attempts } = make();
  attempts.start("appA", "req-1");
  attempts.error("appA", "req-1", "code");
  attempts.error("appA", "req-1", "code");
  attempts.start("appB", "req-2"); // new attempt: strike counter resets
  assert.deepEqual(attempts.error("appB", "req-2", "code"), { outcome: "retry", strikes: 1 });
});

test("verify timeout does not keep a TTL-expired attempt alive", () => {
  const { attempts, tick } = make({ verifyTimeoutMs: 6000, requestTtlMs: 120000 });
  attempts.start("appA", "req-1");
  attempts.armVerify();
  tick(120001);
  assert.equal(attempts.accept("appA", "req-1"), false, "TTL wins over everything");
});
