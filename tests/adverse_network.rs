//! Extension 4: behavior under adverse network conditions, studied with
//! deterministic simulation testing (the FoundationDB/turmoil discipline)
//! over the production `Document` API.
//!
//! A seeded discrete-event network delivers canonical `ESBM` update bytes
//! with delay, loss, duplication, reordering, and partitions; every scenario
//! is a pure function of its seed, so any failure replays exactly. Run with
//! `-- --nocapture` to see the recorded measurements.

use esbt::{Document, ErrorCode, ResourceLimits};
use esbt::replica::ReplicaConfig;

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn chance(state: &mut u64, permille: u64) -> bool {
    xorshift(state) % 1_000 < permille
}

struct InFlight {
    deliver_at: u64,
    sequence: u64,
    from: usize,
    to: usize,
    bytes: Vec<u8>,
}

/// Deterministic lossy broadcast network over `Document` replicas.
struct Sim {
    docs: Vec<Document>,
    now: u64,
    next_sequence: u64,
    inflight: Vec<InFlight>,
    rng: u64,
    drop_permille: u64,
    dup_permille: u64,
    max_delay: u64,
    /// `blocked[a][b]`: link between replicas a and b is cut.
    blocked: Vec<Vec<bool>>,
    pending_high_water: usize,
    delivered_bytes: usize,
    delivered_messages: usize,
}

impl Sim {
    fn new(replicas: usize, seed: u64, drop_permille: u64, dup_permille: u64) -> Self {
        let docs = (0..replicas)
            .map(|index| {
                Document::new(
                    index as u128 + 1,
                    ReplicaConfig::default(),
                    ResourceLimits::default(),
                )
                .expect("document")
            })
            .collect();
        Sim {
            docs,
            now: 0,
            next_sequence: 0,
            inflight: Vec::new(),
            rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            drop_permille,
            dup_permille,
            max_delay: 6,
            blocked: vec![vec![false; replicas]; replicas],
            pending_high_water: 0,
            delivered_bytes: 0,
            delivered_messages: 0,
        }
    }

    fn partition(&mut self, left: &[usize], right: &[usize]) {
        for &a in left {
            for &b in right {
                self.blocked[a][b] = true;
                self.blocked[b][a] = true;
            }
        }
    }

    fn heal_all(&mut self) {
        for row in &mut self.blocked {
            row.iter_mut().for_each(|cut| *cut = false);
        }
    }

    /// Random local edit on one replica; its canonical bytes are broadcast.
    fn edit(&mut self, target: usize, step: usize) {
        let len = self.docs[target].len();
        let update = if len == 0 || !chance(&mut self.rng, 200) {
            let index = (xorshift(&mut self.rng) as usize) % (len + 1);
            let text = char::from(b'a' + (step % 26) as u8).to_string();
            self.docs[target]
                .insert(index, &text, None)
                .expect("local insert")
        } else {
            let index = (xorshift(&mut self.rng) as usize) % len;
            self.docs[target].delete(index, 1, None).expect("local delete")
        };
        if let Some(update) = update {
            self.broadcast(target, update.canonical_bytes);
        }
    }

    fn broadcast(&mut self, from: usize, bytes: Vec<u8>) {
        for to in 0..self.docs.len() {
            if to == from {
                continue;
            }
            let mut copies = 1;
            if chance(&mut self.rng, self.drop_permille) {
                copies = 0;
            } else if chance(&mut self.rng, self.dup_permille) {
                copies = 2;
            }
            for _ in 0..copies {
                let delay = 1 + xorshift(&mut self.rng) % self.max_delay;
                self.next_sequence += 1;
                self.inflight.push(InFlight {
                    deliver_at: self.now + delay,
                    sequence: self.next_sequence,
                    from,
                    to,
                    bytes: bytes.clone(),
                });
            }
        }
    }

    /// Advance one tick: deliver everything due, dropping messages whose
    /// link is cut at delivery time (a severed link loses its traffic; the
    /// loss must be repaired by anti-entropy, never by luck).
    fn tick(&mut self) {
        self.now += 1;
        let now = self.now;
        let mut due: Vec<InFlight> = Vec::new();
        self.inflight.retain_mut(|message| {
            if message.deliver_at > now {
                return true;
            }
            due.push(InFlight {
                bytes: std::mem::take(&mut message.bytes),
                ..*message
            });
            false
        });
        due.sort_by_key(|message| (message.deliver_at, message.sequence));
        for message in due {
            if self.blocked[message.from][message.to] {
                continue;
            }
            self.delivered_bytes += message.bytes.len();
            self.delivered_messages += 1;
            self.docs[message.to]
                .apply_bytes(&message.bytes)
                .expect("apply broadcast update");
        }
        let pending: usize = self.docs.iter().map(Document::pending_len).sum();
        self.pending_high_water = self.pending_high_water.max(pending);
    }

    fn run_ticks(&mut self, ticks: u64) {
        for _ in 0..ticks {
            self.tick();
        }
    }

    /// One reconnect-style anti-entropy round over a reliable channel:
    /// every pair exchanges exactly the operations the other's gap-aware
    /// version summary is missing. Returns the bytes exchanged.
    fn anti_entropy_round(&mut self) -> usize {
        let mut exchanged = 0usize;
        for from in 0..self.docs.len() {
            for to in 0..self.docs.len() {
                if from == to {
                    continue;
                }
                let remote = self.docs[to].version();
                let bytes = self.docs[from]
                    .export_update(&remote)
                    .expect("export reconnect delta");
                exchanged += bytes.len();
                self.docs[to].apply_bytes(&bytes).expect("apply reconnect delta");
            }
        }
        exchanged
    }

    fn converged(&self) -> bool {
        let expected = self.docs[0].text();
        self.docs.iter().all(|doc| {
            doc.pending_len() == 0
                && doc.text() == expected
                && doc.state_hash() == self.docs[0].state_hash()
        })
    }
}

#[test]
fn partition_heals_through_membership_anti_entropy() {
    let mut sim = Sim::new(4, 0xE5B7_0001, 100, 50);

    // Connected warm-up traffic, then settle and repair transport loss.
    for step in 0..40 {
        let target = (xorshift(&mut sim.rng) as usize) % 4;
        sim.edit(target, step);
        sim.tick();
    }
    sim.run_ticks(10);
    sim.anti_entropy_round();
    assert!(sim.converged(), "warm-up did not converge");

    // Partition {0,1} | {2,3} for 200 ticks of concurrent editing.
    sim.partition(&[0, 1], &[2, 3]);
    for step in 0..80 {
        sim.edit(step % 4, step);
        if step % 2 == 0 {
            sim.tick();
        }
    }
    sim.run_ticks(160);
    let sides_diverged = sim.docs[0].text() != sim.docs[2].text();
    assert!(sides_diverged, "partition failed to isolate the sides");

    // Heal. Reconnect anti-entropy must transfer only membership
    // differences — no snapshot, no full history.
    sim.heal_all();
    sim.run_ticks(10);
    let recovery_bytes = sim.anti_entropy_round();
    let follow_up_bytes = sim.anti_entropy_round();
    assert!(sim.converged(), "post-heal replicas did not converge");
    println!(
        "partition/heal: recovery {} B in one round (second round {} B), \
         pending high-water {}, {} messages / {} B delivered pre-heal",
        recovery_bytes,
        follow_up_bytes,
        sim.pending_high_water,
        sim.delivered_messages,
        sim.delivered_bytes,
    );
    // A second round exchanges only empty canonical updates.
    let empty_update = esbt::snapshot::Message::Update(esbt::Update::default())
        .encode()
        .len();
    let empty_round = sim.docs.len() * (sim.docs.len() - 1) * empty_update;
    assert_eq!(follow_up_bytes, empty_round, "recovery did not terminate");
}

#[test]
fn prolonged_disconnection_survives_history_compaction() {
    let mut a = Document::with_defaults(1).expect("a");
    let mut b = Document::with_defaults(2).expect("b");
    let mut c = Document::with_defaults(3).expect("c");

    // Shared base, fully synced.
    let base = a.insert(0, "shared base ", None).expect("base").unwrap();
    b.apply_bytes(&base.canonical_bytes).expect("b base");
    c.apply_bytes(&base.canonical_bytes).expect("c base");

    // C disconnects and edits offline.
    let offline = c
        .insert(c.len(), "[offline-edit]", None)
        .expect("offline edit")
        .unwrap();

    // A and B keep editing, stay synced, and A compacts its journal through
    // the fully acknowledged prefix — C is now behind A's history floor.
    for step in 0..30 {
        let update = a
            .insert(a.len(), &char::from(b'a' + (step % 26)).to_string(), None)
            .expect("online edit")
            .unwrap();
        b.apply_bytes(&update.canonical_bytes).expect("b online");
    }
    let acknowledged = b.version();
    let pruned = a.prune_history_through(&acknowledged).expect("prune");
    assert!(pruned > 0, "compaction removed nothing");

    // Reconnect: the op-level path must refuse, typed — A no longer retains
    // the journal C needs.
    let refusal = a.export_update(&c.version()).expect_err("delta must fail");
    assert_eq!(refusal.code, ErrorCode::HistoryUnavailable);

    // Recovery path: C rebases onto A's compact snapshot. Its offline edits
    // are retained journal, so the merge replays them instead of losing them.
    let snapshot = a.export_compact_snapshot().expect("compact snapshot");
    let receipt = c.apply_snapshot_bytes(&snapshot).expect("rebase onto snapshot");
    assert!(c.text().contains("[offline-edit]"), "offline edits lost");
    assert!(c.text().contains("shared base "), "base lost");

    // C's offline operations flow back and all three converge.
    let to_a = c.export_update(&a.version()).expect("c delta to a");
    a.apply_bytes(&to_a).expect("a applies offline edits");
    let to_b = c.export_update(&b.version()).expect("c delta to b");
    b.apply_bytes(&to_b).expect("b applies offline edits");
    let b_gap = a.export_update(&b.version()).expect("b catch-up");
    b.apply_bytes(&b_gap).expect("b applies catch-up");
    assert_eq!(a.text(), c.text());
    assert_eq!(a.text(), b.text());
    println!(
        "compaction recovery: pruned {} ops, snapshot {} B, offline delta {} B, receipt {:?}",
        pruned,
        snapshot.len(),
        to_a.len(),
        receipt.kind,
    );
    let _ = offline;
}

#[test]
fn sparse_receipts_refuse_to_pose_as_a_merge_base() {
    // A replica that received sequence 2 before sequence 1 must advertise
    // the hole, and must refuse to export a compact base while it exists.
    let mut source = Document::with_defaults(1).expect("source");
    let first = source.insert(0, "A", None).expect("first").unwrap();
    let second = source.insert(1, "B", None).expect("second").unwrap();

    let mut sparse = Document::with_defaults(2).expect("sparse");
    sparse.apply_bytes(&second.canonical_bytes).expect("second only");
    assert_eq!(sparse.text(), "B");

    let refusal = sparse
        .export_compact_snapshot()
        .expect_err("sparse base must refuse");
    assert_eq!(refusal.code, ErrorCode::SnapshotNotCausallyClosed);

    // The gap-aware summary lets the source repair exactly the hole.
    let repair = source.export_update(&sparse.version()).expect("repair");
    sparse.apply_bytes(&repair).expect("apply repair");
    assert_eq!(sparse.text(), "AB");
    assert!(sparse.export_compact_snapshot().is_ok());
    let _ = first;
}

#[test]
fn crash_recovery_restores_pending_state_and_identity() {
    let mut a = Document::with_defaults(1).expect("a");
    let mut b = Document::with_defaults(2).expect("b");

    let insert = a.insert(0, "x", None).expect("insert").unwrap();
    let delete = a.delete(0, 1, None).expect("delete").unwrap();
    let own = b.insert(0, "kept", None).expect("own edit").unwrap();

    // The delete reaches B before its insertion: causally buffered.
    b.apply_bytes(&delete.canonical_bytes).expect("early delete");
    assert_eq!(b.pending_len(), 1);

    // B crashes; only its persisted full archive survives.
    let archive = b.export_full_snapshot().expect("persist archive");
    let mut restored = Document::with_defaults(2).expect("restored");
    restored.apply_snapshot_bytes(&archive).expect("restore archive");
    assert_eq!(restored.pending_len(), 1, "pending op lost in the crash");
    assert_eq!(restored.text(), b.text());
    assert_eq!(restored.state_hash(), b.state_hash());

    // The missing insertion resolves the buffered delete after recovery.
    restored.apply_bytes(&insert.canonical_bytes).expect("late insert");
    assert_eq!(restored.pending_len(), 0);
    assert_eq!(restored.text(), "kept");

    // Identity resumes: new local edits reuse no (origin, seq) or counter.
    let next = restored.insert(0, "y", None).expect("post-crash edit").unwrap();
    a.apply_bytes(&own.canonical_bytes).expect("a pre-crash edit");
    a.apply_bytes(&next.canonical_bytes).expect("a post-crash edit");
    let catch_up = restored.export_update(&a.version()).expect("catch-up");
    a.apply_bytes(&catch_up).expect("apply catch-up");
    let back = a.export_update(&restored.version()).expect("back-fill");
    restored.apply_bytes(&back).expect("apply back-fill");
    assert_eq!(a.text(), restored.text());
    println!(
        "crash recovery: archive {} B, pending preserved, post-crash identity clean",
        archive.len()
    );
}

#[test]
fn chaos_schedules_converge_for_every_seed() {
    let mut report = Vec::new();
    for seed in 1..=12u64 {
        let mut sim = Sim::new(4, 0xC4A0_5000 + seed, 150, 100);
        let mut partitioned = false;
        for step in 0..300 {
            // Random partitions form and heal mid-traffic.
            if step % 50 == 25 {
                if partitioned {
                    sim.heal_all();
                } else {
                    let pivot = 1 + (xorshift(&mut sim.rng) as usize) % 3;
                    let members: Vec<usize> = (0..4).collect();
                    let (left, right) = members.split_at(pivot);
                    sim.partition(left, right);
                }
                partitioned = !partitioned;
            }
            let target = (xorshift(&mut sim.rng) as usize) % 4;
            sim.edit(target, step);
            sim.tick();
        }
        sim.heal_all();
        sim.run_ticks(sim.max_delay + 1);

        let mut rounds = 0;
        let mut recovery_bytes = 0;
        while !sim.converged() {
            rounds += 1;
            assert!(rounds <= 8, "seed {seed} did not converge in 8 rounds");
            recovery_bytes += sim.anti_entropy_round();
        }
        report.push((seed, rounds, recovery_bytes, sim.pending_high_water));
    }
    for (seed, rounds, recovery_bytes, pending_high_water) in &report {
        println!(
            "chaos seed {seed}: converged in {rounds} anti-entropy rounds, \
             {recovery_bytes} recovery B, pending high-water {pending_high_water}"
        );
    }
}
