// Clipboard share core tests (pure, DOM-free, run with: node --test tests/clipboard-core.test.ts)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  CLIP_EMPTY_OR_LONG,
  RESOLVE_NO_PAIRED,
  RESOLVE_ALL_OFFLINE,
  SEND_PAIR_REQUIRED,
  SEND_PEER_OFFLINE,
  SEND_HUB_UNAVAILABLE,
  WRITE_COPY_FAILED,
  isValidClipboardText,
  planClipboardResolve,
  planClipboardSend,
  planHubClipboardInbound,
  planLanClipboardReceived,
  resolveTarget,
} from "../src/lib/clipboard-core.ts";

const C = (key: string, name: string) => ({ key, name });

// ── Bounds (mirror src-tauri/src/clipboard.rs: MAX_CLIPBOARD_TEXT chars) ────

test("isValidClipboardText: empty is invalid, exactly MAX chars is valid", () => {
  assert.equal(isValidClipboardText(""), false);
  assert.equal(isValidClipboardText("   "), false, "whitespace-only is invalid, mirroring native trim");
  assert.equal(isValidClipboardText(" \t\n "), false, "every all-whitespace variant is invalid");
  assert.equal(isValidClipboardText(" h "), true, "inner content with surrounding spaces is text");
  assert.equal(isValidClipboardText("x".repeat(64_000)), true);
  assert.equal(isValidClipboardText("x".repeat(64_001)), false);
  assert.equal(isValidClipboardText("é".repeat(64_000)), true, "counted in chars, not bytes");
  assert.equal(isValidClipboardText("é".repeat(64_001)), false);
});

test("bounds constant matches the native cap", () => {
  assert.equal(isValidClipboardText("a".repeat(64_000)), true);
});

// ── resolveTarget: 0 / 1 / many, dedupe by key, prefer online ───────────────

test("resolveTarget: no paired contacts is none/no-paired", () => {
  assert.deepEqual(resolveTarget([], new Set()), { kind: "none", reason: "no-paired" });
});

test("resolveTarget: paired contacts with none online is none/all-offline", () => {
  assert.deepEqual(
    resolveTarget([C("hub:u-1", "Salón")], new Set()),
    { kind: "none", reason: "all-offline" },
  );
});

test("resolveTarget: exactly one online contact resolves single", () => {
  assert.deepEqual(
    resolveTarget([C("hub:u-1", "Salón")], new Set(["hub:u-1"])),
    { kind: "single", key: "hub:u-1", name: "Salón", route: "hub" },
  );
});

test("resolveTarget: many paired but only one online prefers that one (single)", () => {
  const target = resolveTarget(
    [C("hub:u-1", "Salón"), C("hub:u-2", "Estudio")],
    new Set(["hub:u-2"]),
  );
  assert.deepEqual(target, { kind: "single", key: "hub:u-2", name: "Estudio", route: "hub" });
});

test("resolveTarget: several online contacts offer a picker, online first", () => {
  const target = resolveTarget(
    [C("hub:u-1", "Salón"), C("hub:u-2", "Estudio"), C("hub:u-3", "Offline")],
    new Set(["hub:u-1", "hub:u-2"]),
  );
  assert.equal(target.kind, "pick");
  if (target.kind === "pick") {
    assert.deepEqual(
      target.options.map((o) => o.key),
      ["hub:u-1", "hub:u-2"],
      "only online contacts are offered",
    );
  }
});

test("resolveTarget: dedupes by contact key keeping the first name", () => {
  const target = resolveTarget(
    [C("hub:u-1", "Salón"), C("hub:u-1", "Duplicada"), C("hub:u-2", "Estudio")],
    new Set(["hub:u-1", "hub:u-2"]),
  );
  assert.equal(target.kind, "pick");
  if (target.kind === "pick") assert.equal(target.options.length, 2);
  if (target.kind === "pick") assert.equal(target.options[0].name, "Salón");
});

test("resolveTarget: blank names and non-contact keys are dropped", () => {
  assert.deepEqual(
    resolveTarget([C("hub:u-1", "  ")], new Set(["hub:u-1"])),
    { kind: "none", reason: "no-paired" },
  );
  assert.deepEqual(
    resolveTarget([C("lan:x", "LAN"), C("hub:u-1", "Salón")], new Set(["lan:x", "hub:u-1"])),
    { kind: "single", key: "hub:u-1", name: "Salón", route: "hub" },
  );
});

// ── resolveTarget + LAN devices: unified hub/lan options ────────────────────

const LAN = (key: string, name: string, online: boolean, hasPin: boolean) => ({
  key,
  name,
  online,
  hasPin,
});

test("resolveTarget: online LAN device with pin joins hub contacts in the picker", () => {
  const target = resolveTarget([C("hub:u-1", "Salón")], new Set(["hub:u-1"]), [
    LAN("Beto", "Beto", true, true),
  ]);
  assert.equal(target.kind, "pick");
  if (target.kind === "pick") {
    assert.deepEqual(target.options, [
      { key: "hub:u-1", name: "Salón", online: true, route: "hub" },
      { key: "Beto", name: "Beto", online: true, route: "lan" },
    ]);
  }
});

test("resolveTarget: LAN device without stored pin is not a target", () => {
  assert.deepEqual(
    resolveTarget([], new Set(), [LAN("Beto", "Beto", true, false)]),
    { kind: "none", reason: "no-paired" },
  );
});

test("resolveTarget: offline LAN with pin still counts as a candidate", () => {
  assert.deepEqual(
    resolveTarget([], new Set(), [LAN("Beto", "Beto", false, true)]),
    { kind: "none", reason: "all-offline" },
  );
});

test("resolveTarget: single LAN target resolves with the lan route", () => {
  assert.deepEqual(
    resolveTarget([], new Set(), [LAN("Beto", "Beto", true, true)]),
    { kind: "single", key: "Beto", name: "Beto", route: "lan" },
  );
});

test("resolveTarget: blank LAN names are dropped like hub contacts", () => {
  assert.deepEqual(resolveTarget([], new Set(), [LAN("Beto", "  ", true, true)]), {
    kind: "none",
    reason: "no-paired",
  });
});

// ── LAN inbound clipboard: toast-only plan (entry comes from message-received)

test("planLanClipboardReceived: toast copy and never appends", () => {
  assert.deepEqual(planLanClipboardReceived({ from: "Beto", text: "hola" }), {
    toast: "Copiado de Beto",
    append: false,
  });
  assert.equal(planLanClipboardReceived({ from: "  ", text: "hola" })?.toast, "Copiado del portapapeles");
  assert.equal(planLanClipboardReceived(null), null);
  assert.equal(planLanClipboardReceived({ from: "Beto", text: "" }), null);
  assert.equal(planLanClipboardReceived({ from: 42, text: "hola" }), null);
});

// ── Resolve toast copy (honest, per none-reason) ────────────────────────────

test("planClipboardResolve: no-paired vs all-offline copies", () => {
  assert.equal(planClipboardResolve({ kind: "none", reason: "no-paired" }), RESOLVE_NO_PAIRED);
  assert.equal(planClipboardResolve({ kind: "none", reason: "all-offline" }), RESOLVE_ALL_OFFLINE);
  assert.equal(RESOLVE_ALL_OFFLINE, "Nadie vinculado está en línea");
  assert.equal(planClipboardResolve({ kind: "single", key: "hub:u-1", name: "S" }), null);
  assert.equal(planClipboardResolve({ kind: "pick", options: [] }), null);
});

// ── Send outcome copy (honest toast per Err) ────────────────────────────────

test("planClipboardSend: success names the target", () => {
  assert.deepEqual(planClipboardSend({ ok: true }, "Salón"), {
    kind: "sent",
    toast: "Enviado a Salón",
  });
});

test("planClipboardSend: invalid-text is the empty-or-long copy", () => {
  assert.deepEqual(planClipboardSend({ ok: false, error: "invalid-text" }, "Salón"), {
    kind: "failed",
    toast: CLIP_EMPTY_OR_LONG,
  });
  assert.equal(CLIP_EMPTY_OR_LONG, "Portapapeles vacío o demasiado largo");
});

test("planClipboardSend: pair/peer/hub variants are honest and distinct", () => {
  assert.equal(planClipboardSend({ ok: false, error: "pair-required" }, "S").toast, SEND_PAIR_REQUIRED);
  assert.equal(planClipboardSend({ ok: false, error: "peer-offline" }, "S").toast, SEND_PEER_OFFLINE);
  assert.equal(planClipboardSend({ ok: false, error: "hub-unavailable" }, "S").toast, SEND_HUB_UNAVAILABLE);
  const unknown = planClipboardSend({ ok: false, error: "raro" }, "S");
  assert.equal(unknown.kind, "failed");
  assert.ok(unknown.kind === "failed" && unknown.toast.length > 0);
  assert.notEqual(unknown.kind === "failed" ? unknown.toast : "", SEND_PEER_OFFLINE);
});

test("write failure copy is the honest tap-to-copy fallback", () => {
  assert.equal(WRITE_COPY_FAILED, "No se pudo copiar automáticamente — tocá el mensaje");
});

// ── Inbound hydrate plan: wire id, persist:false, toast copy ────────────────

test("planHubClipboardInbound parses the native payload into a hydrate plan", () => {
  const plan = planHubClipboardInbound({ key: "hub:u-1", name: "Salón", text: "hola", id: "w-1" });
  assert.ok(plan);
  assert.deepEqual(plan!.msg, { key: "hub:u-1", name: "Salón", text: "hola", id: "w-1" });
  assert.equal(plan!.persist, false);
  assert.equal(plan!.toastCopy, "Copiado de Salón");
});

test("planHubClipboardInbound: blank sender name degrades to a generic copy", () => {
  const plan = planHubClipboardInbound({ key: "hub:u-1", name: "  ", text: "hola", id: "w-1" });
  assert.ok(plan);
  assert.equal(plan!.toastCopy, "Copiado del portapapeles");
});

test("planHubClipboardInbound rejects malformed or out-of-bounds payloads", () => {
  assert.equal(planHubClipboardInbound(null), null);
  assert.equal(planHubClipboardInbound({ nonsense: true }), null);
  assert.equal(planHubClipboardInbound({ key: "hub:u-1", name: "n", text: "", id: "w-1" }), null);
  assert.equal(planHubClipboardInbound({ key: "hub:u-1", name: "n", text: "hola", id: "" }), null);
  assert.equal(
    planHubClipboardInbound({ key: "hub-session:c1", name: "n", text: "hola", id: "w" }),
    null,
  );
  assert.equal(
    planHubClipboardInbound({ key: "hub:u-1", name: "n", text: "x".repeat(64_001), id: "w" }),
    null,
  );
});
