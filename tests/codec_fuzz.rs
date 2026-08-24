//! Deterministic fuzz regression for the complete artifact dispatcher.

use esbt::wire::Artifact;
use esbt::{ErrorCode, ResourceLimits, Update};

const VECTORS: &str = include_str!("golden/esbt-codec.txt");

#[test]
fn golden_mutations_and_truncations_never_panic_or_decode_noncanonically() {
    for (_, bytes) in vectors() {
        for end in 0..bytes.len() {
            assert_safe(&bytes[..end]);
        }
        for index in 0..bytes.len() {
            for bit in 0..8 {
                let mut mutated = bytes.clone();
                mutated[index] ^= 1 << bit;
                assert_safe(&mutated);
            }
        }
        let mut trailing = bytes;
        trailing.extend_from_slice(&[0, 0xff, 0x80]);
        assert_safe(&trailing);
    }
}

#[test]
fn seeded_arbitrary_inputs_never_panic_and_success_is_byte_canonical() {
    let mut state = 0xd1b5_4a32_d192_ed03_u64;
    for iteration in 0..20_000usize {
        let length = if iteration % 16 == 0 {
            iteration % 8_192
        } else {
            next(&mut state) as usize % 1_024
        };
        let mut bytes = vec![0_u8; length];
        for byte in &mut bytes {
            *byte = next(&mut state) as u8;
        }
        assert_safe(&bytes);
    }
}

#[test]
fn nonminimal_varints_are_rejected_as_noncanonical() {
    let canonical = Artifact::Update(Update::default()).encode();
    // Empty Update payload is two one-byte zero varints. Expand the first zero
    // to the nonminimal LEB128 form 0x80 0x00 and repair only the envelope
    // length; a permissive decoder would otherwise describe the same value.
    let mut nonminimal = canonical[..11].to_vec();
    nonminimal[7..11].copy_from_slice(&3_u32.to_le_bytes());
    nonminimal.extend_from_slice(&[0x80, 0x00, 0x00]);
    let error = Artifact::decode_with_limits(&nonminimal, &ResourceLimits::default())
        .expect_err("nonminimal varint");
    assert_eq!(error.code, ErrorCode::NonCanonicalEncoding);
}

fn assert_safe(bytes: &[u8]) {
    let result = std::panic::catch_unwind(|| {
        let _ = Artifact::classify(bytes);
        Artifact::decode_with_limits(bytes, &ResourceLimits::default())
    });
    let decoded = result.expect("artifact parser must not panic");
    if let Ok(artifact) = decoded {
        assert_eq!(
            artifact.encode(),
            bytes,
            "accepted bytes must already be canonical"
        );
    }
}

fn vectors() -> Vec<(&'static str, Vec<u8>)> {
    VECTORS
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, hex) = line.split_once(' ').expect("name and hex");
            (name, decode_hex(hex))
        })
        .collect()
}

fn decode_hex(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).expect("hex byte"))
        .collect()
}

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}
