use esbt::wire::{Artifact, ArtifactKind};

const VECTORS: &str = include_str!("golden/esbt-codec.txt");

#[test]
fn every_golden_vector_decodes_and_reencodes_byte_exactly() {
    for line in VECTORS
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (name, encoded) = line.split_once(' ').expect("name and hex");
        let bytes = decode_hex(encoded);
        let artifact = Artifact::decode(&bytes).unwrap_or_else(|| panic!("decode {name}"));
        assert_eq!(kind_name(artifact.kind()), name);
        assert_eq!(artifact.encode(), bytes, "canonical re-encode for {name}");
    }
}

#[test]
fn retired_split_envelopes_are_not_compatibility_decoded() {
    for magic in [b"ESBM", b"ESBS", b"ESBF", b"ESBA"] {
        let mut bytes = magic.to_vec();
        bytes.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0]);
        assert!(
            Artifact::decode(&bytes).is_none(),
            "retired magic {magic:?}"
        );
        assert!(
            Artifact::classify(&bytes).is_err(),
            "retired magic {magic:?}"
        );
    }
}

#[test]
fn envelope_version_length_kind_and_trailing_bytes_fail_closed() {
    let (_, encoded) = VECTORS
        .lines()
        .find(|line| line.starts_with("version "))
        .and_then(|line| line.split_once(' '))
        .expect("version vector");
    let canonical = decode_hex(encoded);

    let mut wrong_version = canonical.clone();
    wrong_version[4..6].copy_from_slice(&2_u16.to_le_bytes());
    assert!(Artifact::decode(&wrong_version).is_none());

    let mut wrong_kind = canonical.clone();
    wrong_kind[6] = 0xff;
    assert!(Artifact::decode(&wrong_kind).is_none());

    let mut wrong_length = canonical.clone();
    wrong_length[7..11].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Artifact::decode(&wrong_length).is_none());

    let mut trailing = canonical;
    trailing.push(0);
    assert!(Artifact::decode(&trailing).is_none());
}

fn kind_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Update => "update",
        ArtifactKind::CompactSnapshot => "compact-snapshot",
        ArtifactKind::FullSnapshot => "full-snapshot",
        ArtifactKind::Version => "version",
        ArtifactKind::Anchor => "anchor",
        ArtifactKind::CausalPosition => "causal-position",
    }
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0, "hex length");
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).expect("hex byte"))
        .collect()
}
