//! WebAssembly Component Model guest implementation generated from `wit/esbt.wit`.

wit_bindgen::generate!({
    path: "wit",
    world: "esbt",
    debug: true,
});

use std::cell::RefCell;

use crate::allocator::AdaptiveDmaxConfig;
use crate::anchor::{Affinity as NativeAffinity, Anchor, CausalPosition};
use crate::clock::Version;
use crate::config::DocumentConfig as NativeDocumentConfig;
use crate::document::{
    LocalUpdate as NativeLocalUpdate, SnapshotKind as NativeSnapshotKind,
    SnapshotReceipt as NativeSnapshotReceipt, UndoDisposition as NativeUndoDisposition,
};
use crate::limits::ResourceLimits as NativeResourceLimits;
use crate::newseq::AllocationStrategy as NativeAllocationStrategy;
use crate::replica::ReplicaConfig;
use crate::update::{
    ApplyOutcome as NativeApplyOutcome, ApplyReceipt as NativeApplyReceipt,
    OperationRef as NativeOperationRef, VisibleEdit as NativeVisibleEdit,
};
use crate::wire::{Artifact, ArtifactKind as NativeArtifactKind};

use exports::esbt::document::engine::{self as wit, Guest, GuestDocument};

struct Component;

struct ComponentDocument {
    inner: RefCell<crate::Document>,
}

fn map_error(error: crate::EngineError) -> wit::EngineError {
    wit::EngineError {
        code: error.code as u32,
        message: error.to_string(),
    }
}

fn native_site(site: wit::SiteId) -> crate::weight::SiteId {
    u128::from(site.low) | (u128::from(site.high) << 64)
}

fn map_site(site: crate::weight::SiteId) -> wit::SiteId {
    wit::SiteId {
        low: site as u64,
        high: (site >> 64) as u64,
    }
}

fn native_affinity(affinity: wit::Affinity) -> NativeAffinity {
    match affinity {
        wit::Affinity::Before => NativeAffinity::Before,
        wit::Affinity::After => NativeAffinity::After,
    }
}

fn map_visible_edit(edit: NativeVisibleEdit) -> wit::VisibleEdit {
    wit::VisibleEdit {
        from: u32::try_from(edit.from).expect("WIT document offset exceeds u32"),
        to: u32::try_from(edit.to).expect("WIT document offset exceeds u32"),
        inserted: edit.inserted,
    }
}

fn map_operation_ref(identity: NativeOperationRef) -> wit::OperationRef {
    wit::OperationRef {
        origin: map_site(identity.origin),
        sequence: identity.sequence,
    }
}

fn map_local_update(update: NativeLocalUpdate) -> wit::LocalChange {
    wit::LocalChange {
        update: update.canonical_bytes,
        visible_changed: update.visible_changed,
        visible_edits: update
            .visible_edits
            .into_iter()
            .map(map_visible_edit)
            .collect(),
    }
}

fn map_apply_receipt(receipt: NativeApplyReceipt) -> wit::ApplyReceipt {
    let outcome = match receipt.outcome {
        NativeApplyOutcome::Applied => wit::ApplyOutcome::Applied,
        NativeApplyOutcome::Duplicate => wit::ApplyOutcome::Duplicate,
        NativeApplyOutcome::Buffered => wit::ApplyOutcome::Buffered,
        NativeApplyOutcome::Mixed => wit::ApplyOutcome::Mixed,
        NativeApplyOutcome::Noop => wit::ApplyOutcome::Noop,
    };
    wit::ApplyReceipt {
        outcome,
        accepted_operations: receipt
            .accepted_operations
            .into_iter()
            .map(map_operation_ref)
            .collect(),
        applied_operations: receipt
            .applied_operations
            .into_iter()
            .map(map_operation_ref)
            .collect(),
        buffered_operations: receipt
            .buffered_operations
            .into_iter()
            .map(map_operation_ref)
            .collect(),
        newly_ready_operations: receipt
            .newly_ready_operations
            .into_iter()
            .map(map_operation_ref)
            .collect(),
        version: receipt.version.encode(),
        visible_changed: receipt.visible_changed,
        visible_edits: receipt
            .visible_edits
            .into_iter()
            .map(map_visible_edit)
            .collect(),
        journal: receipt.journal_bytes,
    }
}

fn map_snapshot_receipt(receipt: NativeSnapshotReceipt) -> wit::SnapshotReceipt {
    wit::SnapshotReceipt {
        kind: match receipt.kind {
            NativeSnapshotKind::Full => wit::SnapshotKind::Full,
            NativeSnapshotKind::Compact => wit::SnapshotKind::Compact,
        },
        version: receipt.version.encode(),
        visible_changed: receipt.visible_changed,
        visible_edits: receipt
            .visible_edits
            .into_iter()
            .map(map_visible_edit)
            .collect(),
        undo: match receipt.undo {
            NativeUndoDisposition::Preserved => wit::UndoDisposition::Preserved,
            NativeUndoDisposition::PartiallyPreserved => wit::UndoDisposition::PartiallyPreserved,
            NativeUndoDisposition::Cleared => wit::UndoDisposition::Cleared,
        },
    }
}

fn map_limits(limits: &NativeResourceLimits) -> wit::ResourceLimits {
    let to_u32 = |value: usize| u32::try_from(value).expect("default WIT limit exceeds u32");
    wit::ResourceLimits {
        max_message_bytes: to_u32(limits.max_message_bytes),
        max_operations_per_update: to_u32(limits.max_operations_per_update),
        max_identifier_depth: to_u32(limits.max_identifier_depth),
        max_version_sites: to_u32(limits.max_version_sites),
        max_sparse_receipts: to_u32(limits.max_sparse_receipts),
        max_snapshot_items: to_u32(limits.max_snapshot_items),
        max_pending_operations: to_u32(limits.max_pending_operations),
        max_deferred_deletes: to_u32(limits.max_deferred_deletes),
        max_document_units: to_u32(limits.max_document_units),
        max_allocation_attempts: to_u32(limits.max_allocation_attempts),
        max_retained_operations: to_u32(limits.max_retained_operations),
        max_undo_transactions: to_u32(limits.max_undo_transactions),
    }
}

fn native_limits(limits: wit::ResourceLimits) -> NativeResourceLimits {
    NativeResourceLimits {
        max_message_bytes: limits.max_message_bytes as usize,
        max_operations_per_update: limits.max_operations_per_update as usize,
        max_identifier_depth: limits.max_identifier_depth as usize,
        max_version_sites: limits.max_version_sites as usize,
        max_sparse_receipts: limits.max_sparse_receipts as usize,
        max_snapshot_items: limits.max_snapshot_items as usize,
        max_pending_operations: limits.max_pending_operations as usize,
        max_deferred_deletes: limits.max_deferred_deletes as usize,
        max_document_units: limits.max_document_units as usize,
        max_allocation_attempts: limits.max_allocation_attempts as usize,
        max_retained_operations: limits.max_retained_operations as usize,
        max_undo_transactions: limits.max_undo_transactions as usize,
    }
}

fn map_config(config: &NativeDocumentConfig) -> wit::DocumentConfig {
    let (kind, boundary) = match config.replica.strategy {
        NativeAllocationStrategy::Midpoint => (wit::AllocationStrategyKind::Midpoint, 0),
        NativeAllocationStrategy::BoundaryLow(value) => {
            (wit::AllocationStrategyKind::BoundaryLow, value)
        }
        NativeAllocationStrategy::BoundaryHigh(value) => {
            (wit::AllocationStrategyKind::BoundaryHigh, value)
        }
        NativeAllocationStrategy::AlternatingByDepth(value) => {
            (wit::AllocationStrategyKind::AlternatingByDepth, value)
        }
    };
    wit::DocumentConfig {
        dmax: u32::try_from(config.replica.dmax).expect("default Dmax exceeds u32"),
        base: config.replica.base,
        depth: config.replica.depth,
        strategy: wit::AllocationStrategy { kind, boundary },
        adaptive_dmax: config
            .replica
            .adaptive_dmax
            .map(|adaptive| wit::AdaptiveDmaxConfig {
                floor: u32::try_from(adaptive.floor).expect("adaptive floor exceeds u32"),
                ceiling: u32::try_from(adaptive.ceiling).expect("adaptive ceiling exceeds u32"),
                window: adaptive.window,
                holdoff_windows: adaptive.holdoff_windows,
            }),
        limits: map_limits(&config.limits),
    }
}

fn native_config(config: wit::DocumentConfig) -> Result<NativeDocumentConfig, wit::EngineError> {
    let strategy = match config.strategy.kind {
        wit::AllocationStrategyKind::Midpoint => {
            if config.strategy.boundary != 0 {
                return Err(map_error(crate::EngineError::new(
                    crate::ErrorCode::InvalidOperation,
                    "midpoint strategy requires a zero boundary",
                )));
            }
            NativeAllocationStrategy::Midpoint
        }
        wit::AllocationStrategyKind::BoundaryLow => {
            NativeAllocationStrategy::BoundaryLow(config.strategy.boundary)
        }
        wit::AllocationStrategyKind::BoundaryHigh => {
            NativeAllocationStrategy::BoundaryHigh(config.strategy.boundary)
        }
        wit::AllocationStrategyKind::AlternatingByDepth => {
            NativeAllocationStrategy::AlternatingByDepth(config.strategy.boundary)
        }
    };
    let adaptive_dmax = config.adaptive_dmax.map(|adaptive| AdaptiveDmaxConfig {
        floor: i64::from(adaptive.floor),
        ceiling: i64::from(adaptive.ceiling),
        window: adaptive.window,
        holdoff_windows: adaptive.holdoff_windows,
    });
    let native = NativeDocumentConfig {
        replica: ReplicaConfig {
            dmax: i64::from(config.dmax),
            base: config.base,
            depth: config.depth,
            adaptive_dmax,
            strategy,
        },
        limits: native_limits(config.limits),
    };
    native.validate().map_err(map_error)?;
    Ok(native)
}

fn map_artifact_kind(kind: NativeArtifactKind) -> wit::ArtifactKind {
    match kind {
        NativeArtifactKind::Update => wit::ArtifactKind::Update,
        NativeArtifactKind::CompactSnapshot => wit::ArtifactKind::CompactSnapshot,
        NativeArtifactKind::FullSnapshot => wit::ArtifactKind::FullSnapshot,
        NativeArtifactKind::Version => wit::ArtifactKind::Version,
        NativeArtifactKind::Anchor => wit::ArtifactKind::Anchor,
        NativeArtifactKind::CausalPosition => wit::ArtifactKind::CausalPosition,
    }
}

fn bounded_artifact(
    bytes: Vec<u8>,
    limits: &NativeResourceLimits,
    label: &'static str,
) -> Result<Vec<u8>, wit::EngineError> {
    if bytes.len() > limits.max_message_bytes {
        return Err(map_error(crate::EngineError::new(
            crate::ErrorCode::MessageTooLarge,
            label,
        )));
    }
    Ok(bytes)
}

impl Guest for Component {
    type Document = ComponentDocument;

    fn default_config() -> wit::DocumentConfig {
        map_config(&NativeDocumentConfig::default())
    }

    fn default_adaptive_dmax_config() -> wit::AdaptiveDmaxConfig {
        let adaptive = AdaptiveDmaxConfig::default();
        wit::AdaptiveDmaxConfig {
            floor: u32::try_from(adaptive.floor).expect("adaptive floor exceeds u32"),
            ceiling: u32::try_from(adaptive.ceiling).expect("adaptive ceiling exceeds u32"),
            window: adaptive.window,
            holdoff_windows: adaptive.holdoff_windows,
        }
    }

    fn create(
        site: wit::SiteId,
        config: wit::DocumentConfig,
    ) -> Result<wit::Document, wit::EngineError> {
        let config = native_config(config)?;
        let document = crate::Document::new(native_site(site), config.replica, config.limits)
            .map_err(map_error)?;
        Ok(wit::Document::new(ComponentDocument {
            inner: RefCell::new(document),
        }))
    }

    fn wire_version() -> u16 {
        crate::WIRE_FORMAT_VERSION
    }

    fn empty_version() -> Vec<u8> {
        Version::default().encode()
    }

    fn classify_artifact(artifact: Vec<u8>) -> Result<wit::ArtifactKind, wit::EngineError> {
        Artifact::classify(&artifact)
            .map(map_artifact_kind)
            .map_err(map_error)
    }

    fn version_covers(version: Vec<u8>, expected: Vec<u8>) -> Result<bool, wit::EngineError> {
        let limits = NativeResourceLimits::wire_default();
        let version = Version::decode_with_limits(&version, &limits).map_err(map_error)?;
        let expected = Version::decode_with_limits(&expected, &limits).map_err(map_error)?;
        Ok(version.covers(&expected))
    }
}

impl GuestDocument for ComponentDocument {
    fn site(&self) -> wit::SiteId {
        map_site(self.inner.borrow().site())
    }

    fn length(&self) -> u32 {
        u32::try_from(self.inner.borrow().len()).expect("WIT document length exceeds u32")
    }

    fn text(&self) -> Vec<u16> {
        self.inner.borrow().utf16_units()
    }

    fn state_hash(&self) -> u64 {
        self.inner.borrow().state_hash()
    }

    fn pending_operations(&self) -> u32 {
        u32::try_from(self.inner.borrow().pending_len()).expect("WIT pending count exceeds u32")
    }

    fn retained_operations(&self) -> u32 {
        u32::try_from(self.inner.borrow().retained_operations())
            .expect("WIT retained count exceeds u32")
    }

    fn current_dmax(&self) -> u32 {
        u32::try_from(self.inner.borrow().current_dmax()).expect("WIT Dmax exceeds u32")
    }

    fn version(&self) -> Vec<u8> {
        self.inner.borrow().version().encode()
    }

    fn history_floor(&self) -> Vec<u8> {
        self.inner.borrow().history_floor().encode()
    }

    fn begin_transaction(&self, undo_group: Option<u64>) -> Result<(), wit::EngineError> {
        self.inner
            .borrow_mut()
            .begin_transaction(undo_group)
            .map_err(map_error)
    }

    fn commit_transaction(&self) -> Result<Option<wit::LocalChange>, wit::EngineError> {
        self.inner
            .borrow_mut()
            .commit_transaction()
            .map(|update| update.map(map_local_update))
            .map_err(map_error)
    }

    fn abort_transaction(&self) -> Result<(), wit::EngineError> {
        self.inner
            .borrow_mut()
            .abort_transaction()
            .map_err(map_error)
    }

    fn replace(
        &self,
        from: u32,
        to: u32,
        inserted: Vec<u16>,
        undo_group: Option<u64>,
    ) -> Result<Option<wit::LocalChange>, wit::EngineError> {
        self.inner
            .borrow_mut()
            .replace_range_utf16(from as usize, to as usize, &inserted, undo_group)
            .map(|update| update.map(map_local_update))
            .map_err(map_error)
    }

    fn insert_at_anchor(
        &self,
        anchor: Vec<u8>,
        inserted: Vec<u16>,
        undo_group: Option<u64>,
    ) -> Result<wit::AnchoredInsert, wit::EngineError> {
        let mut document = self.inner.borrow_mut();
        let anchor = Anchor::decode_with_limits(&anchor, document.limits()).map_err(map_error)?;
        let (change, anchor) = document
            .insert_utf16_at_anchor(&anchor, &inserted, undo_group)
            .map_err(map_error)?;
        Ok(wit::AnchoredInsert {
            change: change.map(map_local_update),
            anchor: anchor.encode(),
        })
    }

    fn apply_update(&self, update: Vec<u8>) -> Result<wit::ApplyReceipt, wit::EngineError> {
        self.inner
            .borrow_mut()
            .apply_bytes(&update)
            .map(map_apply_receipt)
            .map_err(map_error)
    }

    fn export_update(&self, remote_version: Vec<u8>) -> Result<Vec<u8>, wit::EngineError> {
        let document = self.inner.borrow();
        let version =
            Version::decode_with_limits(&remote_version, document.limits()).map_err(map_error)?;
        document.export_update(&version).map_err(map_error)
    }

    fn export_compact_snapshot(&self) -> Result<Vec<u8>, wit::EngineError> {
        self.inner
            .borrow()
            .export_compact_snapshot()
            .map_err(map_error)
    }

    fn export_full_snapshot(&self) -> Result<Vec<u8>, wit::EngineError> {
        self.inner
            .borrow()
            .export_full_snapshot()
            .map_err(map_error)
    }

    fn apply_snapshot(&self, snapshot: Vec<u8>) -> Result<wit::SnapshotReceipt, wit::EngineError> {
        self.inner
            .borrow_mut()
            .apply_snapshot_bytes(&snapshot)
            .map(map_snapshot_receipt)
            .map_err(map_error)
    }

    fn anchor(&self, index: u32, affinity: wit::Affinity) -> Result<Vec<u8>, wit::EngineError> {
        let document = self.inner.borrow();
        let anchor = document
            .anchor(index as usize, native_affinity(affinity))
            .map_err(map_error)?;
        bounded_artifact(
            anchor.encode(),
            document.limits(),
            "anchor exceeds the document message limit",
        )
    }

    fn resolve_anchor(&self, anchor: Vec<u8>) -> Result<u32, wit::EngineError> {
        let document = self.inner.borrow();
        let anchor = Anchor::decode_with_limits(&anchor, document.limits()).map_err(map_error)?;
        Ok(u32::try_from(document.resolve_anchor(&anchor)).expect("WIT anchor offset exceeds u32"))
    }

    fn capture_causal_position(
        &self,
        index: u32,
        affinity: wit::Affinity,
    ) -> Result<Vec<u8>, wit::EngineError> {
        let document = self.inner.borrow();
        let position = document
            .capture_causal_position(index as usize, native_affinity(affinity))
            .map_err(map_error)?;
        bounded_artifact(
            position.encode(),
            document.limits(),
            "causal position exceeds the document message limit",
        )
    }

    fn resolve_causal_position(&self, position: Vec<u8>) -> Result<Option<u32>, wit::EngineError> {
        let document = self.inner.borrow();
        let position =
            CausalPosition::decode_with_limits(&position, document.limits()).map_err(map_error)?;
        Ok(document
            .resolve_causal_position(&position)
            .map(|index| u32::try_from(index).expect("WIT causal position exceeds u32")))
    }

    fn prune_history_through(&self, version: Vec<u8>) -> Result<u32, wit::EngineError> {
        let mut document = self.inner.borrow_mut();
        let version =
            Version::decode_with_limits(&version, document.limits()).map_err(map_error)?;
        document
            .prune_history_through(&version)
            .map(|count| u32::try_from(count).expect("WIT prune count exceeds u32"))
            .map_err(map_error)
    }

    fn can_undo(&self) -> bool {
        self.inner.borrow().can_undo()
    }

    fn can_redo(&self) -> bool {
        self.inner.borrow().can_redo()
    }

    fn undo(&self) -> Result<Option<wit::LocalChange>, wit::EngineError> {
        self.inner
            .borrow_mut()
            .undo()
            .map(|update| update.map(map_local_update))
            .map_err(map_error)
    }

    fn redo(&self) -> Result<Option<wit::LocalChange>, wit::EngineError> {
        self.inner
            .borrow_mut()
            .redo()
            .map(|update| update.map(map_local_update))
            .map_err(map_error)
    }
}

export!(Component);
