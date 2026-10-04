// Lifecycle of outbound pairing attempts: cancel, timeout, superseding
// attempts, and late responses that must not complete anything.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createPairingAttempts } from "../public/pairing-attempts.js";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

test("a cancelled attempt cannot be completed by a late response", () => {
  const timeouts = [];
  const attempts = createPairingAttempts({ timeoutMs: 50, onTimeout: (id) => timeouts.push(id) });
  attempts.start("appA");
  assert.equal(attempts.cancel(), true);
  assert.equal(attempts.pending(), null);
  assert.equal(attempts.accept("appA"), false, "late pair-ok must not navigate the UI");
});

test("an old app's response does not complete a newer attempt", () => {
  const attempts = createPairingAttempts({ timeoutMs: 100 });
  attempts.start("appA");
  attempts.start("appB"); // superseding attempt invalidates the first
  assert.equal(attempts.accept("appA"), false);
  assert.equal(attempts.pending(), "appB");
  assert.equal(attempts.accept("appB"), true);
  assert.equal(attempts.pending(), null);
});

test("timeout fires once, only for the attempt that actually timed out", async () => {
  const timeouts = [];
  const attempts = createPairingAttempts({ timeoutMs: 5, onTimeout: (id) => timeouts.push(id) });
  attempts.start("appA");
  attempts.start("appB"); // appA's timer is cleared, must never fire
  await sleep(25);
  assert.deepEqual(timeouts, ["appB"]);
  assert.equal(attempts.pending(), null);
  assert.equal(attempts.accept("appB"), false, "timed-out attempt is dead");
});

test("double accept completes only once", () => {
  const attempts = createPairingAttempts({ timeoutMs: 100 });
  attempts.start("appA");
  assert.equal(attempts.accept("appA"), true);
  assert.equal(attempts.accept("appA"), false);
});

test("cancel with an app id only clears that app's attempt", () => {
  const attempts = createPairingAttempts({ timeoutMs: 100 });
  attempts.start("appB");
  assert.equal(attempts.cancel("appA"), false);
  assert.equal(attempts.pending(), "appB");
  assert.equal(attempts.cancel("appB"), true);
  assert.equal(attempts.pending(), null);
});

test("timeout callback may safely cancel", async () => {
  const attempts = createPairingAttempts({
    timeoutMs: 5,
    onTimeout: () => attempts.cancel(),
  });
  attempts.start("appA");
  await sleep(20);
  assert.equal(attempts.pending(), null);
});
