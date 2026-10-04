// Explicit lifecycle for outbound pairing attempts. A cancelled, timed-out
// or superseded attempt can no longer be completed by a late pair-ok.
// Wire messages (pair-request / pair-verify / pair-ok) are unchanged.

export function createPairingAttempts({ timeoutMs = 6000, onTimeout } = {}) {
  let attempt = null; // { appId, timer }

  function clear() {
    if (attempt) clearTimeout(attempt.timer);
    attempt = null;
  }

  return {
    pending() {
      return attempt ? attempt.appId : null;
    },

    /** Supersedes any prior attempt; the prior timer never fires. */
    start(appId) {
      clear();
      attempt = {
        appId,
        timer: setTimeout(() => {
          attempt = null;
          onTimeout?.(appId);
        }, timeoutMs),
      };
      return appId;
    },

    /** Only a matching, still-pending attempt completes. */
    accept(appId) {
      if (!attempt || attempt.appId !== appId) return false;
      clear();
      return true;
    },

    /** Cancels the current attempt, or a specific app's attempt. */
    cancel(appId) {
      if (!attempt) return false;
      if (appId != null && attempt.appId !== appId) return false;
      clear();
      return true;
    },
  };
}
