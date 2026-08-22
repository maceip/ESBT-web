# ESBT

Sequence CRDT from Mechaoui & Imine, [arXiv:2607.28101](https://arxiv.org/abs/2607.28101).

Rust core, `wasm32-unknown-unknown`, one-page editor — plus a pure-TypeScript
engine in [`ts/`](ts/) implementing the [marks](https://github.com/maceip/marks)
editor contract (`@marks/esbt`: doc + sync + merge, per-peer undo, presence,
weight anchors; no WASM). Its coverage audit against the Loro/Yjs surface marks
used is [`ts/COVERAGE.md`](ts/COVERAGE.md), and the ready-to-apply integration
that removes Loro and Yjs from marks is the patch series in
[`patches/marks/`](patches/marks/).

`extensions.md` is **not** in this tree (adaptive \(D_{\max}\), compact `sc`,
Yjs/Automerge, partition-recovery study).

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

cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/esbt.wasm web/
python3 -m http.server -d web 8080
```

Open `http://localhost:8080/?room=demo` in two browsers. Same-origin tabs
sync by themselves. Distinct machines: offer / accept / apply SDP.
