//! Extension 3 evaluation: replay real-world and adversarial editing
//! workloads through the engine under each pluggable allocation strategy.
//!
//! ```text
//! cargo run --release --example trace_replay              # synthetic suite
//! cargo run --release --example trace_replay -- edits.json
//! ```
//!
//! The optional argument is an automerge-perf style editing trace: a JSON
//! array of `[position, delete_count, inserted_chars...]` entries. That
//! corpus (a real keystroke-by-keystroke LaTeX paper) is the workload the
//! Yjs/Automerge community benchmarks with, which is what the paper's
//! "evaluate under real-world collaborative workloads" asks for. The file is
//! not vendored here; download `edit-by-index/editing-trace.js` from the
//! automerge-perf repository and strip the JavaScript wrapper.
//!
//! Reported per strategy: replay time, final document length, encoded
//! journal bytes, compact snapshot bytes, and sequence-path depth stats.

use esbt::replica::{Replica, ReplicaConfig};
use esbt::{AdaptiveDmaxConfig, AllocationStrategy, Artifact, Op, Update};
use std::time::Instant;

#[derive(Clone, Debug)]
enum Edit {
    Insert { position: usize, unit: u16 },
    Delete { position: usize, count: usize },
}

/// Minimal exact parser for the restricted trace grammar: arrays,
/// non-negative integers, and JSON strings.
mod trace_json {
    #[derive(Debug)]
    pub enum Value {
        Number(u64),
        Text(String),
        Array(Vec<Value>),
    }

    pub fn parse(input: &str) -> Result<Value, String> {
        let bytes = input.as_bytes();
        let mut offset = 0usize;
        let value = parse_value(bytes, &mut offset)?;
        skip_whitespace(bytes, &mut offset);
        if offset != bytes.len() {
            return Err(format!("trailing bytes at {offset}"));
        }
        Ok(value)
    }

    fn skip_whitespace(bytes: &[u8], offset: &mut usize) {
        while bytes
            .get(*offset)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            *offset += 1;
        }
    }

    fn parse_value(bytes: &[u8], offset: &mut usize) -> Result<Value, String> {
        skip_whitespace(bytes, offset);
        match bytes.get(*offset) {
            Some(b'[') => {
                *offset += 1;
                let mut items = Vec::new();
                loop {
                    skip_whitespace(bytes, offset);
                    if bytes.get(*offset) == Some(&b']') {
                        *offset += 1;
                        return Ok(Value::Array(items));
                    }
                    if !items.is_empty() {
                        if bytes.get(*offset) != Some(&b',') {
                            return Err(format!("expected comma at {offset}"));
                        }
                        *offset += 1;
                    }
                    items.push(parse_value(bytes, offset)?);
                }
            }
            Some(b'"') => {
                *offset += 1;
                let mut text = String::new();
                loop {
                    match bytes.get(*offset) {
                        Some(b'"') => {
                            *offset += 1;
                            return Ok(Value::Text(text));
                        }
                        Some(b'\\') => {
                            *offset += 1;
                            let escape =
                                bytes.get(*offset).ok_or("truncated escape".to_string())?;
                            *offset += 1;
                            match escape {
                                b'"' => text.push('"'),
                                b'\\' => text.push('\\'),
                                b'/' => text.push('/'),
                                b'n' => text.push('\n'),
                                b't' => text.push('\t'),
                                b'r' => text.push('\r'),
                                b'b' => text.push('\u{8}'),
                                b'f' => text.push('\u{c}'),
                                b'u' => {
                                    let hex = bytes
                                        .get(*offset..*offset + 4)
                                        .ok_or("truncated \\u escape".to_string())?;
                                    *offset += 4;
                                    let code = u16::from_str_radix(
                                        std::str::from_utf8(hex).map_err(|e| e.to_string())?,
                                        16,
                                    )
                                    .map_err(|e| e.to_string())?;
                                    text.push(
                                        char::from_u32(u32::from(code))
                                            .ok_or("surrogate escapes unsupported".to_string())?,
                                    );
                                }
                                other => return Err(format!("unknown escape {other}")),
                            }
                        }
                        Some(&byte) if byte < 0x80 => {
                            text.push(byte as char);
                            *offset += 1;
                        }
                        Some(_) => {
                            // Multi-byte UTF-8: take the full scalar.
                            let rest = std::str::from_utf8(&bytes[*offset..])
                                .map_err(|e| e.to_string())?;
                            let ch = rest.chars().next().ok_or("empty".to_string())?;
                            text.push(ch);
                            *offset += ch.len_utf8();
                        }
                        None => return Err("unterminated string".to_string()),
                    }
                }
            }
            Some(byte) if byte.is_ascii_digit() => {
                let mut value = 0u64;
                while let Some(&digit) = bytes.get(*offset) {
                    if !digit.is_ascii_digit() {
                        break;
                    }
                    value = value
                        .checked_mul(10)
                        .and_then(|v| v.checked_add(u64::from(digit - b'0')))
                        .ok_or("number overflow".to_string())?;
                    *offset += 1;
                }
                Ok(Value::Number(value))
            }
            other => Err(format!("unexpected token {other:?} at {offset}")),
        }
    }
}

fn load_trace(path: &str) -> Vec<Edit> {
    let raw = std::fs::read_to_string(path).expect("read trace file");
    let trace_json::Value::Array(entries) = trace_json::parse(&raw).expect("parse trace") else {
        panic!("trace root is not an array");
    };
    let mut edits = Vec::new();
    for entry in entries {
        let trace_json::Value::Array(fields) = entry else {
            panic!("trace entry is not an array");
        };
        let trace_json::Value::Number(position) = fields[0] else {
            panic!("position is not a number");
        };
        let trace_json::Value::Number(deletes) = fields[1] else {
            panic!("delete count is not a number");
        };
        if deletes > 0 {
            edits.push(Edit::Delete {
                position: position as usize,
                count: deletes as usize,
            });
        }
        let mut offset = 0usize;
        for field in &fields[2..] {
            let trace_json::Value::Text(text) = field else {
                panic!("inserted content is not a string");
            };
            for unit in text.encode_utf16() {
                edits.push(Edit::Insert {
                    position: position as usize + offset,
                    unit,
                });
                offset += 1;
            }
        }
    }
    edits
}

fn synthetic(pattern: &str, operations: usize) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut len = 0usize;
    let mut state = 0x9e37_79b9u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    for step in 0..operations {
        let unit = u16::from(b'a' + (step % 26) as u8);
        let position = match pattern {
            "append" => len,
            "prepend" => 0,
            "middle" => len / 2,
            "random-mixed" => {
                if len > 0 && next() % 5 == 0 {
                    edits.push(Edit::Delete {
                        position: next() as usize % len,
                        count: 1,
                    });
                    len -= 1;
                    continue;
                }
                next() as usize % (len + 1)
            }
            other => panic!("unknown pattern {other}"),
        };
        edits.push(Edit::Insert { position, unit });
        len += 1;
    }
    edits
}

struct Outcome {
    name: &'static str,
    elapsed_ms: u128,
    final_len: usize,
    journal_bytes: usize,
    snapshot_bytes: usize,
    mean_path_depth: f64,
    max_path_depth: usize,
    final_dmax: i64,
}

fn replay(name: &'static str, config: ReplicaConfig, edits: &[Edit]) -> Outcome {
    let mut replica = Replica::new(1, config);
    let mut journal_bytes = 0usize;
    let started = Instant::now();
    for edit in edits {
        match *edit {
            Edit::Insert { position, unit } => {
                journal_bytes += encoded_update_len(replica.local_insert(position, unit));
            }
            Edit::Delete { position, count } => {
                for op in replica.local_delete_range(position, count) {
                    journal_bytes += encoded_update_len(op);
                }
            }
        }
    }
    let elapsed_ms = started.elapsed().as_millis();
    let snapshot = replica.snapshot();
    let depths: Vec<usize> = snapshot
        .atoms
        .iter()
        .map(|atom| atom.weight.sc.len())
        .collect();
    Outcome {
        name,
        elapsed_ms,
        final_len: replica.len(),
        journal_bytes,
        snapshot_bytes: snapshot.encode().len(),
        mean_path_depth: depths.iter().sum::<usize>() as f64 / depths.len().max(1) as f64,
        max_path_depth: depths.iter().copied().max().unwrap_or(0),
        final_dmax: replica.alloc.current_dmax(),
    }
}

fn encoded_update_len(operation: Op) -> usize {
    Artifact::Update(Update::new(vec![operation]).expect("local operation"))
        .encode()
        .len()
}

fn run_workload(label: &str, edits: &[Edit]) {
    println!("\n== workload: {label} ({} edits) ==", edits.len());
    println!(
        "{:<24} {:>8} {:>10} {:>12} {:>12} {:>10} {:>8} {:>10}",
        "strategy", "ms", "doc units", "journal B", "snapshot B", "mean sc", "max sc", "dmax"
    );
    let strategies: [(&'static str, AllocationStrategy, Option<AdaptiveDmaxConfig>); 5] = [
        ("midpoint (paper)", AllocationStrategy::Midpoint, None),
        (
            "boundary-low(64)",
            AllocationStrategy::BoundaryLow(64),
            None,
        ),
        (
            "boundary-high(64)",
            AllocationStrategy::BoundaryHigh(64),
            None,
        ),
        (
            "alternating(64)",
            AllocationStrategy::AlternatingByDepth(64),
            None,
        ),
        (
            "midpoint + adaptive",
            AllocationStrategy::Midpoint,
            Some(AdaptiveDmaxConfig::default()),
        ),
    ];
    for (name, strategy, adaptive_dmax) in strategies {
        let config = ReplicaConfig {
            strategy,
            adaptive_dmax,
            ..Default::default()
        };
        let outcome = replay(name, config, edits);
        println!(
            "{:<24} {:>8} {:>10} {:>12} {:>12} {:>10.2} {:>8} {:>10}",
            outcome.name,
            outcome.elapsed_ms,
            outcome.final_len,
            outcome.journal_bytes,
            outcome.snapshot_bytes,
            outcome.mean_path_depth,
            outcome.max_path_depth,
            outcome.final_dmax,
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    if let Some(path) = args.next() {
        let edits = load_trace(&path);
        run_workload(&format!("real-world trace {path}"), &edits);
        return;
    }
    for pattern in ["append", "prepend", "middle", "random-mixed"] {
        let edits = synthetic(pattern, 10_000);
        run_workload(pattern, &edits);
    }
}
