# `@marks/esbt` — the TypeScript ESBT engine

The ESBT sequence CRDT (Mechaoui & Imine, [arXiv:2607.28101](https://arxiv.org/abs/2607.28101))
as a pure-TypeScript package implementing the editor contract in
[`marks/docs/ESBT-INTEGRATION.md`](https://github.com/maceip/marks/blob/main/docs/ESBT-INTEGRATION.md).
No WASM, synchronous mutations, UTF-16 indices, main thread in the browser and
in Node — exactly what the contract demands. The Rust/wasm reference lives in
this repository's `src/`; this package is its editor-facing sibling and the
one the [marks](https://github.com/maceip/marks) editor binds to.

```ts
import { EsbtDoc, EphemeralStore, UndoManager, VersionVector } from '@marks/esbt';

const doc = new EsbtDoc();
const undo = new UndoManager(doc, { mergeIntervalMs: 500 });
const presence = new EphemeralStore(30_000);

doc.subscribe((event) => {
  if (event.origin !== 'editor') reconcileEditor(event.text);
});
doc.subscribeLocalUpdates((bytes) => socket.send(frame(MSG_UPDATE, bytes)));

doc.transact(() => {
  doc.delete(from, to - from);
  doc.insert(from, inserted);
}, 'editor');

const snapshot = doc.export({ mode: 'snapshot' });
const shallow = doc.export({ mode: 'shallow-snapshot' });
const delta = doc.export({ mode: 'update', from: VersionVector.decode(peerVv) });
doc.import(payload); // merges; never clobbers newer local ops
```

## Layout

Exactly the file split the integration document suggests:

```
src/weight.ts     Fraction / Weight / total order (Def. 2), NEWSEQ (Alg. 1),
                  CREATE_WEIGHT + Tracker (Alg. 2, Def. 4)
src/tree.ts       order-statistic red-black tree of (weight, unit, counter)
src/ops.ts        INS/DEL, pending queue, delete log, site table, op codec
src/codec.ts      varint writer/reader, payload tags, version map codec
src/encode.ts     snapshot / shallow-snapshot / update payloads
src/vector.ts     VersionVector (site → max seq)
src/doc.ts        EsbtDoc — index API, transact, import/export, subscriptions,
                  anchors, undo hooks
src/undo.ts       UndoManager (per-peer, emits new ops, optional merge window)
src/ephemeral.ts  EphemeralStore (TTL presence, tombstones, heartbeat-friendly)
src/api.ts        the contract types
src/constructors.ts  compile-time check that the exports satisfy the contract
src/contract.test.ts every invariant the contract says marks will test
```

## Building and testing

```bash
npm install
npm run build        # tsc → dist/ (ESM + d.ts)
npm test             # build + node --test dist/contract.test.js
npm run typecheck
```

36 tests cover: the paper's Situations 1–3 and NEWSEQ examples, SEC under
shuffled delivery (fuzzed, three sites), delete-before-insert buffering,
weight reuse with fresh counters, offline delta reconnect, snapshot and
shallow-snapshot merge semantics, idempotent import, corrupt-payload rejection,
version-vector size, per-peer undo (including remote-safe skip and merge
grouping), presence TTL and tombstones, surrogate-pair transport, anchors, and
degenerate-gap (twin) insertion.

## Contract coverage and deviations

See [`COVERAGE.md`](./COVERAGE.md) for the full audit of the loro/yjs surface
marks used against this contract, the three additions the audit forced
(`EphemeralStore.keys()`, `UndoManagerOptions.mergeIntervalMs`,
`indexToAnchor`/`anchorToIndex`), and four documented corrections to the
reference algorithm (strict mediant separation, neighbour-aware sn ladder,
uncapped NEWSEQ fallback, twin-pinch widening).

## Shipping into marks

The marks repository consumes this package as the `esbt` npm workspace. The
complete integration — this package dropped in, both old engines removed,
client/server/benchmark rewired — is exported as a `git am`-ready patch series
in [`../patches/marks/`](../patches/marks/).
