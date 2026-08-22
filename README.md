# ESBT

Sequence CRDT from Mechaoui & Imine, [arXiv:2607.28101](https://arxiv.org/abs/2607.28101).

Rust core, `wasm32-unknown-unknown`, and a production one-document browser
adapter. The Rust/Wasm implementation is the sole production engine. The
private package in [`ts/`](ts/) is frozen as a behavioral reference only; Marks
must not import or ship it as an independent engine.

`extensions.md` is **not** in this tree (adaptive \(D_{\max}\), compact `sc`,
Yjs/Automerge, partition-recovery study).

## Repository boundary

This repository owns collaboration-engine concerns only:

- ESBT weights, operations, ordering, and allocator;
- gap-aware version summaries;
- UTF-16 document semantics;
- operation and snapshot encoding;
- exact, bounded, non-panicking decoding;
- intention-preserving concurrent insertion behavior;
- snapshot merge and compaction semantics;
- native Rust API;
- browser Wasm API;
- local-operation and undo primitives needed by the Wasm adapter;
- native/Wasm golden fixtures;
- convergence, permutation, malformed-input, and performance tests; and
- reproducible Wasm packaging and engine CI.

It does not own principals, sessions, devices as product identities, phone
controllers, scratch workspaces as product records, ACLs or roles, share links,
room tickets, comments, assets, HTTP cookies, databases, or the production
WebSocket server.

The ESBT core receives only an ESBT site identifier and operation bytes. It
does not know who the person is or whether they are allowed to edit. The
product boundary validates authority and actor-to-site binding before dispatch;
ESBT still independently performs exact bounded structural decoding and
validates its own operation invariants.

## Paper claims → code

| Claim | Where |
|---|---|
| Weight ⟨f, sn, sc, δ⟩, sentinels 0/1 and 1/0 | `src/weight.rs` |
| Total order Def. 2 | `Weight::cmp` |
| NEWSEQ Alg. 1 + examples | `src/newseq.rs` |
| CREATE_WEIGHT Alg. 2, Tracker Def. 4 | `src/allocator.rs` |
| Situations 1–3 | `src/verify.rs` |
| Bounded fractions (Thm. 2: max(p,q) < Dmax) | `Allocator::mediant_fits` |
| RB-tree document, rank by index | `src/rbtree.rs` |
| INS(ω,e,c) / DEL(ω,c) | `src/op.rs` |
| Alg. 3 Q, L, CounterMap, isCausallyReady | `src/replica.rs` |
| Scenario 3 reuse distinguished by c | same |
| Epidemic + join/leave | `src/snapshot.rs` + `web/mesh.js` |
| Evaluation defaults base=2³¹−1, depth=256 | `ReplicaConfig` |

## Marks qualification changes

The Rust core is the intended production authority for marks. It deliberately
includes the following corrections and product-level extensions beyond a
literal transcription of the preprint:

- version summaries retain a contiguous per-site prefix plus explicit higher
  receipts, so reconnect can repair out-of-order sequence gaps;
- every allocator candidate is checked strictly between its immediate
  neighbors, with an unbounded NEWSEQ retry and typed exhaustion when that
  exact gap has no representable identifier;
- consecutive local insertions reserve a site-specific ESBT path prefix, so
  concurrent typing runs remain contiguous rather than converging to a
  character shuffle;
- reused weights order their insertion counters: a newer reuse waits behind an
  older live occupant, while an older reuse arriving late is suppressed;
- document elements are UTF-16 code units, so Rust/Wasm indices exactly match
  JavaScript strings and CodeMirror positions, including non-BMP emoji;
- a causally closed compact snapshot can become a new base while retained
  local operations are replayed; the merge fails explicitly if the snapshot
  has gaps or required local history has already been compacted;
- persisted snapshots and mesh messages carry an explicit engine-format
  version and exact checked decoding rejects malformed, trailing, or
  non-canonical bytes.

These changes preserve ESBT weights, ordering, operations, delete counters,
snapshots, and replica merge semantics. The typing-run rule is an
intention-preservation extension; it is not a claim of Fugue-style maximal
non-interleaving. There is no legacy decoder because marks has no released
clients or durable documents requiring compatibility.

The `esbt_doc_*` ABI and [`web/esbt-document.js`](web/esbt-document.js) provide
the production one-document boundary: opaque lifecycle, exact UTF-16 edits,
transaction updates, atomic imports, snapshots, reconnect deltas, anchors,
per-replica undo/redo, typed failures, and explicit Wasm allocation ownership.
Marks owns persistence and transport around the canonical update bytes emitted
by that boundary; it does not reimplement the CRDT in TypeScript.

Algorithm 2 line 10 is typeset `p < Dmax **or** q < Dmax`. That admits
unbounded denominators and contradicts line 9, Theorem 2, and Situation 1
(3/7 rejected at Dmax=5). The implementation follows the theorem.

The paper assumes *reliable* epidemic broadcast and never names QUIC or
WebRTC. The page supplies that layer: BroadcastChannel, WebRTC data
channels, hello/snapshot anti-entropy.

## Run

```bash
cargo test --lib
# if the volume is noexec:
cp target/debug/deps/esbt-* /tmp/esbt-test && /tmp/esbt-test

# Default artifact: production `esbt_doc_*` API only.
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/esbt.wasm web/
python3 -m http.server -d web 8080
```

Open `http://localhost:8080/?room=demo` in two browsers. Same-origin tabs
sync by themselves. Distinct machines: offer / accept / apply SDP.
