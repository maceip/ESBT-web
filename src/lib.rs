//! Extended Stern–Brocot Tree sequence CRDT.
//! Faithful to Mechaoui & Imine, arXiv:2607.28101v1.

pub mod allocator;
pub mod anchor;
pub mod clock;
mod codec;
pub mod document;
pub mod error;
pub mod fraction;
pub mod limits;
pub mod newseq;
pub mod op;
pub mod rbtree;
pub mod replica;
pub mod snapshot;
pub mod update;
#[cfg(test)]
pub mod verify;
pub mod weight;

#[cfg(target_arch = "wasm32")]
pub mod wasm_abi;

pub use allocator::{AdaptiveDmaxConfig, Allocator, DMAX_HARD_CEILING};
pub use anchor::{Affinity, Anchor, AnchorRange, AnchorTarget};
pub use document::{Document, LocalUpdate, SnapshotKind, SnapshotReceipt, UndoDisposition};
pub use error::{EngineError, ErrorCode};
pub use limits::ResourceLimits;
pub use op::{Op, OpKind};
pub use replica::{Replica, ReplicaConfig, SnapshotMergeError};
pub use snapshot::{FullSnapshot, Snapshot};
pub use update::{ApplyOutcome, ApplyReceipt, OperationRef, Update};
pub use weight::Weight;
