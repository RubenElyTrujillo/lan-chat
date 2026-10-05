// Presence metadata validation for hub hellos. Pure: no I/O, no globals.
// A sid or caps list that does not meet the bounds is OMITTED, never stored
// partially and never a crash: peers keep working as plain legacy entries.
//
// Bounds (hub-side contract, mirrored by the desktop presence parser):
// - sid: optional, 1..64 chars, [A-Za-z0-9_-] (UUIDs with hyphens are valid).
//   It is an UNTRUSTED HINT: never an authorization or identity guarantee,
//   duplicates are listed as separate peers keyed by conn id.
// - caps: optional array; valid string items (1..32 chars after trim) kept,
//   at most 8; an empty result is omitted.

export const SID_MAX = 64;
export const SID_PATTERN = /^[A-Za-z0-9_-]+$/;
export const CAPS_MAX = 8;
export const CAP_MAX = 32;

export function validSid(value) {
  return (
    typeof value === "string" &&
    value.length >= 1 &&
    value.length <= SID_MAX &&
    SID_PATTERN.test(value)
  );
}

export function parsePresence(hello) {
  const out = {};
  if (hello && validSid(hello.sid)) out.sid = hello.sid;
  if (Array.isArray(hello?.caps)) {
    const caps = hello.caps
      .filter((c) => typeof c === "string")
      .map((c) => c.trim())
      .filter((c) => c.length >= 1 && c.length <= CAP_MAX)
      .slice(0, CAPS_MAX);
    if (caps.length > 0) out.caps = caps;
  }
  return out;
}
