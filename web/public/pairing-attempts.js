// Explicit lifecycle for outbound pairing attempts against the native
// desktop protocol. Each attempt carries a fresh reqId (UUID) minted by the
// caller; only the CURRENT attempt — same peer id, same reqId, still inside
// its request TTL — can be completed by a pair-ok. A cancelled, timed-out,
// superseded or exhausted (3 wrong codes) attempt can never complete.
//
// The helper holds no real timers: deadlines are computed from an injected
// clock, and the UI drives expiry checks (expired / verifyTimedOut) from its
// own scheduled callbacks. That keeps the state machine deterministic and
// testable. Wire message shapes are unchanged by this module.

export function createPairingAttempts({
  verifyTimeoutMs = 6000,
  requestTtlMs = 120000,
  maxWrongCodes = 3,
  now = () => Date.now(),
} = {}) {
  let attempt = null; // { appId, reqId, startedAt, verifyArmedAt, wrongCodes }
  let requestExpired = false;
  let verifyExpired = false;

  function clear() {
    attempt = null;
    requestExpired = false;
    verifyExpired = false;
  }

  function requestDeadline() {
    return attempt.startedAt + requestTtlMs;
  }

  function requestTimedOut() {
    return now() >= requestDeadline();
  }

  function verifyTimedOutAt() {
    return attempt.verifyArmedAt != null && now() >= attempt.verifyArmedAt + verifyTimeoutMs;
  }

  return {
    /** Current attempt, or null. Never exposes deadlines. */
    pending() {
      return attempt ? { appId: attempt.appId, reqId: attempt.reqId } : null;
    },

    /** A new attempt supersedes any prior one; the old reqId is dead. */
    start(appId, reqId) {
      clear();
      attempt = { appId, reqId, startedAt: now(), verifyArmedAt: null, wrongCodes: 0 };
      return { appId, reqId };
    },

    /** Marks a pair-verify as in flight for this attempt. */
    armVerify() {
      if (!attempt) return null;
      verifyExpired = false;
      attempt.verifyArmedAt = now();
      return now() + verifyTimeoutMs;
    },

    /** True while a pair-verify is awaiting its reply (blocks duplicates). */
    awaitingVerify() {
      return !!attempt && attempt.verifyArmedAt != null && !verifyTimedOutAt();
    },

    /**
     * Verify window elapsed. Fires once per arm; the attempt stays live so
     * the user may retry the same attempt within the request TTL.
     */
    verifyTimedOut() {
      if (!attempt || verifyExpired || !verifyTimedOutAt()) return false;
      verifyExpired = true;
      return true;
    },

    /** Request TTL elapsed. Fires once; the attempt is dead afterwards. */
    expired() {
      if (!attempt || requestExpired || !requestTimedOut()) return false;
      clear();
      return true;
    },

    /**
     * Only the current peer id AND current reqId, within TTL, can complete.
     * Returns true exactly once for the winning pair-ok.
     */
    accept(appId, reqId) {
      if (!attempt) return false;
      if (appId !== attempt.appId || reqId !== attempt.reqId) return false;
      if (requestTimedOut()) {
        clear();
        return false;
      }
      clear();
      return true;
    },

    /**
     * Handles a pair-error. Outcomes:
     *  - "stale":     identity does not match the current attempt; ignored.
     *  - "retry":     wrong code, attempt stays live (strike counted).
     *  - "exhausted": max wrong codes reached; attempt is dead.
     *  - "cleared":   busy / rate / unknown; attempt is dead, start a new one.
     */
    error(appId, reqId, reason) {
      if (
        !attempt ||
        appId !== attempt.appId ||
        reqId !== attempt.reqId ||
        requestTimedOut()
      ) {
        return { outcome: "stale" };
      }
      if (requestTimedOut()) return { outcome: "stale" };
      if (reason === "code") {
        // The reply ended the verify exchange: retrying must not wait for a
        // timeout that will never matter.
        attempt.verifyArmedAt = null;
        verifyExpired = false;
        attempt.wrongCodes += 1;
        if (attempt.wrongCodes >= maxWrongCodes) {
          clear();
          return { outcome: "exhausted", strikes: maxWrongCodes };
        }
        return { outcome: "retry", strikes: attempt.wrongCodes };
      }
      clear();
      return { outcome: "cleared" };
    },

    /**
     * Cancels the current attempt and returns its identity so the caller can
     * send a correlated pair-cancel. Null when nothing is pending or it
     * already expired.
     */
    cancel() {
      if (!attempt || requestTimedOut()) {
        clear();
        return null;
      }
      const info = { appId: attempt.appId, reqId: attempt.reqId };
      clear();
      return info;
    },
  };
}
