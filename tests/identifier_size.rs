//! Extension 2 measurements against the retired fixed-width prototype.
//!
//! The prototype had a closed-form size, so the comparison reconstructs byte
//! counts arithmetically from the same weights. No legacy encoder or decoder
//! is present.

use esbt::replica::{Replica, ReplicaConfig};
use esbt::{Artifact, Op, OpKind, Update, Weight};

/// Prototype weight: p:i64 + q:i64 + sn:i64 + site:u128 + len:u16 + 4·sc.
fn fixed_weight_bytes(weight: &Weight) -> usize {
    8 + 8 + 8 + 16 + 2 + 4 * weight.sc.len()
}

/// Prototype operation: tag + origin:u128 + seq:u64 + c:u64 + weight (+ unit).
fn fixed_op_bytes(op: &Op) -> usize {
    1 + 16
        + 8
        + 8
        + fixed_weight_bytes(&op.weight)
        + if matches!(op.kind, OpKind::Ins { .. }) {
            2
        } else {
            0
        }
}

/// Prototype snapshot: magic + version + length-prefixed insertion and
/// version summaries + u32-counted atom and delete lists.
fn fixed_snapshot_bytes(snapshot: &esbt::Snapshot) -> usize {
    let mut total = 4 + 2;
    total += 4 + snapshot.insertions.encode().len();
    total += 4;
    for atom in &snapshot.atoms {
        total += fixed_weight_bytes(&atom.weight) + 2 + 8;
    }
    total += 4;
    for (weight, _) in &snapshot.delete_log {
        total += fixed_weight_bytes(weight) + 8;
    }
    total + 4 + snapshot.version.encode().len()
}

fn cfg() -> ReplicaConfig {
    ReplicaConfig::default()
}

fn one_update_bytes(operation: &Op) -> Vec<u8> {
    Artifact::Update(Update::new(vec![operation.clone()]).expect("one operation")).encode()
}

#[test]
fn minimal_insertion_operation_shrinks() {
    let mut replica = Replica::new(3, cfg());
    let op = replica.local_insert(0, 'A' as u16);
    let fixed = fixed_op_bytes(&op);
    let compact = one_update_bytes(&op).len();
    println!("minimal insertion update: fixed {fixed} B, canonical {compact} B");
    assert!(
        compact * 5 < fixed * 3,
        "fixed {fixed} vs compact {compact}"
    );
}

#[test]
fn concurrent_typing_run_snapshot_shrinks() {
    let mut a = Replica::new(1, cfg());
    let mut b = Replica::new(2, cfg());
    let from_a = a.local_insert_str(0, &"the quick brown fox ".repeat(5));
    let from_b = b.local_insert_str(0, &"jumps over the lazy dog ".repeat(5));
    for op in &from_b {
        a.receive(op.clone());
    }
    for op in &from_a {
        b.receive(op.clone());
    }
    assert_eq!(a.text(), b.text());
    assert_eq!(a.len(), 220);

    let snapshot = a.snapshot();
    let fixed = fixed_snapshot_bytes(&snapshot);
    let compact = snapshot.encode().len();
    println!(
        "two-site typing-run snapshot ({} units): fixed {fixed} B, compact {compact} B ({:.1}%)",
        a.len(),
        100.0 * compact as f64 / fixed as f64
    );
    assert!(compact * 3 < fixed, "fixed {fixed} vs compact {compact}");
    assert_eq!(esbt::Snapshot::decode(&snapshot.encode()), Some(snapshot));
}

#[test]
fn mixed_editing_journal_shrinks() {
    let mut a = Replica::new(1, cfg());
    let mut b = Replica::new(2, cfg());
    let mut state = 0x9e37_79b9u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };

    let mut ops = Vec::new();
    for step in 0..400u32 {
        let replica = if step % 2 == 0 { &mut a } else { &mut b };
        let len = replica.len();
        if len == 0 || next() % 5 != 0 {
            let index = next() as usize % (len + 1);
            ops.push(replica.local_insert(index, u16::from(b'a' + (step % 26) as u8)));
        } else if let Some(op) = replica.local_delete(next() as usize % len) {
            ops.push(op);
        }
    }
    let (fixed, standalone): (usize, usize) = ops
        .iter()
        .map(|op| (fixed_op_bytes(op), one_update_bytes(op).len()))
        .fold((0, 0), |(fixed, standalone), (a, b)| {
            (fixed + a, standalone + b)
        });
    println!(
        "mixed journal ({} ops, standalone): fixed {fixed} B, compact {standalone} B ({:.1}%)",
        ops.len(),
        100.0 * standalone as f64 / fixed as f64
    );
    assert!(
        standalone * 5 < fixed * 3,
        "fixed {fixed} vs compact {standalone}"
    );
    for op in &ops {
        let decoded = Artifact::decode(&one_update_bytes(op)).expect("one-operation Update");
        assert!(matches!(decoded, Artifact::Update(update) if update.operations() == [op.clone()]));
    }

    // The canonical whole-batch Update adds a per-update site dictionary and
    // cross-operation path front-coding. The prototype payload had 4 count
    // bytes plus a 4-byte length per operation and the same envelope width.
    let update = esbt::Update::new(ops.clone()).expect("canonical update");
    let message = Artifact::Update(update.clone()).encode();
    let fixed_message = 11 + 4 + fixed + 4 * ops.len();
    println!(
        "mixed journal ({} ops, one Update artifact): fixed {fixed_message} B, canonical {} B ({:.1}%)",
        ops.len(),
        message.len(),
        100.0 * message.len() as f64 / fixed_message as f64
    );
    assert!(message.len() * 3 < fixed_message);
    assert_eq!(Artifact::decode(&message), Some(Artifact::Update(update)));
}
