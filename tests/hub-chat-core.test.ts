// Hub chat core tests (pure, DOM-free, run with: node --test tests/hub-chat-core.test.ts)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  HUB_PAIRED_TAG,
  HUB_SYSTEM_FILE_TOO_LARGE,
  HUB_SYSTEM_HUB_UNAVAILABLE,
  HUB_SYSTEM_PAIR_REQUIRED,
  HUB_SYSTEM_PEER_OFFLINE,
  buildHubRows,
  hubFileErrorNotice,
  hubSendOutcome,
  isContactKey,
  isGrantBacked,
  learnContactBindings,
  newlyPairedConns,
  parseHubMessage,
  planEntryCommit,
  planHubFileInbound,
  planHubInbound,
  routeSend,
} from "../src/lib/hub-chat-core.ts";
import { isHubKey } from "../src/lib/hub-presence.ts";
import type { HubPeer } from "../src/lib/hub-presence.ts";
import type { DeviceState } from "../src/types.ts";

const PEER = (over: Partial<HubPeer> = {}): HubPeer => ({
  conn_id: "c1",
  name: "Navegador de salón",
  kind: "web",
  caps: [],
  ...over,
});

const CONTACTS = { "hub:u-1": "Navegador de salón", "hub:u-2": "Tablet de Ana" };

// ── Key + routing decision (hub vs lan vs optimistic) ───────────────────────

test("isContactKey only accepts non-empty hub:<uuid> keys", () => {
  assert.equal(isContactKey("hub:abc-123"), true);
  assert.equal(isContactKey("hub:"), false);
  assert.equal(isContactKey("hub-session:c1"), false);
  assert.equal(isContactKey("demo:valeria"), false);
});

test("routeSend: contact key routes to hub even without a device row", () => {
  assert.deepEqual(routeSend("hub:u-1", undefined), { kind: "hub" });
});

test("routeSend: lan device with ip routes to lan", () => {
  const device: DeviceState = { key: "mac", name: "Mac", ip: "192.168.1.5", online: true };
  assert.deepEqual(routeSend("mac", device), { kind: "lan", device });
});

test("routeSend: device without ip is the optimistic (demo) lane", () => {
  const device: DeviceState = { key: "demo:valeria", name: "Valeria", online: true };
  assert.deepEqual(routeSend("demo:valeria", device), { kind: "optimistic", device });
});

test("routeSend: unknown key without device is refused", () => {
  assert.deepEqual(routeSend("ghost", undefined), { kind: "unknown" });
});

// ── Send state mapping for each hub Err ──────────────────────────────────────

test("hubSendOutcome: ack 'sent' maps to sent", () => {
  assert.deepEqual(hubSendOutcome({ ok: true, status: "sent" }), { kind: "sent" });
});

test("hubSendOutcome: pair-required fails with pairing guidance", () => {
  const out = hubSendOutcome({ ok: false, error: "pair-required" });
  assert.equal(out.kind, "failed");
  assert.equal(out.kind === "failed" && out.systemText, HUB_SYSTEM_PAIR_REQUIRED);
});

test("hubSendOutcome: peer-offline fails with the honest cause", () => {
  const out = hubSendOutcome({ ok: false, error: "peer-offline" });
  assert.equal(out.kind, "failed");
  assert.match(out.kind === "failed" ? out.systemText : "", /no está conectado/);
});

test("hubSendOutcome: hub-unavailable fails with the honest cause", () => {
  const out = hubSendOutcome({ ok: false, error: "hub-unavailable" });
  assert.equal(out.kind, "failed");
  assert.match(out.kind === "failed" ? out.systemText : "", /hub no está disponible/);
});

test("hubSendOutcome: unknown errors fail with a generic honest line", () => {
  const out = hubSendOutcome({ ok: false, error: "algo raro" });
  assert.equal(out.kind, "failed");
  assert.ok(out.kind === "failed" && out.systemText.length > 0);
  assert.notEqual(out.kind === "failed" ? out.systemText : "", HUB_SYSTEM_PEER_OFFLINE);
});

// ── Contacts merge: offline rows + live paired single row + presence rows ────

test("contacts become offline hub conversation rows named from contacts", () => {
  const { rows, pairedKeys } = buildHubRows({
    contacts: CONTACTS,
    peers: [],
    pairedConnIds: new Set(),
    contactForConn: new Map(),
  });
  assert.deepEqual(
    rows.map((r) => [r.key, r.name, r.online]),
    [
      ["hub:u-1", "Navegador de salón", false],
      ["hub:u-2", "Tablet de Ana", false],
    ],
  );
  assert.equal(pairedKeys.size, 0);
});

test("a live peer with a grant and a known binding merges into ONE online contact row", () => {
  const { rows, pairedKeys } = buildHubRows({
    contacts: CONTACTS,
    peers: [PEER()],
    pairedConnIds: new Set(["c1"]),
    contactForConn: new Map([["c1", "hub:u-1"]]),
  });
  const byKey = new Map(rows.map((r) => [r.key, r]));
  assert.equal(byKey.get("hub:u-1")?.online, true);
  assert.equal(byKey.get("hub:u-2")?.online, false);
  assert.equal(byKey.has("hub-session:c1"), false);
  assert.equal(rows.length, 2);
  assert.ok(pairedKeys.has("hub:u-1"));
});

test("a live peer WITHOUT a grant keeps its hub-session row, separate from contacts", () => {
  const { rows, pairedKeys } = buildHubRows({
    contacts: CONTACTS,
    peers: [PEER({ conn_id: "c9", name: "Firefox de estudio" })],
    pairedConnIds: new Set(),
    contactForConn: new Map(),
  });
  const sessionRows = rows.filter((r) => r.key === "hub-session:c9");
  assert.equal(sessionRows.length, 1);
  assert.equal(sessionRows[0].online, true);
  assert.equal(rows.filter((r) => r.key === "hub:u-1")[0].online, false);
  assert.ok(!pairedKeys.has("hub-session:c9"));
});

test("a paired peer with NO known binding stays a hub-session row but is tagged paired", () => {
  const { rows, pairedKeys } = buildHubRows({
    contacts: CONTACTS,
    peers: [PEER()],
    pairedConnIds: new Set(["c1"]),
    contactForConn: new Map(),
  });
  assert.equal(rows.filter((r) => r.key === "hub-session:c1").length, 1);
  assert.ok(pairedKeys.has("hub-session:c1"));
  assert.ok(!pairedKeys.has("hub:u-1"));
});

test("unpaired peers never steal a contact binding even if the map has their conn", () => {
  const { rows } = buildHubRows({
    contacts: CONTACTS,
    peers: [PEER({ conn_id: "c1" })],
    pairedConnIds: new Set(),
    contactForConn: new Map([["c1", "hub:u-1"]]),
  });
  assert.equal(rows.filter((r) => r.key === "hub:u-1")[0].online, false);
  assert.equal(rows.filter((r) => r.key === "hub-session:c1").length, 1);
});

test("contact keys are the history keys, so DeviceList previews resolve from history", () => {
  const { rows } = buildHubRows({
    contacts: CONTACTS,
    peers: [PEER()],
    pairedConnIds: new Set(["c1"]),
    contactForConn: new Map([["c1", "hub:u-1"]]),
  });
  assert.ok(rows.every((r) => isContactKey(r.key) || r.key.startsWith("hub-session:")));
});

// ── Binding learning: unique non-empty name match only (display grouping) ───

test("hub: and hub-session: prefixes are mutually exclusive for every routing gate", () => {
  // Regression guard for the E2E gating bug: neither prefix may swallow the
  // other (routing, selectedContact derivation and show-conv all depend on
  // these two gates agreeing).
  assert.equal(isContactKey("hub:u-1"), true);
  assert.equal(isContactKey("hub-session:c1"), false);
  assert.equal(isHubKey("hub-session:c1"), true);
  assert.equal(isHubKey("hub:u-1"), false);
});

test("newlyPairedConns reports only conns absent from the previous paired set", () => {
  assert.deepEqual(newlyPairedConns(new Set(), new Set(["c1"])), ["c1"]);
  assert.deepEqual(newlyPairedConns(new Set(["c1"]), new Set(["c1", "c2"])), ["c2"]);
  assert.deepEqual(newlyPairedConns(new Set(["c1"]), new Set(["c1"])), []);
  assert.deepEqual(newlyPairedConns(new Set(["c1"]), new Set()), []);
});

test("a freshly paired conn with no message yet is the reload signal for its contact row", () => {
  // E2E regression: right after pairing, the native side already minted the
  // contact, but the UI had no row for it — the ONLY row tagged "Vinculado"
  // was the hub-session row, and clicking it opened the presence-only detail.
  const fresh = newlyPairedConns(new Set(), new Set(["c1"]));
  assert.deepEqual(fresh, ["c1"]);
  const { rows, pairedKeys } = buildHubRows({
    contacts: { "hub:u-1": "Navegador de salón" },
    peers: [PEER()],
    pairedConnIds: new Set(["c1"]),
    contactForConn: new Map(),
  });
  assert.equal(rows.filter((r) => r.key === "hub-session:c1").length, 1);
  assert.ok(pairedKeys.has("hub-session:c1"));
});

test("learnContactBindings binds a conn to the uniquely named contact", () => {
  const map = learnContactBindings([PEER()], CONTACTS);
  assert.deepEqual([...map], [["c1", "hub:u-1"]]);
});

test("duplicate contact names never bind (names are not identity)", () => {
  const map = learnContactBindings([PEER()], {
    "hub:u-1": "Navegador de salón",
    "hub:u-2": "Navegador de salón",
  });
  assert.equal(map.size, 0);
});

test("empty or blank names never bind", () => {
  const map = learnContactBindings([PEER({ name: "  " })], { "hub:u-1": "" });
  assert.equal(map.size, 0);
});

// ── Inbound hydrate: adopt the wire id, never persist, learn unknown keys ────

test("parseHubMessage accepts the native payload shape", () => {
  assert.deepEqual(parseHubMessage({ key: "hub:u-1", name: "Salón", text: "hola", id: "w-1" }), {
    key: "hub:u-1",
    name: "Salón",
    text: "hola",
    id: "w-1",
  });
});

test("parseHubMessage rejects malformed payloads defensively", () => {
  assert.equal(parseHubMessage(null), null);
  assert.equal(parseHubMessage({ key: "hub:u-1", text: "hola", id: "w-1" }), null);
  assert.equal(parseHubMessage({ key: "hub:u-1", name: "n", text: "", id: "w-1" }), null);
  assert.equal(parseHubMessage({ key: "hub:u-1", name: "n", text: "hola", id: "" }), null);
  assert.equal(parseHubMessage({ key: "hub-session:c1", name: "n", text: "hola", id: "w" }), null);
});

test("planHubInbound hydrates with persist:false (row already committed natively)", () => {
  const plan = planHubInbound({ key: "hub:u-1", name: "Salón", text: "hola", id: "w-1" }, CONTACTS);
  assert.ok(plan);
  assert.equal(plan!.persist, false);
  assert.equal(plan!.msg.id, "w-1");
  assert.equal(plan!.upsertName, null);
});

test("planHubInbound learns the contact name when the key is unknown so far", () => {
  const plan = planHubInbound(
    { key: "hub:new", name: " recién vinculado ", text: "hola", id: "w-2" },
    CONTACTS,
  );
  assert.ok(plan);
  assert.equal(plan!.upsertName, "recién vinculado");
});

test("planHubInbound ignores malformed events (unknown/unpaired keys dropped)", () => {
  assert.equal(planHubInbound({ nonsense: true }, CONTACTS), null);
  assert.equal(planHubInbound({ key: "lan:x", name: "n", text: "hola", id: "w" }, CONTACTS), null);
});

// ── Persist flag + paired label ──────────────────────────────────────────────

test("planEntryCommit: persist defaults true and is only false when explicitly disabled", () => {
  assert.equal(planEntryCommit(), true);
  assert.equal(planEntryCommit({ persist: true }), true);
  assert.equal(planEntryCommit({ persist: false }), false);
});

test("the paired tag is the short honest label", () => {
  assert.equal(HUB_PAIRED_TAG, "Vinculado");
});

test("isGrantBacked is keyed membership on the paired set", () => {
  const keys = new Set(["hub:u-1", "hub-session:c1"]);
  assert.equal(isGrantBacked("hub:u-1", keys), true);
  assert.equal(isGrantBacked("hub-session:c1", keys), true);
  assert.equal(isGrantBacked("hub:u-2", keys), false);
});

// ── Minted-contact binding: the reload delta links a fresh conn to its key ──

test("bindMintedContact binds one fresh conn to the one new contact key", async () => {
  const { bindMintedContact } = await import("../src/lib/hub-chat-core.ts");
  const before = new Set(["hub:old-1"]);
  const after = { "hub:old-1": "Vieja", "hub:new-1": "e2e-chat-A" };
  assert.deepEqual(bindMintedContact(["c1"], before, after), ["c1", "hub:new-1"]);
});

test("bindMintedContact stays unbound when the reload adds more than one key", async () => {
  const { bindMintedContact } = await import("../src/lib/hub-chat-core.ts");
  const before = new Set<string>();
  const after = { "hub:a": "Uno", "hub:b": "Dos" };
  assert.equal(bindMintedContact(["c1"], before, after), null);
});

test("bindMintedContact stays unbound for multiple fresh conns or no new key", async () => {
  const { bindMintedContact } = await import("../src/lib/hub-chat-core.ts");
  const before = new Set<string>(["hub:a"]);
  const after = { "hub:a": "Vieja", "hub:b": "Nueva" };
  assert.equal(bindMintedContact(["c1", "c2"], before, after), null);
  assert.equal(bindMintedContact(["c1"], before, { "hub:a": "Vieja" }), null);
});

test("a learned minted binding merges a paired session into its contact row despite duplicate names", () => {
  // Regression for the e2e screenshot: two persisted contacts share the
  // browser name, so name-based binding fails; the minted delta binding must
  // still collapse the paired session into ONE conversation row.
  const contacts = { "hub:a": "e2e-chat-A", "hub:b": "e2e-chat-A" };
  const { rows, pairedKeys } = buildHubRows({
    contacts,
    peers: [PEER({ conn_id: "c1", name: "e2e-chat-A" })],
    pairedConnIds: new Set(["c1"]),
    contactForConn: new Map([["c1", "hub:b"]]),
  });
  assert.deepEqual(
    rows.map((r) => r.key).sort(),
    ["hub:a", "hub:b"],
    "no hub-session row may remain",
  );
  const byKey = new Map(rows.map((r) => [r.key, r]));
  assert.equal(byKey.get("hub:b")?.online, true);
  assert.ok(pairedKeys.has("hub:b"));
});

// ── Hub file sends (attach lane) ────────────────────────────────────────────

test("hubSendOutcome: file-too-large fails with the 25 MB copy", () => {
  assert.deepEqual(hubSendOutcome({ ok: false, error: "file-too-large" }), {
    kind: "failed",
    systemText: HUB_SYSTEM_FILE_TOO_LARGE,
  });
  assert.equal(
    HUB_SYSTEM_FILE_TOO_LARGE,
    "El archivo supera el máximo de 25 MB y no se envió.",
  );
});

test("routeSend drives the attach lane: hub contact vs lan device", () => {
  // The attach/drop flow uses the SAME router as text: hub:<uuid> goes to
  // hub_send_file, a device row with ip goes to the LAN send_file.
  assert.deepEqual(routeSend("hub:u-1", undefined), { kind: "hub" });
  const device: DeviceState = { key: "mac", name: "Mac", kind: "lan", online: true, ip: "192.168.1.5" };  assert.deepEqual(routeSend("mac", device), { kind: "lan", device });
});

test("planHubFileInbound hydrates with persist:false (row already committed natively)", () => {
  const plan = planHubFileInbound(
    { key: "hub:u-1", name: "foto.png", path: "/tmp/foto.png", size: 12, id: "w-f1" },
    CONTACTS,
  );
  assert.ok(plan);
  assert.deepEqual(plan.file, { key: "hub:u-1", name: "foto.png", path: "/tmp/foto.png", size: 12, id: "w-f1" });
  assert.equal(plan.persist, false);
  assert.equal(plan.upsertName, null);
});

test("planHubFileInbound learns the contact name when the key is unknown so far", () => {
  const plan = planHubFileInbound(
    { key: "hub:u-9", name: "doc.pdf", path: "/tmp/doc.pdf", size: 1, id: "w-f2" },
    CONTACTS,
  );
  assert.ok(plan);
  assert.equal(plan.upsertName, "doc.pdf");
  assert.equal(planHubFileInbound({ nonsense: true }, CONTACTS), null);
  assert.equal(
    planHubFileInbound({ key: "lan:x", name: "n", path: "/p", size: 1, id: "w" }, CONTACTS),
    null,
  );
});

test("hubFileErrorNotice builds the honest persisted system copy", () => {
  // The error notice is a system row appended by the UI (planEntryCommit
  // defaults true), so the copy must stand alone and stay honest.
  assert.equal(planEntryCommit(), true);
  assert.equal(
    hubFileErrorNotice("foto.png", "file-too-large"),
    "No se pudo recibir foto.png: el archivo supera el máximo de 25 MB",
  );
  assert.equal(
    hubFileErrorNotice("doc.pdf", "stage-failed"),
    "No se pudo recibir doc.pdf: no se pudo guardar el archivo",
  );
  assert.equal(
    hubFileErrorNotice("doc.pdf", "algo-raro"),
    "No se pudo recibir doc.pdf: error desconocido",
  );
});
