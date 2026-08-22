# Interface coverage: what marks called on Loro/Yjs vs. the ESBT contract

The question this document answers: does the contract in
[`marks/docs/ESBT-INTEGRATION.md`](https://github.com/maceip/marks/blob/main/docs/ESBT-INTEGRATION.md)
cover **at least the surface marks actually used** from `loro-crdt`,
`loro-codemirror`, `yjs`, `y-codemirror.next`, `y-indexeddb`, and Hocuspocus —
and where it did not, what was designed and implemented to close the gap.

Every row was taken from a call site in marks (`client/src/collab/loro-engine.ts`,
`client/src/collab/yjs-engine.ts`, `server/src/loro-room.ts`,
`server/src/yjs-room.ts`, `server/src/api.ts`, `server/src/seed.ts`,
`client/src/workers/bench.worker.ts`), not from the libraries' documentation.

## Document surface

| marks called (loro / yjs) | Contract equivalent | Status |
| --- | --- | --- |
| `new LoroDoc()` / `new Y.Doc()` | `new EsbtDoc(config?)` | covered |
| `doc.getText('markdown').toString()` / `Y.Text.toString()` | `doc.getText()` (single implicit text) | covered |
| `text.insert(i, s)` / `text.delete(i, n)` | `doc.insert` / `doc.delete` | covered |
| `doc.commit({ origin })` / `Y.Doc.transact(fn)` | `doc.transact(fn, origin)` | covered |
| `doc.subscribe(e => e.origin …)` / `text.observe` | `doc.subscribe((EsbtEvent) => …)` | covered |
| `doc.subscribeLocalUpdates(bytes => …)` / `doc.on('update')` | `doc.subscribeLocalUpdates` | covered |
| `doc.import(bytes)` / `Y.applyUpdate` | `doc.import` (merging, idempotent, tagged) | covered |
| `doc.export({ mode: 'snapshot' })` / `Y.encodeStateAsUpdate` | `doc.export({ mode: 'snapshot' })` | covered |
| `doc.export({ mode: 'shallow-snapshot', frontiers })` | `doc.export({ mode: 'shallow-snapshot' })` (no frontiers argument needed) | covered |
| `doc.export({ mode: 'update', from: vv })` / `encodeStateAsUpdate(doc, sv)` | `doc.export({ mode: 'update', from })` | covered |
| `doc.oplogVersion()` / `Y.encodeStateVector` | `doc.oplogVersion()` | covered |
| `VersionVector.decode(bytes)` | `VersionVector.decode` | covered |
| `doc.peerIdStr` / `awareness.clientID` | `doc.siteId` | covered |

## Undo

| marks called | Contract | Status |
| --- | --- | --- |
| `new UndoManager(doc, {})` (loro) / `new Y.UndoManager(text)` | `new UndoManager(doc)` | covered |
| `canUndo` / `canRedo` / `undo` / `redo` / `destroy` | same | covered |
| **Keystroke grouping** — Loro's merge interval and Yjs's `captureTimeout` group a typing burst into one undo step | *not in the original contract* | **gap → closed.** `UndoManagerOptions.mergeIntervalMs` added (default 0 = strict one-transact-one-step; marks passes 500). Without it, Mod-Z undoes one character at a time and the smoke test's "undo reverts my own edit" loops out. |
| `new UndoManager(doc, { excludeOriginPrefixes: [COMMENT_ORIGIN] })` — comment writes must never be undoable | *not in the original contract* | **gap → closed.** `UndoManagerOptions.excludeOriginPrefixes` added with Loro's semantics, and map ops are additionally skipped inside undo application. |

## Comments (the browser-surface feature)

| marks called | Contract | Status |
| --- | --- | --- |
| `doc.getMap(COMMENTS_MAP).set(id, json)` / `.delete(id)` / `.entries()` / `.toJSON()` (loro) — `Y.Map` equivalent — comment records ride the document CRDT | *not in the original contract* (it only sketched a server-side table) | **gap → closed.** `EsbtDoc.mapSet` / `mapDelete` / `mapGet` / `mapEntries`: a keyed last-writer-wins register map riding the same oplog, snapshots (both flavours — cold opens must paint comments), version vectors, and update deltas as the text. Values are opaque strings; highest (lamport, site) wins; deletes leave mergeable tombstones. |
| `text.getCursor(pos)` → `Cursor.encode()`, `doc.getCursorPos(Cursor.decode(bytes))` — stable comment anchors across edits | anchors were already this audit's addition | **covered.** `indexToAnchor(pos)` → `EsbtAnchor` (JSON-encode for the comment record's cursor strings), `anchorToIndex` to re-resolve. Deleted anchors resolve to their weight's lower bound, mirroring Loro cursor recovery. |

## Presence

| marks called | Contract | Status |
| --- | --- | --- |
| `new EphemeralStore(30_000)` / Hocuspocus awareness | `new EphemeralStore(ttlMs)` | covered |
| `set` / `get` / `delete` / `getAllStates` / `encodeAll` / `apply` / `subscribe` / `subscribeLocalUpdates` / `destroy` | same | covered |
| `ephemeral.keys()` — the server room gates its first presence frame on `keys().length > 0` (`loro-room.ts:138`) | *not in the original contract* | **gap → closed.** `keys(): string[]` added to `EphemeralStore`. |
| `LoroEphemeralPlugin(doc, ephemeral, userInfo, getText)` / `yCollab` cursor layer — publishes the local selection and draws remote carets in CodeMirror | Deliberately out of scope for the crate ("CodeMirror two-way sync and remote cursor decorations" are marks'). | **gap → closed on the marks side.** `client/src/collab/presence.ts` (in the marks patch series) publishes `${siteId}-cm-user` / `${siteId}-cm-sel` with a 15 s heartbeat (§7's y-protocols guidance) and renders remote carets and selections as CodeMirror decorations. The store above is the transport. |

## Server-side usage

| marks called | Contract | Status |
| --- | --- | --- |
| Room replica: import stored snapshot, `oplogVersion().encode()` → `MSG_SERVER_VV`, delta on join, snapshot / shallow on HTTP | all `EsbtDoc` methods above | covered |
| `seed.ts`: build a doc, insert, export snapshot | `setText` + `export` | covered |
| `api.ts` export path: fresh doc + `import(state)` + `getText()` | same | covered |
| **Stable server site id** (§6: "server replica must use a stable siteId per document … not `random()` on every boot") | `EsbtConfig.siteId` existed, but nothing guaranteed the *generators* survive rehydrate | **gap → closed.** Snapshots carry per-site insertion counters and the version vector; importing a snapshot that knows your `siteId` resumes `seq` and `c` above everything recorded, so a restarted server never reissues an `(site, seq)` or reuses a counter. Covered by the "rehydrated server site resumes its counters" contract test. |

## Benchmark worker

`bench.worker.ts` used only surface already listed (construct, insert/delete,
local-update subscription, import, snapshot/update export, version vectors),
so the ESBT benchmark port needs nothing beyond the contract.

## Anchors (requested, not yet consumed)

§7 of the integration document asks the crate for `indexToAnchor` /
`anchorToIndex` "as soon as comments exist", because index-based presence
cannot anchor a comment across concurrent edits. Implemented now —
`EsbtAnchor { weight, offset }`, with deleted anchors resolving to the lower
bound of their weight so ranges collapse instead of drifting. Cursors keep
using UTF-16 indices (v1 presence), as the document specifies.

## Corrections the implementation makes to the reference algorithm

Found by fuzzing convergence while porting `src/*.rs`; each is marked in the
code:

1. **The mediant must strictly separate.** `CREATE_WEIGHT` between two
   weights with equal fractions (an sn-ladder pair, or concurrent same-gap
   inserts) computes `mediant(f, f) = f`, which "fits" and lands on an
   arbitrary side of the neighbours — or reproduces one of them exactly. The
   Rust reference returns the mediant there; its Situation 3 test passes only
   because site 2 happens to sort above site 1. This port routes the case to
   the sn / sequence layers, which is what the paper's Situation 3 describes.
2. **The sn ladder consults the neighbours, not only the Tracker.** The
   Tracker resets on rehydrate and never saw weights minted by other sites,
   so `snR + 1` alone can collide with or overshoot a live neighbour. The
   ladder value is now derived from the tracker *and* the actual neighbour
   weights, and every candidate is verified strictly between them.
3. **NEWSEQ exhaustion must not recycle paths.** Past `depth`, Algorithm 1
   appends a per-site constant tie digit, so a site can only ever mint one
   path under a saturated prefix — the second identical insert silently
   collides. After the capped walk fails, this port retries without the cap
   (terminates within `max(|scL|, |scR|) + 1` levels and is strictly between
   whenever the neighbour paths differ at all).
4. **Twin pinch.** Two sites inserting into the same gap concurrently mint
   weights that may differ only by site; the weight order has no room between
   such twins for a third site. Fresh weights therefore carry a site-flavoured
   first path digit (twins now need a 32-bit hash collision on top of the
   race), and if a pinch still occurs the document widens the gap rightward —
   the character lands next to the contested pair instead of being dropped,
   and every replica converges on the same order.

## Size and speed, honestly

Same 25 000-op trace the marks benchmark page generates (Node 22, one run,
this machine):

| | ESBT (this engine) | Loro (marks README) | Yjs (marks README) |
| --- | --- | --- | --- |
| Type the trace | 311 ms | 157 ms | 83 ms |
| Receive updates | 194 ms | 140 ms | 35 ms |
| Merge two branches | 47 ms | 22.5 ms | 5.1 ms |
| Open from snapshot | 236 ms | 2.0 ms | 2.8 ms |
| Snapshot size | 1.6 MB | 18.8 KB | 27.3 KB |
| Update traffic | 1.4 MB | 456 KB | 128 KB |

Per keystroke that is ~12 µs — invisible at human typing speed, which is the
editor's requirement. The snapshot format is item-per-code-unit with explicit
weights (fractions dominate; `sc` paths are already prefix-delta-coded), so
stored size trails the mature engines by orders of magnitude. That matches the
paper's own future-work list ("compact encoding and serialization techniques
for the sequence path"); run-length item coalescing and columnar encoding are
the known next steps and fit behind `export`/`import` without contract
changes. HTTP payloads additionally gzip well (the marks server compresses
responses).
