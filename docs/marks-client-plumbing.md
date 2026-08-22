# Marks client plumbing guide

How the marks client (maceip/marks) should wire the ESBT engine's production
surface — `web/esbt-document.js` over the `esbt_doc_*` Wasm ABI — so the
engine's operational guarantees actually hold in the product. The engine
deliberately does not schedule its own compaction, persistence, or transport;
this document specifies the loops the client must own, with the exact API
for each.

Boundary reminder (from the README): the engine receives only an ESBT site
identifier and operation bytes. Authority, actor-to-site binding, rooms,
persistence, and the WebSocket server are marks concerns.

## 1. Document creation and configuration

`EsbtDocument.create` now accepts a `config` object which is encoded
(`encodeDocumentConfig`, config format v1 — byte layout documented in
`src/config.rs`) and passed to `esbt_doc_create_configured`:

```js
const doc = await EsbtDocument.create({
  runtime,
  siteId,                       // marks binds actor → site before dispatch
  config: {
    // Allocator (all optional; defaults = paper evaluation defaults)
    dmax: 65536,
    base: 2147483647,
    depth: 256,
    strategy: { kind: "midpoint" },            // or boundary-low/-high,
                                               // alternating-by-depth + boundary
    adaptiveDmax: { floor: 16, ceiling: 2147483648, window: 256, holdoffWindows: 4 },
    // Per-document resource ceilings (browser tabs should be stricter
    // than the wire defaults; every field optional)
    limits: {
      maxDocumentUnits: 2_000_000,
      maxMessageBytes: 16 * 1024 * 1024,
      maxRetainedOperations: 4_000_000,
      // ... see LIMIT_FIELDS in web/esbt-document.js
    },
  },
});
```

Recommended production configuration for marks:

- **Leave `dmax`/`base`/`depth` at defaults.** The real-trace evaluation
  showed the fraction layer is never pressured at the default bound.
- **Enable `adaptiveDmax` with defaults.** It is convergence-neutral (each
  replica adapts independently), costs a few integer operations per
  allocation, and self-reverts if a probe regresses identifier cost. Poll
  `doc.currentDmax()` into telemetry if you want visibility.
- **Strategy: `midpoint`** for prose documents. Consider
  `boundary-low` only for documents with measured heavy middle-insertion
  churn (it cut journal bytes 36% on that adversarial pattern but deepens
  mean paths on prose; see `docs/extension-considerations.md`).
- **Set `limits.maxDocumentUnits`** to the product's real document ceiling
  rather than the 2M default, and lower `maxMessageBytes` if the transport
  frames are smaller. Violations fail typed (`DocumentTooLarge` = 15,
  `MessageTooLarge` = 7) before any state mutates.

Configuration is constructor-only. Changing policy means creating a new
document from a full snapshot (`exportFullSnapshot` → `applySnapshot` on the
new handle), which is cheap and preserves everything including pending
operations.

## 2. Memory: the compaction loop the client must own

Three engine structures grow with history, not with document size:

| Structure | Grows on | Reclaimed by |
|---|---|---|
| Retained journal (`retainedOperations`) | every local + remote op | `pruneHistoryThrough(version)` |
| Delete log | deletes whose insertion hasn't arrived | automatic once the insertion arrives |
| Pending queue (`pendingOperations`) | causally early ops | automatic once prerequisites arrive |

Only the retained journal is unbounded under normal operation. The engine
exposes the policy inputs and the knife; marks must supply the loop:

```js
// After the server acknowledges durable receipt of a causally closed
// prefix (an encoded Version it has journaled for every participant):
const pruned = doc.pruneHistoryThrough(serverAckedVersion);
```

**The safety rule:** prune only through a version the *sync authority* (the
marks room server) has durably journaled. After pruning, this document
answers reconnect requests below its `historyFloor()` with typed
`HistoryUnavailable` (21), and those peers must be served a snapshot — by
the server, which still has the journal. Pruning through a merely-local
version strands offline peers on the snapshot path unnecessarily; pruning
through a version the server does not have loses the only copy.

Suggested policy (all inputs already exposed):

1. On every server ack, if `doc.retainedOperations > N` (e.g. 50k) or the
   last prune was > T minutes ago, call `pruneHistoryThrough(ackedVersion)`.
2. After each prune, persist a fresh checkpoint:
   `idb.put(docId, doc.exportFullSnapshot())`. The full snapshot contains
   the materialized state, the retained journal above the floor, and the
   exact pending set — it is the complete crash-recovery artifact
   (verified byte-for-byte by `tests/adverse_network.rs`).
3. On startup, `applySnapshot(persistedBytes)` on a pristine document
   restores state, pending operations, and identity counters; then run the
   reconnect flow below. Never re-mint a site ID for a document that has a
   persisted snapshot — identity continuity comes from the snapshot.
4. Also checkpoint before `document.destroy()` and on `beforeunload`.

The pending queue and delete log need no policy, but surface
`pendingOperations` in diagnostics: a persistently nonzero value means a
peer's operations are not arriving (transport gap), and the reconnect flow
should run.

## 3. Reconnect and recovery flows

These flows are exactly the scenarios measured in
`tests/adverse_network.rs`; the client should implement them as stated
there.

**Normal reconnect (both sides retain history):**

```js
// exchange encoded versions, then:
const delta = doc.exportUpdate(remoteVersion);   // membership-exact, gap-aware
peerSend(delta);                                  // and apply theirs via doc.import()
```

The version summary is gap-aware (sparse receipts), so a hole like
"received seq 2, missing seq 1" is repaired with exactly the missing
operation. One round converges; a second round returns empty updates.

**Reconnect across a compaction horizon:** `exportUpdate` throws
`HistoryUnavailable` (21). The client must fall back:

```js
try {
  peerSend(doc.exportUpdate(remoteVersion));
} catch (e) {
  if (e.code === 21) peerSend(doc.exportCompactSnapshot());
  else throw e;
}
```

The receiving side calls `doc.import(bytes)` — snapshots and updates are
distinguished by envelope tag. A compact-snapshot rebase *preserves* the
receiver's unsynced local edits (they are retained journal and are replayed
over the new base); after the rebase the receiver's own edits still need to
flow back with `exportUpdate`, so always finish with one normal round.
Two refusals the client must surface rather than retry:
`MissingLocalHistory` (20: this replica compacted away edits the base
lacks — request a *newer* snapshot) and `SnapshotHasSequenceGaps` (18: the
offered base itself has holes — it is not a valid base; get one from a
caught-up peer).

**Crash recovery:** restore from the persisted full snapshot (§2), then run
normal reconnect. Post-restore local edits are guaranteed not to reuse
operation identities.

## 4. Transport framing: batch through transactions

Since engine format v3, an update payload carries one site dictionary and
front-codes identifier paths across its operations, so bytes-per-operation
falls sharply with batch size (a 400-op batch measures ~14 B/op vs ~28 B/op
encoded singly). The client controls batching with transactions:

```js
doc.transact(() => {
  for (const change of editorChanges) applyChange(doc, change);
});           // → exactly one canonical update, one journal row
```

- Wrap each editor change-set (CodeMirror transaction) in one
  `doc.transact`, never one update per keystroke.
- The `canonical_bytes` emitted on commit are retry-safe: send them as-is,
  resend on doubt; duplicates are idempotent. Journal them verbatim on the
  server — the bytes are the canonical record.
- Do not re-encode or split updates outside the engine; decode canonicality
  (site tables, exact front-coding, minimal varints) will reject foreign
  re-encodings by design.

## 5. Error codes the client must handle

| Code | Name | Client action |
|---|---|---|
| 3 | `AllocationExhausted` | The exact gap has no identifier. Refuse the keystroke, keep the document; occurs only in adversarial twin pinches. |
| 7 | `MessageTooLarge` | Split the batch (smaller transactions) or raise the limit at creation. |
| 9 | `IdentifierTooDeep` | Position is pathologically deep (adversarial middle insertion). Refuse the edit; suggest inserting adjacent; consider a fresh-document migration for hostile docs. |
| 15 | `DocumentTooLarge` | Product-level ceiling reached; surface to the user. |
| 18 / 20 / 21 | snapshot/history refusals | Drive the §3 fallbacks; never retry blindly. |
| 22 / 23 | transaction misuse | Client bug: unbalanced `begin/commit/abort`. |

Everything else (malformed, non-canonical, unsupported-version) indicates a
corrupt or foreign byte stream: drop the frame, log it, and resync via §3.

## 6. Telemetry worth shipping

- `retainedOperations` (compaction pressure), `pendingOperations`
  (transport health), `historyFloor()` length (snapshot-serving burden),
- `currentDmax()` (adaptive controller activity),
- sizes of exported updates/snapshots (regression canary for format
  changes; expected magnitudes are recorded in
  `docs/extension-considerations.md`).
