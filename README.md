<img width="312" src="https://github.com/user-attachments/assets/f7b8dd53-a616-4f5b-922a-d9e2d8622edd" />

# esbt

A sequence CRDT for collaborative text editing, built from the extended
Stern–Brocot tree paper by Mechaoui and Imine
([arXiv:2607.28101](https://arxiv.org/abs/2607.28101)). Concurrent changes
made on different replicas merge without a central ordering authority, and
every replica that integrates the same operations converges to the same
document.

The engine is Rust. Native hosts use the Rust API; JavaScript hosts use the
versioned WIT component in [`wit/esbt.wit`](wit/esbt.wit). Jco turns that
component into a generated JavaScript binding plus ordinary core Wasm modules
for Chromium, Firefox, and WebKit/Safari. Browsers do not need native WIT or
component-model support.

## What is in the box

- `src/` — bounded-rational allocation, red–black tree document state,
  causal delivery, transactions, undo/redo, snapshots, anchors, and exact
  resource-bounded decoding.
- `wit/esbt.wit` — the only Wasm/JavaScript host ABI. Configuration, errors,
  receipts, and visible edits are typed WIT values.
- `src/wire.rs` — the only byte protocol: one `ESBT` envelope for six durable
  CRDT artifacts (Update, CompactSnapshot, FullSnapshot, Version, Anchor, and
  CausalPosition).
- `web/esbt-document.js` — a high-level browser owner over Jco's generated
  component binding.
- `tests/` — deterministic convergence, adverse-network recovery, malformed
  input, golden codec, and identifier-size evidence against the production
  Rust engine.
- `src/bin/esbt-inspect.rs` — a bounded JSON inspector for artifacts.

There is deliberately no legacy raw `esbt_doc_*` ABI, split-envelope decoder,
Protobuf layer, or Cap'n Proto/Cap'n Web layer to coordinate.

## Identifier and resource boundary

Each inserted UTF-16 unit receives a unique immutable weight: a reduced
positive rational plus sequence-number, sequence-path, and site tie-breakers.
The allocator statically bounds newly minted rational numerator and
denominator components with `Dmax`; configuration above the `2^31` hard
ceiling is rejected.

The complete identifier is not claimed to have a constant bound under every
adversarial history: a pathological repeated pinch can lengthen its sequence
path. Each document therefore carries an explicit `maxIdentifierDepth` and
fails the edit atomically with typed `IdentifierTooDeep` rather than growing
without policy. Message bytes, operation counts, sparse receipts, snapshots,
pending queues, document units, retained history, and undo history have the
same explicit resource treatment.

## How synchronization works

Local edits apply immediately and emit a retry-safe Update artifact. A
gap-aware Version artifact tells a peer exactly which operations are missing,
including holes above a contiguous prefix. On reconnect, peers exchange
versions and deltas. If requested history is below a compaction floor, the
sender returns typed `HistoryUnavailable` and sends one canonical snapshot
instead. Applying the same valid artifact again is idempotent.

The engine owns CRDT identifiers, ordering, merging, and artifact encoding.
Authentication, actor-to-site admission, storage, compaction scheduling, and
transport remain application responsibilities. A site identifies one live
operation generator; two live generators must never mint counters under the
same site.

## Build and verify

```bash
cargo test --locked
npm ci
npm run test:component
```

`test:component` builds the Rust guest, wraps it as a component, checks the
actual WIT export and lifted JavaScript value types, parses every generated
core module, exercises the high-level API, runs the mesh anti-entropy tests,
and crosses the former one-million-unit visible-edit boundary.

To run the demo:

```bash
npm run build:component
npm run demo
```

Open `http://127.0.0.1:8080/?room=demo` in two browsers. Same-origin tabs use
BroadcastChannel; distinct browser processes can use the WebSocket relay or
the manual WebRTC offer flow. The demo exchanges Version artifacts, sends
exact deltas, falls back to a snapshot across a history floor, reconnects its
relay with bounded exponential backoff, and uses exact bounded artifact
deduplication rather than a correctness-affecting short hash.

## Contracts and integration

- [`docs/esbt-codec.md`](docs/esbt-codec.md) — complete WIT-versus-codec
  boundary, byte layout, canonicality rules, browser execution path, golden
  vectors, and inspector.
- [`docs/extension-considerations.md`](docs/extension-considerations.md) —
  the four paper extensions, evidence, tradeoffs, and the deliberately honest
  scope of the sortable-key experiment.
- [`docs/marks-client-plumbing.md`](docs/marks-client-plumbing.md) — typed WIT
  client configuration, one-live-generator site rule, persistence,
  compaction, and reconnect flows used by Marks.

Where the paper's typeset pseudocode conflicts with its formal statements,
the implementation follows the formal definitions and pins the choice in
tests and documentation.
