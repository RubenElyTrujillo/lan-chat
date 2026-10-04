// Client-IP resolution for the hub. TRUST_PROXY is an exact, normalized IP
// allowlist (comma separated). Default is off: X-Forwarded-For is ignored and
// the socket address is used. Only a socket whose (normalized) IP is on the
// allowlist may contribute a client IP via X-Forwarded-For, and only its
// rightmost entry — the one the edge proxy (Caddy) appended last — is used.
// Any malformed or missing header falls back to the socket address.
import { isIP } from "node:net";

const V4_MAPPED = /^::ffff:(.+)$/i;

// "::ffff:192.168.1.5" -> "192.168.1.5"; lowercases IPv6; null if not an IP.
export function normalizeIp(raw) {
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim();
  if (!trimmed || !isIP(trimmed)) return null;
  const mapped = trimmed.match(V4_MAPPED);
  if (mapped && isIP(mapped[1]) === 4) return mapped[1];
  return trimmed.toLowerCase();
}

// TRUST_PROXY value -> Set of normalized exact IPs. Empty set = trust off.
// Entries that are not valid IPs ("true", "*", CIDR, etc.) are dropped.
export function parseTrustedProxies(value) {
  const set = new Set();
  if (typeof value !== "string") return set;
  for (const part of value.split(",")) {
    const ip = normalizeIp(part);
    if (ip) set.add(ip);
  }
  return set;
}

export function resolveClientIp({ socketIp, forwardedFor, trustedProxies }) {
  const socket = normalizeIp(socketIp) ?? "";
  if (!trustedProxies || trustedProxies.size === 0) return socket;
  if (!trustedProxies.has(socket)) return socket;

  const values = Array.isArray(forwardedFor)
    ? forwardedFor[forwardedFor.length - 1]
    : forwardedFor;
  if (typeof values !== "string" || !values.trim()) return socket;

  const entries = values.split(",");
  const ip = normalizeIp(entries[entries.length - 1]);
  return ip ?? socket;
}
