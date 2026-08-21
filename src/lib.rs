//! Extended Stern–Brocot Tree sequence CRDT.
//! Faithful to Mechaoui & Imine, arXiv:2607.28101v1.

pub mod allocator;
pub mod clock;
pub mod fraction;
pub mod newseq;
pub mod op;
pub mod rbtree;
pub mod replica;
pub mod snapshot;
pub mod verify;
pub mod weight;

#[cfg(target_arch = "wasm32")]
pub mod wasm_abi;

pub use allocator::Allocator;
pub use op::{Op, OpKind};
pub use replica::{Replica, ReplicaConfig};
pub use snapshot::Snapshot;
pub use weight::Weight;
