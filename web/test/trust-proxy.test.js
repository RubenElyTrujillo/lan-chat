// Trust proxy client-IP resolution: exact TRUST_PROXY allowlist, XFF rightmost
// only from trusted sockets, malformed/missing headers fall back to the socket.
// Run: npm test (from web/)
import { test } from "node:test";
import assert from "node:assert/strict";
import { normalizeIp, parseTrustedProxies, resolveClientIp } from "../trust-proxy.js";

const TRUSTED = parseTrustedProxies("172.17.0.1");

test("normalizeIp strips IPv4-mapped prefix, lowercases, rejects invalid", () => {
  assert.equal(normalizeIp("  ::ffff:192.168.1.5  "), "192.168.1.5");
  assert.equal(normalizeIp("::FFFF:1.2.3.4"), "1.2.3.4");
  assert.equal(normalizeIp("2001:DB8::ABC"), "2001:db8::abc");
  assert.equal(normalizeIp("198.51.100.9"), "198.51.100.9");
  assert.equal(normalizeIp(""), null);
  assert.equal(normalizeIp("banana"), null);
  assert.equal(normalizeIp("999.1.1.1"), null);
  assert.equal(normalizeIp("::ffff:banana"), null);
  assert.equal(normalizeIp(undefined), null);
});

test("TRUST_PROXY unset defaults to off: XFF ignored, socket used", () => {
  const off = parseTrustedProxies(undefined);
  assert.equal(off.size, 0);
  assert.equal(
    resolveClientIp({
      socketIp: "172.17.0.1",
      forwardedFor: "8.8.8.8",
      trustedProxies: off,
    }),
    "172.17.0.1",
  );
});

test("non-IP TRUST_PROXY entries are ignored, so no broad trust is possible", () => {
  const off = parseTrustedProxies("true, *, 10.0.0.0/8, ,");
  assert.equal(off.size, 0);
  assert.equal(
    resolveClientIp({
      socketIp: "172.17.0.1",
      forwardedFor: "8.8.8.8",
      trustedProxies: off,
    }),
    "172.17.0.1",
  );
});

test("exact trust matching only: nearby IP is not trusted", () => {
  assert.equal(
    resolveClientIp({
      socketIp: "172.17.0.2",
      forwardedFor: "1.2.3.4",
      trustedProxies: TRUSTED,
    }),
    "172.17.0.2",
  );
});

test("untrusted socket ignores client-supplied XFF spoof", () => {
  assert.equal(
    resolveClientIp({
      socketIp: "203.0.113.7",
      forwardedFor: "1.2.3.4",
      trustedProxies: TRUSTED,
    }),
    "203.0.113.7",
  );
});

test("trusted socket may use XFF IPv4", () => {
  assert.equal(
    resolveClientIp({
      socketIp: "172.17.0.1",
      forwardedFor: "198.51.100.9",
      trustedProxies: TRUSTED,
    }),
    "198.51.100.9",
  );
});

test("trusted socket may use XFF IPv6", () => {
  assert.equal(
    resolveClientIp({
      socketIp: "172.17.0.1",
      forwardedFor: "2001:db8::1",
      trustedProxies: TRUSTED,
    }),
    "2001:db8::1",
  );
});

test("IPv4-mapped socket and allowlist entries normalize before exact matching", () => {
  const trusted = parseTrustedProxies("::ffff:172.17.0.1");
  assert.equal(
    resolveClientIp({
      socketIp: "::ffff:172.17.0.1",
      forwardedFor: "198.51.100.9",
      trustedProxies: trusted,
    }),
    "198.51.100.9",
  );
});

test("rightmost XFF entry wins over client-supplied prefix", () => {
  assert.equal(
    resolveClientIp({
      socketIp: "172.17.0.1",
      forwardedFor: "1.2.3.4, 198.51.100.9",
      trustedProxies: TRUSTED,
    }),
    "198.51.100.9",
  );
});

test("repeated XFF headers resolve to the rightmost entry overall", () => {
  assert.equal(
    resolveClientIp({
      socketIp: "172.17.0.1",
      forwardedFor: ["9.9.9.9, 198.51.100.9", "2001:db8::2"],
      trustedProxies: TRUSTED,
    }),
    "2001:db8::2",
  );
});

test("malformed, empty or missing XFF falls back to the socket without crashing", () => {
  const cases = [undefined, "", "   ", "not-an-ip", "1.2.3.4, garbage"];
  for (const forwardedFor of cases) {
    assert.equal(
      resolveClientIp({
        socketIp: "172.17.0.1",
        forwardedFor,
        trustedProxies: TRUSTED,
      }),
      "172.17.0.1",
      `XFF ${JSON.stringify(forwardedFor)} must fall back to the socket`,
    );
  }
});

test("missing socket address degrades to empty grouping key, never throws", () => {
  assert.equal(
    resolveClientIp({
      socketIp: undefined,
      forwardedFor: "1.2.3.4",
      trustedProxies: TRUSTED,
    }),
    "",
  );
});
