<img width="312" src="https://github.com/user-attachments/assets/f7b8dd53-a616-4f5b-922a-d9e2d8622edd" />

# esbt

a sequence crdt for collaborative text editing, built from the extended
stern–brocot tree paper by mechaoui & imine
([arxiv:2607.28101](https://arxiv.org/abs/2607.28101)). concurrent changes
made on different devices merge automatically, without requiring any central
server, and every replica converges to the same document.

the engine is written in rust and compiles to webassembly for the browser.
it is network-agnostic: updates and snapshots are plain bytes you can send
over any transport and apply in any order. deleted text is removed for real —
no tombstones — and identifier growth stays bounded even under heavy
concurrent editing.

## what's in the box

- `src/` — the rust core: bounded identifier allocation, a red–black tree
  document, causal delivery, undo, snapshots, and exact, non-panicking
  decoding of every byte that crosses the wire
- `web/esbt-document.js` — the browser adapter over the `esbt_doc_*` wasm
  abi: transactions, anchors, undo/redo, reconnect deltas, and configurable
  documents
- `tests/` — deterministic convergence, adverse-network, recovery, codec,
  and identifier-size evidence against the production Rust engine
- `docs/` — design notes: the paper's four future-work extensions
  (implemented here, with measurements) and the client integration guide

## how it works

every inserted character gets a unique, immutable identifier — a bounded
rational fraction plus small disambiguation layers — so concurrent inserts
at the same position resolve deterministically on every replica. local
changes apply immediately; remote changes merge when they arrive. a
gap-aware version summary tells peers exactly which operations are missing,
so reconnecting after going offline exchanges only the difference. when
history has been compacted away, a compact snapshot becomes the new base and
your unsynced local edits are replayed on top, not lost.

the engine owns only the crdt: identifiers, ordering, merging, and encoding.
who is allowed to edit, where bytes are stored, and how they travel are your
application's concerns.

## quickstart

```bash
cargo test

cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/esbt.wasm web/
python3 -m http.server -d web 8080
```

open `http://localhost:8080/?room=demo` in two browsers and type. same-origin
tabs sync by themselves; distinct machines exchange webrtc offers.

## learn more

- [`docs/extension-considerations.md`](docs/extension-considerations.md) —
  adaptive allocation bounds, the compact wire formats, pluggable allocation
  strategies and order-preserving keys, and the deterministic
  partition/recovery study, each with recorded measurements
- [`docs/marks-client-plumbing.md`](docs/marks-client-plumbing.md) — how a
  client should configure documents, bound memory with history compaction,
  batch changes into transactions, and recover from disconnection or crash
- the implementation follows the paper's formal statements where the typeset
  pseudocode contradicts them, and documents every deliberate deviation
  (typing-run contiguity, gap-aware receipts, utf-16 document indices) in
  the files above
