//! Extension 2 measurements: encoded identifier storage, format v1 vs v2.
//!
//! Format v1 had a closed-form size (fixed-width fields), so the comparison
//! reconstructs v1 byte counts exactly from the same weights instead of
//! keeping a legacy encoder alive.

use esbt::replica::{Replica, ReplicaConfig};
use esbt::{Op, OpKind, Weight};

/// v1 weight bytes: p:i64 + q:i64 + sn:i64 + site:u128 + len:u16 + 4·sc.
fn v1_weight_bytes(weight: &Weight) -> usize {
    8 + 8 + 8 + 16 + 2 + 4 * weight.sc.len()
}

/// v1 operation bytes: tag + origin:u128 + seq:u64 + c:u64 + weight (+ unit).
fn v1_op_bytes(op: &Op) -> usize {
    1 + 16
        + 8
        + 8
        + v1_weight_bytes(&op.weight)
        + if matches!(op.kind, OpKind::Ins { .. }) {
            2
        } else {
            0
        }
}

/// v1 snapshot bytes: magic + version + length-prefixed insertion and
/// version summaries + u32-counted atom and delete lists.
fn v1_snapshot_bytes(snapshot: &esbt::Snapshot) -> usize {
    let mut total = 4 + 2;
    total += 4 + snapshot.insertions.encode().len();
    total += 4;
    for atom in &snapshot.atoms {
        total += v1_weight_bytes(&atom.weight) + 2 + 8;
    }
    total += 4;
    for (weight, _) in &snapshot.delete_log {
        total += v1_weight_bytes(weight) + 8;
    }
    total + 4 + snapshot.version.encode().len()
}

fn cfg() -> ReplicaConfig {
    ReplicaConfig::default()
}

#[test]
fn minimal_insertion_operation_shrinks() {
    let mut replica = Replica::new(3, cfg());
    let op = replica.local_insert(0, 'A' as u16);
    let v1 = v1_op_bytes(&op);
    let v2 = op.encode().len();
    println!("minimal insertion op: v1 {v1} B, v2 {v2} B");
    assert!(v2 * 2 < v1, "v1 {v1} vs v2 {v2}");
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
    let v1 = v1_snapshot_bytes(&snapshot);
    let v2 = snapshot.encode().len();
    println!(
        "two-site typing-run snapshot ({} units): v1 {v1} B, v2 {v2} B ({:.1}%)",
        a.len(),
        100.0 * v2 as f64 / v1 as f64
    );
    assert!(v2 * 3 < v1, "v1 {v1} vs v2 {v2}");
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
    let (v1, v2): (usize, usize) = ops
        .iter()
        .map(|op| (v1_op_bytes(op), op.encode().len()))
        .fold((0, 0), |(v1, v2), (a, b)| (v1 + a, v2 + b));
    println!(
        "mixed journal ({} ops, standalone): v1 {v1} B, v2 {v2} B ({:.1}%)",
        ops.len(),
        100.0 * v2 as f64 / v1 as f64
    );
    assert!(v2 * 2 < v1, "v1 {v1} vs v2 {v2}");
    for op in &ops {
        assert_eq!(Op::decode(&op.encode()).as_ref(), Some(op));
    }

    // Format v3: the whole batch as one update message — the per-update site
    // dictionary and cross-operation path front-coding on top of varints.
    // The v1 payload was 4 count bytes plus a 4-byte length per operation,
    // and 11 envelope bytes on both sides.
    let update = esbt::Update::new(ops.clone()).expect("canonical update");
    let message = esbt::snapshot::Message::Update(update.clone()).encode();
    let v1_message = 11 + 4 + v1 + 4 * ops.len();
    println!(
        "mixed journal ({} ops, one v3 update message): v1 {v1_message} B, v3 {} B ({:.1}%)",
        ops.len(),
        message.len(),
        100.0 * message.len() as f64 / v1_message as f64
    );
    assert!(message.len() * 3 < v1_message);
    assert_eq!(
        esbt::snapshot::Message::decode(&message),
        Some(esbt::snapshot::Message::Update(update))
    );
}
