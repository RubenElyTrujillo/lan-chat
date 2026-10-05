// Persistence cutover core tests (pure, DOM-free, run with: node --test tests/history-core.test.ts)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  INITIAL_TOKEN,
  isStaleHistoryError,
  mergeLoadedHistory,
  planLegacyImport,
  tokensAfterDeleteAll,
  tokensFromRevs,
  withHistoryToken,
  type HistToken,
} from "../src/lib/history-core.ts";
import { makeHistoryApi } from "../src/lib/history.ts";
import type { Entry, History } from "../src/types.ts";

const entry = (id: string, at: number, over: Partial<Entry> = {}): Entry => ({
  id,
  mine: false,
  text: `t-${id}`,
  at,
  ...over,
});

const hist = (key: string, ...entries: Entry[]): History => ({ [key]: entries });

test("mergeLoadedHistory keeps local-only messages and DB wins equal ids", () => {
  const local = hist("k", entry("local-1", 1), entry("shared", 2, { text: "local-version" }));
  const db = hist("k", entry("db-only", 0), entry("shared", 3, { text: "db-version" }));
  const merged = mergeLoadedHistory(local, db);
  const ids = merged.k.map((e) => e.id);
  assert.ok(ids.includes("local-1"), "local-only entry survives the race");
  assert.ok(ids.includes("db-only"), "db-only entry present");
  assert.equal(ids.filter((i) => i === "shared").length, 1, "no duplicate ids");
  assert.equal(merged.k.find((e) => e.id === "shared")?.text, "db-version", "DB wins equal ids");
});

test("mergeLoadedHistory with empty DB keeps local intact", () => {
  const local = hist("k", entry("local-1", 1));
  assert.deepEqual(mergeLoadedHistory(local, {}), local);
});

test("withHistoryToken passes the token captured at call time", async () => {
  let seen: HistToken | null = null;
  const token: HistToken = { epoch: 2, rev: 5 };
  await withHistoryToken(token, async (t) => {
    seen = { ...t };
  }, async () => {
    throw new Error("refresh must not run on success");
  });
  assert.deepEqual(seen, token);
});

test("withHistoryToken reloads and retries exactly once on stale-history", async () => {
  const stale: HistToken = { epoch: 0, rev: 0 };
  const fresh: HistToken = { epoch: 0, rev: 1 };
  let attempts = 0;
  let refreshes = 0;
  const seen: HistToken[] = [];
  const result = await withHistoryToken(
    stale,
    async (t) => {
      attempts++;
      seen.push({ ...t });
      if (attempts === 1) throw new Error("stale-history");
      return `ok:${t.rev}`;
    },
    async () => {
      refreshes++;
      return fresh;
    },
  );
  assert.equal(result, "ok:1");
  assert.equal(attempts, 2, "one retry, no loop");
  assert.equal(refreshes, 1);
  assert.deepEqual(seen, [stale, fresh]);
});

test("withHistoryToken gives up honestly after a second stale-history", async () => {
  let attempts = 0;
  await assert.rejects(
    withHistoryToken(
      INITIAL_TOKEN,
      async () => {
        attempts++;
        throw new Error("stale-history");
      },
      async () => ({ epoch: 9, rev: 9 }),
    ),
    /stale-history/,
  );
  assert.equal(attempts, 2, "reload+retry once, then stop");
});

test("withHistoryToken does not retry non-stale errors", async () => {
  let attempts = 0;
  await assert.rejects(
    withHistoryToken(
      INITIAL_TOKEN,
      async () => {
        attempts++;
        throw new Error("db locked");
      },
      async () => {
        throw new Error("refresh must not run");
      },
    ),
    /db locked/,
  );
  assert.equal(attempts, 1);
});

test("isStaleHistoryError matches the backend sentinel", () => {
  assert.ok(isStaleHistoryError(new Error("stale-history")));
  assert.ok(isStaleHistoryError("stale-history"));
  assert.ok(!isStaleHistoryError(new Error("conflict")));
});

test("planLegacyImport imports once only when flag unset and snapshot exists", () => {
  let reads = 0;
  const read = () => {
    reads++;
    return hist("k", entry("l1", 1));
  };
  const whenFlagged = planLegacyImport(true, read);
  assert.equal(whenFlagged, null, "already imported: snapshot never even read");
  assert.equal(reads, 0);
  const plan = planLegacyImport(false, read);
  assert.ok(plan?.k.some((e) => e.id === "l1"), "unimported + snapshot present: import planned");
  const none = planLegacyImport(false, () => null);
  assert.equal(none, null, "no snapshot: nothing to import");
});

test("startup sequence load->import->removeItem runs once and honors the flag", async () => {
  // Pure storage stand-in: the frontend is the only side that touches
  // localStorage; the fake invoker stands in for the (fixed) backend.
  let storage: string | null = JSON.stringify(hist("k", entry("l1", 1)));
  let removes = 0;
  let imports = 0;
  let legacyImported = false;
  const readSnapshot = (): History | null =>
    storage === null ? null : (JSON.parse(storage) as History);
  const removeSnapshot = () => {
    removes++;
    storage = null;
  };
  const invoker = async (cmd: string) => {
    if (cmd === "history_load") return { history: {}, epoch: 0, revs: {}, legacyImported };
    if (cmd === "history_import_legacy") {
      imports++;
      legacyImported = true;
      return true;
    }
    return null;
  };
  const api = makeHistoryApi(invoker);
  const startup = async () => {
    const stored = await api.load();
    const plan = planLegacyImport(stored.legacyImported, readSnapshot);
    if (plan) {
      const imported = await api.importLegacy(plan);
      if (imported) removeSnapshot();
    }
  };

  await startup(); // first launch: import then remove exactly once
  assert.equal(imports, 1, "snapshot imported once");
  assert.equal(removes, 1, "snapshot removed once, after a successful import");
  assert.equal(legacyImported, true, "backend flag set by the import, not by migration");

  await startup(); // second launch: flag true -> no import, no remove
  assert.equal(imports, 1, "no second import");
  assert.equal(removes, 1, "no second remove");
});

test("tokensFromRevs and tokensAfterDeleteAll keep the token map honest", () => {
  const revs = { a: 3, b: 0 };
  const tokens = tokensFromRevs(revs, 4);
  assert.deepEqual(tokens.a, { epoch: 4, rev: 3 });
  assert.deepEqual(tokens.b, { epoch: 4, rev: 0 });
  const bumped = tokensAfterDeleteAll(tokens, 5);
  assert.deepEqual(bumped.a, { epoch: 5, rev: 3 }, "delete_all keeps per-key revs, bumps epoch");
});

test("makeHistoryApi maps commands and arguments exactly", async () => {
  const calls: Array<{ cmd: string; args: Record<string, unknown> | undefined }> = [];
  const invoker = async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    if (cmd === "history_load") {
      return {
        history: { k: [entry("e1", 1, { mine: true, filePath: "/f.bin", read: true })] },
        epoch: 7,
        revs: { k: 2 },
        legacyImported: true,
      };
    }
    if (cmd === "history_delete_conversation") return { epoch: 7, rev: 3 };
    if (cmd === "history_delete_all") return 8;
    if (cmd === "history_patch_state") return true;
    if (cmd === "history_import_legacy") return true;
    return null;
  };
  const api = makeHistoryApi(invoker);
  const loaded = await api.load();
  assert.equal(calls[0].cmd, "history_load");
  assert.equal(loaded.epoch, 7);
  assert.equal(loaded.legacyImported, true);
  assert.equal(loaded.history.k[0].filePath, "/f.bin", "camelCase mapping survives the boundary");

  const token: HistToken = { epoch: 7, rev: 2 };
  await api.append("k", "e1", entry("e1", 1), token);
  assert.equal(calls[1].cmd, "history_append");
  assert.deepEqual(calls[1].args, { key: "k", id: "e1", entry: entry("e1", 1), token });

  await api.patchState("k", "e1", "delivered", false, token);
  assert.deepEqual(calls[2].args, {
    key: "k",
    id: "e1",
    newState: "delivered",
    read: false,
    token,
  });

  await api.patchState("k", "e1", undefined, true, token);
  assert.equal(calls[3].args?.newState, null, "undefined state is sent as null, not dropped");

  const fresh = await api.deleteConversation("k");
  assert.deepEqual(fresh, { epoch: 7, rev: 3 });
  assert.deepEqual(await api.deleteAll(), 8);
  await api.importLegacy(hist("k", entry("l1", 1)));
  assert.equal(calls[6].cmd, "history_import_legacy");
});
