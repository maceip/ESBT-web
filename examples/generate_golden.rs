//! Print the six deterministic codec vectors checked into `tests/golden`.

use esbt::anchor::{Affinity, Anchor, AnchorTarget, CausalPosition};
use esbt::clock::Version;
use esbt::fraction::Fraction;
use esbt::op::Op;
use esbt::snapshot::{Atom, FullSnapshot, Snapshot};
use esbt::wire::Artifact;
use esbt::{Update, Weight};

fn main() {
    let site = 0x0102_0304_0506_0708_1112_1314_1516_1718_u128;
    let weight = Weight::new(Fraction::new(1, 2), 0, vec![0], site);
    let operation = Op::ins(weight.clone(), u16::from(b'A'), 1, site, 1);
    let update = Update::new(vec![operation.clone()]).expect("golden update");

    let mut version = Version::default();
    version.note(site, 1);
    let snapshot = Snapshot {
        atoms: vec![Atom {
            weight: weight.clone(),
            unit: u16::from(b'A'),
            counter: 1,
        }],
        delete_log: Vec::new(),
        version: version.clone(),
        insertions: version.clone(),
    };
    let full = FullSnapshot::new(
        snapshot.clone(),
        Version::default(),
        vec![operation],
        Vec::new(),
    )
    .expect("golden full snapshot");
    let anchor = Anchor {
        target: AnchorTarget::Item { weight, counter: 1 },
        affinity: Affinity::After,
    };
    let causal = CausalPosition::new(version.clone(), anchor.clone());

    let vectors = [
        ("update", Artifact::Update(update)),
        ("compact-snapshot", Artifact::CompactSnapshot(snapshot)),
        ("full-snapshot", Artifact::FullSnapshot(full)),
        ("version", Artifact::Version(version)),
        ("anchor", Artifact::Anchor(anchor)),
        ("causal-position", Artifact::CausalPosition(causal)),
    ];
    println!("# name canonical-hex");
    for (name, artifact) in vectors {
        println!("{name} {}", hex(&artifact.encode()));
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
