// Session id (sid) minting for the web client: one ephemeral id per page
// load, sent with hello, never persisted. Pure factory so it runs under
// node --test with injected crypto.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createSessionId } from "../public/session-id.js";

test("mints a sid within the hub's bounds and charset", () => {
  const sid = createSessionId();
  assert.match(sid, /^[A-Za-z0-9_-]{1,64}$/, "sid must be ASCII-safe for the hub");
});

test("mints a fresh sid on every call: no memoization, no persistence", () => {
  const a = createSessionId();
  const b = createSessionId();
  assert.notEqual(a, b);
});

test("uses crypto.randomUUID when available", () => {
  const sid = createSessionId({ cryptoObj: { randomUUID: () => "0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f" } });
  assert.equal(sid, "0b7f9a1e-6c2d-4f8a-9d3e-1a2b3c4d5e6f");
});

test("falls back to getRandomValues when randomUUID is missing (insecure contexts)", () => {
  const fake = {
    getRandomValues: (arr) => {
      arr.fill(7);
      return arr;
    },
  };
  const sid = createSessionId({ cryptoObj: fake });
  assert.equal(sid, "7-7-7-7");
  assert.match(sid, /^[A-Za-z0-9_-]{1,64}$/);
});
