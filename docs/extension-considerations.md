# Extension considerations

`extensions.md` quotes the future-work section of the ESBT paper
(arXiv:2607.28101v1, §10). It names four extensions:

1. adaptive mechanisms for automatically tuning `Dmax` from observed editing
   dynamics and workload characteristics;
2. compact encoding and serialization for the sequence path `sc`;
3. ESBT as a **pluggable identifier allocation strategy** for existing
   collaborative frameworks (the paper names Yjs and Automerge), evaluated
   under real-world workloads;
4. behavior and performance under adverse network conditions — partitions,
   prolonged disconnection, replica recovery.

This document records, per extension: what the paper actually proposes
(cross-checked against the text and against this repository's source), the
prior work surveyed for the Jan 2024 – Aug 2026 window, the full stack of
candidate designs considered — including non-obvious ones imported from other
domains — and the reason each pick won. "Easier to code" was excluded as a
criterion throughout; several rejected options are strictly easier to code
than the picks.

## Prior-work survey and applicability rulings

Strictness rule applied: a paper's benchmark data is only admissible if the
paper does something very close to what the extension needs. Methodology may
transfer where data may not.

| Work | Date | What it does | Ruling |
|---|---|---|---|
| Eg-walker (Gentle & Kleppmann, EuroSys) | 2025 | Event-graph walking; replaces persistent positional identifiers entirely; publishes real editing traces | In window. Its *identifier* results do not transfer (it has no allocator to tune or encode). Its **editing-trace corpus idea** transfers to Extension 3's evaluation. Its numbers are not used. |
| Crust (IEEE Access) | 2025 | Rust framework that validates CRDT convergence during/after simulated network partitions | In window. Closest match for Extension 4's *methodology* (partition → diverge → heal → verify convergence). Its benchmarks target set/counter CRDTs on clusters, not sequence-identifier schemes — **numbers not used**. |
| Loro fractional-index jitter | 2024 | Random jitter bytes appended to fractional indexes to reduce concurrent-insert collisions in movable trees | In window. Solves collision probability, not identifier growth or bound tuning. Confirms the "position-string slot" exists in shipping frameworks (used by Extension 3). No benchmark borrowed. |
| Automerge columnar encoding experiments | pre-window (design still current) | Columnar layout, RLE, LEB128, actor-ID dictionary tables for op metadata | Techniques (varints, site dictionaries) transfer to Extension 2 as encoding *vocabulary*; the measured 1.1 B/op figure is for RGA-style op IDs, not Stern–Brocot weights — **numbers not used**. |
| LSEQ (Nédelec et al.) | 2013 (out of window) | Adaptive *choice among allocation strategies* (boundary+/boundary−, random), base doubling | Out of window and adapts a different thing (strategy selection, not a numeric bound). Cited as conceptual ancestor for Extension 3's strategy seam only. |
| Self-tuned congestion control (Duke, HPCA) | 2001 (out of window, different domain) | Hill-climbing threshold tuner driven by delivered throughput, with drop-back on regression | Imported as a *control pattern* for Extension 1 (explicitly allowed: "standard solutions used in other domains"). No data borrowed. |
| Deterministic simulation testing (FoundationDB lineage; turmoil, madsim) | active 2024–2025 | Seeded, single-threaded simulation of hosts + network faults; reproducible failure schedules | Imported as the *testing discipline* for Extension 4. |
| Stern–Brocot number system (Graham/Knuth/Patashnik; Niqui, J. Discrete Algorithms) | classical | A positive rational ⇔ a unique finite L/R path; the path is the run-length form of the continued-fraction expansion, and lexicographic path order equals rational order | Imported as the mathematical basis for Extension 3's order-preserving keys and evaluated (rejected) as a wire encoding for Extension 2. |

No paper in the window was found that (a) adaptively tunes a bounded-mediant
threshold, (b) compactly encodes Stern–Brocot composite identifiers, (c) hosts
a rational-allocation scheme inside Yjs/Automerge, or (d) measures a
tombstone-free weight-reuse CRDT under partitions. All four extensions are
therefore implemented from first principles, with cross-domain imports where
they fit, and no external benchmark numbers are cited as evidence for this
repository's behavior.

---

## Extension 1 — adaptive `Dmax`

### What the paper proposes, cross-checked

`Dmax` bounds the numerator and denominator of newly minted mediants
(Lemma 1, Theorem 2). When the mediant exceeds it, allocation falls through to
the `sn` ladder and then the `sc` path. In this repository the bound is a
static field (`ReplicaConfig::dmax`, default `2^16`) consumed by
`Allocator::mediant_fits`. Nothing observes the workload.

A fact the paper never states but that the formal model implies, and which
this design depends on: **`Dmax` is a purely local allocation policy.**
Definition 2's total order and Theorems 3–5's convergence argument depend only
on weights being unique and totally ordered — never on *how* a site chose its
weight. Two replicas with different (or time-varying) `Dmax` values still
converge. That makes runtime adaptation safe without any coordination
protocol, which is what makes this extension tractable at all. A dedicated
test (`heterogeneous_and_time_varying_dmax_replicas_converge`) pins this.

### What `Dmax` actually trades off in this codebase

- Fractions are stored as two `i64`s in memory, so **in-memory** identifier
  size is unaffected by `Dmax`. The bound's real effects are (a) wire bytes
  once fractions are varint-encoded (Extension 2 makes fraction cost grow
  with `log2 Dmax`), (b) overflow headroom for the `i128` cross-multiply
  comparison, and (c) *when* allocation abandons the dense fraction layer for
  the `sn`/`sc` layers.
- Workload asymmetry: boundary editing (append/prepend) grows one side of the
  fraction **linearly** (`n/1`, `1/n`), so a rejected mediant is typically a
  *near miss* — barely above the bound — and raising `Dmax` genuinely buys
  more cheap fraction-layer allocations. Repeated middle insertion grows the
  mediant **exponentially** (Fibonacci-like), so rejected mediants overshoot
  by orders of magnitude and no realistic `Dmax` increase helps; the `sn`/`sc`
  layers are the right tool there. An adaptive mechanism must tell these two
  rejection regimes apart or it will chase an exponential.

### Consideration stack

1. **Static profiles selected at document creation** (e.g. "log-like" vs
   "prose-like"). Rejected: it is not adaptive — the paper asks for tuning
   from *observed* dynamics, and a document's insertion pattern changes over
   its lifetime.
2. **Consensus-tuned global `Dmax`** (replicas agree on a bound via metadata
   in updates). Rejected: adds a coordination protocol to a
   coordination-free CRDT for zero correctness benefit — the bound is
   provably local (see above). Also creates a mixed-version hazard the paper
   never asks for.
3. **Machine-learned predictor** (classify workload from a feature window).
   Rejected: unverifiable in an engine whose decoding and allocation paths
   are otherwise exact and deterministic; the training corpus does not exist;
   and the signal available (rejected-mediant magnitude) is already almost
   perfectly separable by inspection, so learning adds risk without
   information.
4. **Loro-style jitter** (randomize allocation inside the gap). Rejected for
   this extension: jitter addresses concurrent-collision probability, not the
   bound; it also injects nondeterminism into an allocator whose local
   determinism the test suite and the undo/rollback journal rely on.
5. **AIMD on fallback rate alone** (raise `Dmax` when fraction-layer
   rejections are frequent). Rejected as stated: it cannot distinguish the
   linear-drift regime (raising helps) from the exponential-pinch regime
   (raising is futile), so a middle-insertion attack would ratchet `Dmax` to
   its ceiling for nothing.
6. **Picked: magnitude-discriminating hill-climb with hysteresis** — a
   composite of the congestion-control pattern (import from the networking
   domain) and the near-miss discrimination that is specific to mediant
   arithmetic:
   - Every fraction-layer decision records whether the mediant fit, and if
     not, whether the rejected magnitude was a *near miss*
     (`max(p,q) < Dmax × 8`) or an *overshoot*.
   - At window boundaries (256 fraction-layer decisions), if near misses
     dominate rejections and rejections are a meaningful share of decisions,
     `Dmax` doubles (multiplicative increase toward a hard ceiling of `2^31`,
     which preserves the `i128` cross-multiplication headroom and the
     wasm-facing invariants).
   - Overshoot-dominated windows leave `Dmax` alone: the pinch belongs to
     `sn`/`sc` by design (this is the paper's own layering argument).
   - Every increase is a *probe*: the controller keeps an EWMA of encoded
     identifier cost (bytes, using the Extension 2 codec as the cost model);
     if cost regressed after a probe, the controller steps back down and
     enters a hold-off period (hysteresis) so it cannot oscillate. This is
     the Duke self-tuning shape: climb on the objective, drop back on
     regression, never trust a single window.

   Why this pick: it is the only candidate that (a) uses signals actually
   observable at the allocator (rejected-mediant magnitudes are computed and
   then discarded today), (b) is provably convergence-neutral, (c) cannot be
   driven to a pathological state by the known adversarial workload, and
   (d) optimizes the quantity the paper cares about (identifier storage)
   rather than a proxy.

## Extension 2 — compact encoding of the sequence path

### What the paper proposes, cross-checked

§8.3.1 concedes that the prototype "explicitly stores the sequence-path field
even when it contains only its default value (`sc = [0]`)" and says the
default "can be represented implicitly during serialization". This
repository's codec has the same flaw and more: `write_weight` emits
`p: i64 + q: i64 + sn: i64 + site: u128 + len: u16 + 4 bytes per sc digit` —
46 bytes minimum per weight, before the operation envelope. Three structural
redundancies exist:

- the default path `[0]` is always materialized;
- an insertion's `weight.site` always equals the operation's `origin`
  (enforced by `import_operation`), yet both 16-byte values are written;
- consecutive snapshot atoms are sorted by weight, and this engine's
  typing-run reservation gives runs a long shared `sc` root
  (site-discriminator prefix + run counter), yet every atom re-serializes the
  full path.

### Consideration stack

1. **General-purpose compression (gzip/zstd over the payload)**. Rejected:
   it destroys the engine's exact, bounded, non-panicking decode contract
   (decompression bombs, non-canonical representations of equal states), and
   it hides the structure instead of removing it. Transport may still stack
   compression on top later.
2. **Continued-fraction / Stern–Brocot path encoding of `f` on the wire.**
   The SB path (equivalently the CF run-lengths) is the information-optimal
   representation of a mediant-derived rational and was seriously evaluated.
   Rejected *for the wire codec*: with `p,q < 2^31` a canonical varint pair
   costs at most 10 bytes and usually 2–4; the CF form saves little, has a
   dual-representation hazard (`[...,a] ≡ [...,a−1,1]`) that complicates the
   canonical-bytes rule the decoders enforce, and costs a Euclidean walk per
   decode. The representation is not discarded — it becomes the *order-
   preserving* key basis in Extension 3, where lexicographic-order-equals-
   rational-order is worth paying for and varint pairs cannot provide it.
3. **Full columnar layout with RLE across operations (Automerge-style).**
   Rejected at this scope: operations inside an `Update` are canonically
   sorted by `(origin, seq)` — transport identity — not by weight, so
   cross-op weight columns RLE poorly; and the journal/receipt machinery
   (`journal_bytes`) requires each operation to remain independently
   re-emittable. Columnarizing would force a second, non-journal format and
   double the canonicality surface. The two techniques from that lineage
   that *do* fit are taken: LEB128 varints and dictionary/reference encoding
   of repeated 16-byte site IDs.
4. **Picked: structure-aware weight codec + sorted-context prefix sharing**,
   incorporated into the one unified ESBT artifact codec (the repository's
   explicit no-legacy-decoder policy makes a clean break correct):
   - a per-weight flags byte marks `sn = 0` (omitted), `sc = [0]` (omitted),
     and, in operations, `site = origin` (omitted — free 16-byte saving on
     every insertion, justified by the invariant the replica already
     enforces);
   - `p`, `q`, sequence counts and `sc` digits as **canonical** LEB128
     varints (non-minimal encodings rejected), `sn` zigzag-varint;
   - in snapshots, where atoms and delete-log entries are canonically sorted
     by weight, each weight's `sc` is **front-coded** against its
     predecessor (shared-prefix length + suffix). Front coding is the
     imported technique here — it comes from string dictionaries and search
     indexes, not from CRDT literature — and it is what actually attacks the
     paper's stated target, because this engine's deep paths (typing-run
     roots) are exactly the ones that repeat prefix-for-prefix across
     adjacent atoms. Canonicality is preserved by *requiring* the encoded
     shared length to equal the true longest common prefix, which the
     decoder recomputes and enforces;
   - sites in snapshots are dictionary-encoded (sorted unique table once,
     varint references per weight).

   Why this pick: every byte removed corresponds to a redundancy the engine
   *proves* (invariants already rejected at decode time), so the exact-decode
   discipline survives; the default-path fix implements the paper's own
   observation; the front-coding attacks the only variable-length component
   (`sc`), which §8.1 identifies as the entire growth story; and it composes
   with Extension 1, which needs encoded size as its cost signal.

Measured on this repository after implementation
(`tests/identifier_size.rs`; same engine and weights, the retired fixed-width
prototype recomputed from its closed-form layout — no external numbers, and quoted
here together with the lazy run-reservation change below): a 220-unit
two-site concurrent typing-run snapshot shrinks from 13,356 to 2,375 bytes
(−82.2%), a 400-operation mixed-editing journal shrinks from 33,558 to
17,666 bytes (−47.4%) when each operation is wrapped as its own complete
Update artifact, and a single complete insertion Update drops from 81 to 42
bytes. These artifact measurements include the 11-byte unified ESBT envelope.

**Follow-up: compact the update payload itself.** Per-operation
encoding still repeated the 16-byte origin and a 4-byte length prefix per
operation. The canonical Update payload gives updates the same treatment as snapshots
already had: a sorted per-update site dictionary (origins and weight sites
become varint indexes), self-delimiting varint operation records, and
sequence paths front-coded across the canonically `(origin, seq)`-sorted
operation list — which places each typing run's weights adjacently, so run
roots are paid once per update. The same 400-operation batch as one canonical
Update artifact is 5,665 bytes against 35,173 for the fixed-width prototype
(−83.9%) and roughly one third of the standalone complete-artifact sum. Downstream,
the adverse-network suite's
measured recovery traffic dropped accordingly (partition-heal round
6,591 → 2,891 bytes; offline reconnect delta 518 → 211 bytes; crash archive
419 → 331 bytes).

## Extension 3 — ESBT as a pluggable identifier allocation strategy

### What the paper proposes, cross-checked

"Integrate ESBT as a pluggable identifier allocation strategy into existing
collaborative editing frameworks, such as Yjs and Automerge, and evaluate its
effectiveness under real-world collaborative workloads."

Cross-checking against the named frameworks' internals: **neither Yjs nor
Automerge has an identifier-allocation seam.** Yjs items are
`(clientID, clock)` Lamport pairs positioned by YATA origin pointers;
Automerge uses RGA-style op IDs. There is no component that "allocates a
position identifier between two neighbors" to swap out — the paper's phrasing
presumes a Logoot/LSEQ-shaped host. A literal reading therefore requires
forking Yjs's `Item#integrate` or Automerge's op tree, which is (a) outside
this repository's declared boundary (the engine repo owns the CRDT and has
no second browser implementation), and (b) would not
produce ESBT-in-Yjs but a new CRDT wearing Yjs's API.

Where a real allocation seam *does* exist in shipping frameworks is the
**fractional-index / position-string slot**: Figma's ordered sequences,
Loro's movable-tree sibling order (2024), y-utility position encodings, and
every database pattern that stores an ordered collection under sortable keys.
Those hosts accept any generator of totally ordered keys with a
"mint-between" operation. That is exactly what ESBT is.

### Consideration stack

1. **Fork Yjs, replace YATA integration with ESBT ordering.** Rejected: not
   a plug-in but a reimplementation; violates the repository boundary;
   unmaintainable against upstream; and it would invalidate the paper's own
   comparison story (the result would be neither Yjs nor ESBT).
2. **Automerge adapter via its columnar op model.** Rejected for the same
   category reason: Automerge's ordering is causal-graph-derived; there is
   no identifier to substitute.
3. **Sync-protocol-level bridge** (run ESBT internally, speak y-protocols to
   Yjs peers). Rejected: a translation gateway between two engines with
   different intention-preservation behavior cannot be made convergent in
   general (the same concurrent edits legitimately order differently), so
   the "integration" would be unsound rather than pluggable.
4. **Implemented as bounded adjacent research, not claimed as the literal
   host integration:**
   - **a. Pluggable allocation strategy inside the allocator.** The digit
     choice inside `NEWSEQ` becomes a strategy value (`Midpoint` — the
     paper's algorithm and the default; `BoundaryLow(w)` / `BoundaryHigh(w)`
     — LSEQ's boundary± translated to the `sc` digit space;
     `AlternatingByDepth(w)` — LSEQ's alternation made deterministic by
     depth parity instead of cached coin flips, preserving the engine's
     reproducibility and rollback guarantees). This is the "pluggable
     strategy" half of the sentence made real *in the direction that is
     actually sound*: the allocator is the host, strategies are the plugs,
     and ESBT's weight structure is the invariant.
   - **b. Order-preserving byte keys (`orderkey` module).** A `Weight` is
     encoded to a byte string whose lexicographic order equals Definition 2's
     weight order, and decoded back exactly. The fraction is encoded as its
     Stern–Brocot L/R path in run-length form — the classical result that
     lexicographic path order is rational order — with FoundationDB-tuple-
     style order-preserving length-prefixed integers (ascending for R-runs,
     complemented for L-runs), a terminator symbol that sorts strictly
     between L and R, `sn` as an order-preserving signed field, `sc` digits
     as order-preserving varints behind a low terminator (shorter path
     precedes, matching `sc_cmp`), and the site as a fixed big-endian
     tiebreak. `key_between(left?, right?, site)` mints a new key strictly
     inside the gap. This is an **experimental native sortable-key adapter**
     for a host that already has that seam. It is not exported through WIT,
     is not part of the ESBT document codec, and is not wired into Marks,
     Yjs, Automerge, or Loro.
   - **c. Real-world workload evaluation.** A trace-replay harness
     (`examples/trace_replay.rs`) consumes the automerge-perf real-keystroke
     editing-trace format — the corpus the Yjs and Automerge communities
     benchmark with — plus deterministic synthetic adversarial patterns, and
     reports per-strategy allocation-layer histograms, encoded identifier
     bytes (the canonical weight codec), and snapshot sizes. The evaluation compares this
     engine's own strategies against each other, faithfully to the survey
     ruling that no external system is similar enough to donate its numbers.

**Scope verdict:** the repository does not satisfy the paper's literal
"integrate into Yjs or Automerge" request. It establishes an internal
strategy seam, implements and tests a sortable-key reinterpretation, and
evaluates those pieces on a real trace. Calling that a completed framework
integration would require a pinned external host adapter and host-level
convergence/performance evidence, neither of which exists here. Keeping the
adapter native-only also avoids creating a seventh document artifact or a
second host protocol merely to make the claim look broader.

### Recorded results (`cargo run --release --example trace_replay`)

Replaying the automerge-perf real-keystroke trace (259,778 edits; the replay
is validated by reproducing the corpus's known final document, 104,852
units): every strategy converges to the same document; the paper's midpoint
produces a 20.8 MB compact-encoded journal (≈80 B/op) and a 1.30 MB compact
snapshot; `BoundaryLow(64)` trades deeper mean paths (46.8 vs 38.8
components) for an 8% smaller journal. On the synthetic 10,000-op
adversarial middle-insertion pattern the strategy seam matters far more:
`BoundaryLow(64)` emits 175 MB of journal against midpoint's 275 MB (−36%).
That workload also quantified a real engine trade-off the evaluation
surfaced — and drove a fix. The original typing-run reservation appended a
fixed-width full-site discriminator (5–6 digits at the production base) to
*every* first insertion, which amplified sequence-path depth under
adversarial middle insertion to a mean of ≈17,500 components at 10,000 ops
on a bare `Replica`. The production `Document` boundary already failed such
mints typed (`IdentifierTooDeep`), but availability suffered.

**Follow-up: lazy site-marked run roots.** True laziness (pay nothing until
a run continues) is provably impossible for twin-prone roots: any
continuation of a first character `C` sorts after every same-`(f,sn,sc)`
twin of `C`, so contiguity must be established at the first character or
never. What *is* safe is observing that mediant- and gap-midpoint-layer
candidates already end in a site-derived digit — their subtrees cannot
collide — while sn-ladder candidates (which copy `left.sc`, paper
Algorithm 2 line 21) and NEWSEQ midpoints do not. The reservation now roots
the run at the first character's own weight and appends at most **one**
site digit, only when the candidate is not already site-derived. The
certainty of the old fixed-width prefix degrades to
whp-contiguity (site pairs colliding modulo `base − 1`, ≈2⁻³¹ at the
production base; convergence is unaffected either way, and every pinned
contiguity test still passes). Measured effect: adversarial middle
insertion drops from a mean path depth of 17,499 to 7,498 and from a 275 MB
to a 175 MB journal (replay 13.2 s → 4.7 s; `BoundaryLow(64)` now 75 MB);
boundary typing drops from constant depth 6–7 to 1–2; the real-trace
journal shrinks 20.8 MB → 15.3 MB (−27%) with mean path depth 38.8 → 15.6.

The adaptive controller correctly stays idle on the real trace — at the
default bound the fraction layer is never pressured — which is the designed
behavior, not a gap.

## Extension 4 — adverse network conditions

### What the paper proposes, cross-checked

§4 *assumes* reliable epidemic broadcast; §10 asks what happens without it:
partitions, prolonged disconnection, replica recovery. The engine already
carries the primitives a hostile network needs — gap-aware version summaries
(sparse receipts, deliberately not just a max), `ops_missing_from`
membership-based anti-entropy, `merge_snapshot` with typed refusals
(`SnapshotHasSequenceGaps`, `MissingLocalHistory`), causally-buffered
deletes, and idempotent receive — but nothing in the repository *exercises*
them under an adversarial schedule; the existing tests hand-order a few
messages.

### Consideration stack

1. **Live-network chaos testing** (real processes, `tc netem`/toxiproxy or a
   Jepsen-style harness over the demo server). Rejected: non-reproducible
   failures are the worst possible fit for an engine whose value proposition
   is exactness; CI can't carry it; and the browser mesh (`web/mesh.js`) is
   product surface, not engine surface.
2. **Statistical/randomized property tests only** (extend the existing
   shuffled-delivery tests with drops). Rejected as insufficient: reordering
   and duplication are already covered; the *interesting* failures live in
   scheduled interactions between partition topology, compaction timing, and
   recovery paths, which random shuffles reach with vanishing probability
   and reproduce never.
3. **Formal modeling (TLA+/statistical model checking) of the sync layer.**
   Considered seriously — it is the strongest guarantee — but rejected as
   the *deliverable* here because the extension asks for observed "behavior
   and performance", i.e. an empirical study with measurements, and a model
   would not execute this codebase (the divergences the README documents —
   typing runs, reuse ordering, sparse receipts — are exactly what a
   re-model would abstract away).
4. **Picked: deterministic simulation testing (DST), imported from the
   database world** (FoundationDB lineage; turmoil/madsim are the Rust
   incarnations), implemented as a dependency-free, seeded discrete-event
   simulator over the public engine API (`tests/adverse_network.rs`):
   - a virtual network with per-link delay windows, drop and duplication
     probabilities, and an explicit partition matrix, all driven by one
     xorshift seed — any failure is a replayable seed, matching the engine's
     exactness culture;
   - scripted scenarios: two-sided partition with concurrent editing and
     heal-time anti-entropy; prolonged disconnection across a compaction
     horizon (asserting the typed `MissingLocalHistory` refusal fires and
     that full-snapshot recovery then converges *without* losing offline
     edits); crash + restart from persisted `FullSnapshot` bytes; and a
     many-seed randomized chaos schedule (partitions forming and healing
     mid-traffic, duplicated and reordered delivery) ending in full
     anti-entropy;
   - measurements, not just assertions: recovery-op counts, exchanged bytes,
     and pending-queue high-water marks are computed per scenario and
     recorded below.

   Why this pick over a third-party simulator crate: the engine is sans-IO
   and single-threaded by design, so hosting it under an async-runtime
   simulator (turmoil/madsim) would add a tokio dependency and a fake
   concurrency layer only to schedule pure function calls; a bespoke
   discrete-event loop gives the same determinism with none of the
   dependency surface, and keeps the suite runnable under the wasm-oriented
   toolchain constraints. The Crust framework's partition→heal→verify shape
   is followed; its data is not cited.

### Recorded results (`cargo test --test adverse_network -- --nocapture`)

The suite is deterministic; every number below replays exactly from its seed.

- **Partition/heal** (4 replicas, lossy links with 10% drop / 5% duplicate /
  1–6-tick delay; a 200-tick {0,1}|{2,3} partition with 80 concurrent
  edits): the sides demonstrably diverge, then one reconnect anti-entropy
  round of 2,891 bytes restores full convergence; the follow-up round
  exchanges only empty canonical updates, proving termination.
  Pending-queue high-water stayed at 2 across 178 delivered lossy messages.
- **Prolonged disconnection across compaction**: after the connected
  replicas prune 42 acknowledged operations, the op-level reconnect path
  refuses the returning replica with typed `HistoryUnavailable`; rebasing
  onto a 528-byte compact snapshot preserves every offline edit (they are
  retained journal, replayed over the new base), and the offline delta flows
  back in 211 bytes to full three-way convergence.
- **Sparse receipts**: a replica holding sequence 2 without sequence 1
  advertises the hole, refuses to export a compact base
  (`SnapshotNotCausallyClosed`), is repaired by exactly the missing
  operation via the gap-aware summary, and only then may serve as a base.
- **Crash/recovery**: a 331-byte persisted full archive restores a replica
  with its causally buffered delete intact (`pending_len` 1 before and
  after), the late insertion resolves it post-restart, and post-crash local
  edits reuse no operation identity.
- **Chaos schedules** (12 distinct seeds × 300 events, partitions forming
  and healing mid-traffic every 50 ticks, 15% drop / 10% duplicate):
  every seed converges in **one** anti-entropy round after the final heal
  (5,105–6,795 recovery bytes with the canonical Update artifact; pending high-water 17–35),
  and no pending operation survives convergence.
