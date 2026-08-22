//! Stateful production API layered over the paper-level `Replica`.

use crate::anchor::{Affinity, Anchor};
use crate::clock::Version;
use crate::error::{EngineError, ErrorCode};
use crate::limits::ResourceLimits;
use crate::op::{Op, OpKind};
use crate::replica::{LocalTransactionCheckpoint, Replica, ReplicaConfig, SnapshotMergeError};
use crate::snapshot::{FullSnapshot, Message, Snapshot};
use crate::update::{ApplyOutcome, ApplyReceipt, OperationRef, Update};
use crate::weight::{SiteId, Weight};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalUpdate {
    /// Canonical decoded value, useful to native callers that do not need to
    /// parse their own journal bytes.
    pub update: Update,
    /// Exact retry-safe `ESBM` bytes emitted once after the local state commits.
    pub canonical_bytes: Vec<u8>,
    pub visible_changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotKind {
    Full,
    Compact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoDisposition {
    Preserved,
    PartiallyPreserved,
    Cleared,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotReceipt {
    pub kind: SnapshotKind,
    pub version: Version,
    pub visible_changed: bool,
    pub undo: UndoDisposition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransactionMode {
    Normal,
    Undo,
    Redo,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum UndoAction {
    DeleteInserted {
        weight: Weight,
        counter: u64,
    },
    RestoreDeleted {
        weight: Weight,
        unit: u16,
        deleted_counter: u64,
        deletion: OperationRef,
    },
}

#[derive(Clone)]
struct TransactionState {
    checkpoint: LocalTransactionCheckpoint,
    operations: Vec<Op>,
    undo_actions: Vec<UndoAction>,
    undo_group: Option<u64>,
    mode: TransactionMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UndoRecord {
    actions: Vec<UndoAction>,
    group: Option<u64>,
}

fn normalize_undo_actions(actions: Vec<UndoAction>) -> Vec<UndoAction> {
    let mut inserted = HashMap::<(Weight, u64), usize>::new();
    let mut normalized = Vec::<Option<UndoAction>>::with_capacity(actions.len());

    for action in actions {
        match &action {
            UndoAction::DeleteInserted { weight, counter } => {
                inserted.insert((weight.clone(), *counter), normalized.len());
                normalized.push(Some(action));
            }
            UndoAction::RestoreDeleted {
                weight,
                deleted_counter,
                ..
            } => {
                let identity = (weight.clone(), *deleted_counter);
                if let Some(index) = inserted.remove(&identity) {
                    // The item was both created and removed inside this undo
                    // group. Neither transient step belongs in the inverse.
                    normalized[index] = None;
                } else {
                    normalized.push(Some(action));
                }
            }
        }
    }

    normalized.into_iter().flatten().collect()
}

#[derive(Clone)]
pub struct Document {
    replica: Replica,
    limits: ResourceLimits,
    transaction: Option<TransactionState>,
    undo_stack: Vec<UndoRecord>,
    redo_stack: Vec<UndoRecord>,
    /// Causally closed prefix represented by materialized state even when its
    /// individual operations have been pruned from `Replica::log`.
    history_floor: Version,
}

impl Document {
    pub fn new(
        site: SiteId,
        replica_config: ReplicaConfig,
        limits: ResourceLimits,
    ) -> Result<Self, EngineError> {
        if site == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidSiteId,
                "site zero is reserved for sentinels",
            ));
        }
        if limits.max_identifier_depth == 0
            || limits.max_operations_per_update == 0
            || limits.max_message_bytes < 16
            || limits.max_allocation_attempts == 0
        {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "resource limits disable a required engine primitive",
            ));
        }
        Ok(Self {
            replica: Replica::new(site, replica_config),
            limits,
            transaction: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            history_floor: Version::default(),
        })
    }

    pub fn with_defaults(site: SiteId) -> Result<Self, EngineError> {
        Self::new(site, ReplicaConfig::default(), ResourceLimits::default())
    }

    pub fn site(&self) -> SiteId {
        self.replica.site
    }

    pub fn len(&self) -> usize {
        self.replica.len()
    }

    pub fn is_empty(&self) -> bool {
        self.replica.is_empty()
    }

    pub fn text(&self) -> String {
        self.replica.text()
    }

    /// Exact JavaScript/CodeMirror representation, including unpaired UTF-16
    /// surrogates that cannot be represented by a Rust `String`.
    pub fn utf16_units(&self) -> Vec<u16> {
        self.replica.doc.units()
    }

    pub fn version(&self) -> Version {
        self.replica.version.clone()
    }

    pub fn history_floor(&self) -> Version {
        self.history_floor.clone()
    }

    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    pub fn pending_len(&self) -> usize {
        self.replica.pending.len()
    }

    /// Operations currently retained for reconnect and delta export. This is
    /// the quantity that grows without bound unless the product calls
    /// `prune_history_through`; expose it so clients can drive compaction.
    pub fn retained_operations(&self) -> usize {
        self.replica.log.len()
    }

    /// The `Dmax` currently in force (moves over time when the adaptive
    /// controller is enabled).
    pub fn current_dmax(&self) -> i64 {
        self.replica.alloc.current_dmax()
    }

    pub fn state_hash(&self) -> u64 {
        self.replica.hash_state()
    }

    pub fn anchor(&self, index: usize, affinity: Affinity) -> Result<Anchor, EngineError> {
        if index > self.len() {
            return Err(EngineError::new(
                ErrorCode::InvalidRange,
                "anchor index exceeds the document",
            ));
        }
        Ok(Anchor::at_index(&self.replica, index, affinity))
    }

    pub fn resolve_anchor(&self, anchor: &Anchor) -> usize {
        anchor.resolve(&self.replica)
    }

    pub fn begin_transaction(&mut self, undo_group: Option<u64>) -> Result<(), EngineError> {
        self.begin_transaction_mode(undo_group, TransactionMode::Normal)
    }

    fn begin_transaction_mode(
        &mut self,
        undo_group: Option<u64>,
        mode: TransactionMode,
    ) -> Result<(), EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "a document transaction is already active",
            ));
        }
        let checkpoint = self.replica.begin_local_transaction()?;
        self.transaction = Some(TransactionState {
            checkpoint,
            operations: Vec::new(),
            undo_actions: Vec::new(),
            undo_group,
            mode,
        });
        Ok(())
    }

    pub fn abort_transaction(&mut self) -> Result<(), EngineError> {
        let transaction = self.transaction.take().ok_or_else(|| {
            EngineError::new(ErrorCode::NoActiveTransaction, "no transaction to abort")
        })?;
        self.rollback_transaction(transaction);
        Ok(())
    }

    pub fn commit_transaction(&mut self) -> Result<Option<LocalUpdate>, EngineError> {
        self.commit_transaction_inner()
    }

    fn commit_transaction_inner(&mut self) -> Result<Option<LocalUpdate>, EngineError> {
        let transaction = self.transaction.take().ok_or_else(|| {
            EngineError::new(ErrorCode::NoActiveTransaction, "no transaction to commit")
        })?;
        if transaction.operations.is_empty() {
            self.replica.commit_local_transaction();
            return Ok(None);
        }

        let too_many_sites = self.replica.version.site_count() > self.limits.max_version_sites
            || self.replica.insertion_version.site_count() > self.limits.max_version_sites;
        if too_many_sites {
            self.rollback_transaction(transaction);
            return Err(EngineError::new(
                ErrorCode::TooManyVersionSites,
                "local edit exceeds the receipt site limit",
            ));
        }
        let too_many_sparse = self.replica.version.sparse_receipt_count()
            > self.limits.max_sparse_receipts
            || self.replica.insertion_version.sparse_receipt_count()
                > self.limits.max_sparse_receipts;
        if too_many_sparse {
            self.rollback_transaction(transaction);
            return Err(EngineError::new(
                ErrorCode::TooManySparseReceipts,
                "local edit exceeds the sparse receipt limit",
            ));
        }

        let update = match Update::new(transaction.operations.clone()) {
            Ok(update) => update,
            Err(error) => {
                self.rollback_transaction(transaction);
                return Err(error);
            }
        };
        let canonical_bytes = Message::Update(update.clone()).encode();
        if canonical_bytes.len() > self.limits.max_message_bytes {
            self.rollback_transaction(transaction);
            return Err(EngineError::new(
                ErrorCode::MessageTooLarge,
                "local transaction exceeds the message byte limit",
            ));
        }

        let touched_weights: BTreeSet<_> = transaction
            .operations
            .iter()
            .map(|operation| operation.weight.clone())
            .collect();
        let projection = self
            .replica
            .project_pending_drain(touched_weights.iter().cloned());
        let projection_error = if projection.visible_units > self.limits.max_document_units {
            Some(EngineError::new(
                ErrorCode::DocumentTooLarge,
                "committing local edits would release too much buffered text",
            ))
        } else if projection.pending_operations > self.limits.max_pending_operations {
            Some(EngineError::new(
                ErrorCode::TooManyPendingOperations,
                "committing local edits would exceed the pending causal queue",
            ))
        } else if projection.deferred_deletes > self.limits.max_deferred_deletes {
            Some(EngineError::new(
                ErrorCode::TooManyDeferredDeletes,
                "committing local edits would exceed deferred deletions",
            ))
        } else {
            None
        };
        if let Some(error) = projection_error {
            self.rollback_transaction(transaction);
            return Err(error);
        }

        let normalized_actions = normalize_undo_actions(transaction.undo_actions.clone());
        let visible_changed = !normalized_actions.is_empty();
        let record = UndoRecord {
            actions: normalized_actions,
            group: transaction.undo_group,
        };
        if !record.actions.is_empty() {
            match transaction.mode {
                TransactionMode::Normal => {
                    self.push_normal_undo(record);
                    self.redo_stack.clear();
                }
                TransactionMode::Undo => self.push_redo(record),
                TransactionMode::Redo => self.push_undo(record),
            }
        }

        self.replica.commit_local_transaction();
        self.replica.drain_weights(touched_weights.iter());

        Ok(Some(LocalUpdate {
            update,
            canonical_bytes,
            visible_changed,
        }))
    }

    fn rollback_transaction(&mut self, transaction: TransactionState) {
        for action in transaction.undo_actions.iter().rev() {
            match action {
                UndoAction::DeleteInserted { weight, counter } => {
                    if self
                        .replica
                        .doc
                        .find(weight)
                        .is_some_and(|(_, live_counter)| live_counter == *counter)
                    {
                        self.replica.doc.delete(weight);
                        self.replica.counter_map.remove(weight);
                    }
                }
                UndoAction::RestoreDeleted {
                    weight,
                    unit,
                    deleted_counter,
                    ..
                } => {
                    if !self.replica.doc.contains(weight) {
                        self.replica
                            .doc
                            .insert(weight.clone(), *unit, *deleted_counter);
                        self.replica
                            .counter_map
                            .insert(weight.clone(), *deleted_counter);
                    }
                }
            }
        }
        for operation in &transaction.operations {
            self.replica.log.remove(&(operation.origin, operation.seq));
        }
        self.replica
            .rollback_local_transaction(transaction.checkpoint);
    }

    fn push_normal_undo(&mut self, mut record: UndoRecord) {
        if record.group.is_some()
            && self
                .undo_stack
                .last()
                .is_some_and(|previous| previous.group == record.group)
        {
            if let Some(previous) = self.undo_stack.last_mut() {
                previous.actions.append(&mut record.actions);
                previous.actions = normalize_undo_actions(std::mem::take(&mut previous.actions));
            }
        } else {
            self.push_undo(record);
        }
    }

    fn push_undo(&mut self, record: UndoRecord) {
        self.undo_stack.push(record);
        if self.undo_stack.len() > self.limits.max_undo_transactions {
            let excess = self.undo_stack.len() - self.limits.max_undo_transactions;
            self.undo_stack.drain(0..excess);
        }
    }

    fn push_redo(&mut self, record: UndoRecord) {
        self.redo_stack.push(record);
        if self.redo_stack.len() > self.limits.max_undo_transactions {
            let excess = self.redo_stack.len() - self.limits.max_undo_transactions;
            self.redo_stack.drain(0..excess);
        }
    }

    /// Insert text at a UTF-16 boundary. When no explicit transaction is
    /// active, this method creates and commits one transaction.
    pub fn insert(
        &mut self,
        index: usize,
        text: &str,
        undo_group: Option<u64>,
    ) -> Result<Option<LocalUpdate>, EngineError> {
        let units: Vec<_> = text.encode_utf16().collect();
        self.insert_utf16(index, &units, undo_group)
    }

    pub fn insert_utf16(
        &mut self,
        index: usize,
        units: &[u16],
        undo_group: Option<u64>,
    ) -> Result<Option<LocalUpdate>, EngineError> {
        let auto = self.transaction.is_none();
        if auto {
            self.begin_transaction(undo_group)?;
        }
        let result = self.insert_units(index, units.iter().copied());
        self.finish_auto(auto, result)
    }

    /// Insert at a stable caret and return the caret immediately after the new
    /// run. Keeping this anchor across remote updates prevents raw-index drift.
    pub fn insert_at_anchor(
        &mut self,
        anchor: &Anchor,
        text: &str,
        undo_group: Option<u64>,
    ) -> Result<(Option<LocalUpdate>, Anchor), EngineError> {
        let units: Vec<_> = text.encode_utf16().collect();
        self.insert_utf16_at_anchor(anchor, &units, undo_group)
    }

    pub fn insert_utf16_at_anchor(
        &mut self,
        anchor: &Anchor,
        units: &[u16],
        undo_group: Option<u64>,
    ) -> Result<(Option<LocalUpdate>, Anchor), EngineError> {
        let index = self.resolve_anchor(anchor);
        let update = self.insert_utf16(index, units, undo_group)?;
        let caret = self.anchor(index + units.len(), Affinity::After)?;
        Ok((update, caret))
    }

    pub fn delete(
        &mut self,
        index: usize,
        length: usize,
        undo_group: Option<u64>,
    ) -> Result<Option<LocalUpdate>, EngineError> {
        let auto = self.transaction.is_none();
        if auto {
            self.begin_transaction(undo_group)?;
        }
        let result = self.delete_units(index, length);
        self.finish_auto(auto, result)
    }

    pub fn replace_range(
        &mut self,
        from: usize,
        to: usize,
        inserted: &str,
        undo_group: Option<u64>,
    ) -> Result<Option<LocalUpdate>, EngineError> {
        let units: Vec<_> = inserted.encode_utf16().collect();
        self.replace_range_utf16(from, to, &units, undo_group)
    }

    pub fn replace_range_utf16(
        &mut self,
        from: usize,
        to: usize,
        inserted: &[u16],
        undo_group: Option<u64>,
    ) -> Result<Option<LocalUpdate>, EngineError> {
        let auto = self.transaction.is_none();
        if auto {
            self.begin_transaction(undo_group)?;
        }
        let result = (|| {
            if from > to || to > self.len() {
                return Err(EngineError::new(
                    ErrorCode::InvalidRange,
                    "replace range is outside the document",
                ));
            }
            let inserted_units = inserted.len();
            let remaining = self.len() - (to - from);
            let final_length = remaining.checked_add(inserted_units).ok_or_else(|| {
                EngineError::new(ErrorCode::IntegerOverflow, "document length overflow")
            })?;
            if final_length > self.limits.max_document_units {
                return Err(EngineError::new(
                    ErrorCode::DocumentTooLarge,
                    "replace would exceed the document limit",
                ));
            }
            self.delete_units(from, to - from)?;
            self.insert_units(from, inserted.iter().copied())
        })();
        self.finish_auto(auto, result)
    }

    fn finish_auto(
        &mut self,
        auto: bool,
        result: Result<(), EngineError>,
    ) -> Result<Option<LocalUpdate>, EngineError> {
        if let Err(error) = result {
            // A failing edit invalidates the entire explicit transaction too;
            // callers never observe a half-mutated transaction awaiting commit.
            if let Some(transaction) = self.transaction.take() {
                self.rollback_transaction(transaction);
            }
            return Err(error);
        }
        if auto {
            self.commit_transaction_inner()
        } else {
            Ok(None)
        }
    }

    fn insert_units(
        &mut self,
        index: usize,
        units: impl IntoIterator<Item = u16>,
    ) -> Result<(), EngineError> {
        if self.transaction.is_none() {
            return Err(EngineError::new(
                ErrorCode::NoActiveTransaction,
                "insert requires an active transaction",
            ));
        }
        if index > self.len() {
            return Err(EngineError::new(
                ErrorCode::InvalidRange,
                "insert index exceeds the document",
            ));
        }
        let units: Vec<u16> = units.into_iter().collect();
        if units.len() > self.limits.max_operations_per_update {
            return Err(EngineError::new(
                ErrorCode::TooManyOperations,
                "insert exceeds the transaction operation limit",
            ));
        }
        let final_length = self.len().checked_add(units.len()).ok_or_else(|| {
            EngineError::new(ErrorCode::IntegerOverflow, "document length overflow")
        })?;
        if final_length > self.limits.max_document_units {
            return Err(EngineError::new(
                ErrorCode::DocumentTooLarge,
                "insert would exceed the document limit",
            ));
        }
        for (offset, unit) in units.into_iter().enumerate() {
            self.ensure_transaction_capacity(1)?;
            let operation = self.replica.try_local_insert(
                index + offset,
                unit,
                self.limits.max_allocation_attempts,
            )?;
            let identifier_too_deep = operation.weight.sc.len() > self.limits.max_identifier_depth;
            let action = UndoAction::DeleteInserted {
                weight: operation.weight.clone(),
                counter: operation.counter,
            };
            let transaction = self.transaction.as_mut().expect("checked transaction");
            transaction.operations.push(operation);
            transaction.undo_actions.push(action);
            if identifier_too_deep {
                return Err(EngineError::new(
                    ErrorCode::IdentifierTooDeep,
                    "allocated identifier exceeds the resource policy",
                ));
            }
        }
        Ok(())
    }

    fn delete_units(&mut self, index: usize, length: usize) -> Result<(), EngineError> {
        if self.transaction.is_none() {
            return Err(EngineError::new(
                ErrorCode::NoActiveTransaction,
                "delete requires an active transaction",
            ));
        }
        let end = index
            .checked_add(length)
            .ok_or_else(|| EngineError::new(ErrorCode::IntegerOverflow, "delete range overflow"))?;
        if index > self.len() || end > self.len() {
            return Err(EngineError::new(
                ErrorCode::InvalidRange,
                "delete range is outside the document",
            ));
        }
        for _ in 0..length {
            self.ensure_transaction_capacity(1)?;
            let (weight, unit, counter) = self
                .replica
                .doc
                .get_at(index)
                .map(|(weight, unit, counter)| (weight.clone(), unit, counter))
                .ok_or_else(|| {
                    EngineError::new(ErrorCode::InvalidRange, "delete target disappeared")
                })?;
            let operation = self
                .replica
                .try_local_delete(index)?
                .ok_or_else(|| EngineError::new(ErrorCode::InvalidRange, "delete was empty"))?;
            let deletion = OperationRef::from(&operation);
            let transaction = self.transaction.as_mut().expect("checked transaction");
            transaction.operations.push(operation);
            transaction.undo_actions.push(UndoAction::RestoreDeleted {
                weight,
                unit,
                deleted_counter: counter,
                deletion,
            });
        }
        Ok(())
    }

    fn ensure_transaction_capacity(&self, additional: usize) -> Result<(), EngineError> {
        let count = self
            .transaction
            .as_ref()
            .map(|transaction| transaction.operations.len())
            .unwrap_or(0)
            .checked_add(additional)
            .ok_or_else(|| {
                EngineError::new(ErrorCode::IntegerOverflow, "transaction size overflow")
            })?;
        if count > self.limits.max_operations_per_update {
            return Err(EngineError::new(
                ErrorCode::TooManyOperations,
                "transaction exceeds the operation limit",
            ));
        }
        let retained = self
            .replica
            .log
            .len()
            .checked_add(additional)
            .ok_or_else(|| {
                EngineError::new(ErrorCode::IntegerOverflow, "retained history size overflow")
            })?;
        if retained > self.limits.max_retained_operations {
            return Err(EngineError::new(
                ErrorCode::TooManyOperations,
                "local edit exceeds retained operation history",
            ));
        }
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn clear_undo(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    fn reconcile_undo_after_compaction(&mut self) -> UndoDisposition {
        // A compact base records the resulting state and receipts, not the
        // complete set of delete operations that targeted each insertion. Once
        // history advances, retaining our own delete is therefore insufficient
        // proof that no collaborator also deleted the same item. Resurrection
        // records must be invalidated; otherwise undo could defeat a delete
        // already absorbed by the compact base. Pure insertion undo remains
        // safe because it only targets the exact insertion identity we minted.
        let before = self.undo_stack.len().saturating_add(self.redo_stack.len());
        let safe = |record: &UndoRecord| {
            record
                .actions
                .iter()
                .all(|action| matches!(action, UndoAction::DeleteInserted { .. }))
        };
        self.undo_stack.retain(safe);
        self.redo_stack.retain(safe);
        let after = self.undo_stack.len().saturating_add(self.redo_stack.len());
        if before == after {
            UndoDisposition::Preserved
        } else if after == 0 {
            UndoDisposition::Cleared
        } else {
            UndoDisposition::PartiallyPreserved
        }
    }

    pub fn undo(&mut self) -> Result<Option<LocalUpdate>, EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "cannot undo inside a transaction",
            ));
        }
        while let Some(record) = self.undo_stack.pop() {
            self.begin_transaction_mode(record.group, TransactionMode::Undo)?;
            for action in record.actions.iter().rev() {
                if let Err(error) = self.compensate(action) {
                    let _ = self.abort_transaction();
                    self.undo_stack.push(record);
                    return Err(error);
                }
            }
            let update = match self.commit_transaction_inner() {
                Ok(update) => update,
                Err(error) => {
                    self.undo_stack.push(record);
                    return Err(error);
                }
            };
            if update.is_some() {
                return Ok(update);
            }
        }
        Ok(None)
    }

    pub fn redo(&mut self) -> Result<Option<LocalUpdate>, EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "cannot redo inside a transaction",
            ));
        }
        while let Some(record) = self.redo_stack.pop() {
            self.begin_transaction_mode(record.group, TransactionMode::Redo)?;
            for action in record.actions.iter().rev() {
                if let Err(error) = self.compensate(action) {
                    let _ = self.abort_transaction();
                    self.redo_stack.push(record);
                    return Err(error);
                }
            }
            let update = match self.commit_transaction_inner() {
                Ok(update) => update,
                Err(error) => {
                    self.redo_stack.push(record);
                    return Err(error);
                }
            };
            if update.is_some() {
                return Ok(update);
            }
        }
        Ok(None)
    }

    fn compensate(&mut self, action: &UndoAction) -> Result<(), EngineError> {
        self.ensure_transaction_capacity(1)?;
        match action {
            UndoAction::DeleteInserted { weight, counter } => {
                let Some((unit, live_counter)) = self.replica.doc.find(weight) else {
                    return Ok(());
                };
                if live_counter != *counter {
                    return Ok(());
                }
                let Some(index) = self.replica.doc.index_of(weight) else {
                    return Ok(());
                };
                let operation = self.replica.try_local_delete(index)?.ok_or_else(|| {
                    EngineError::new(ErrorCode::InvalidRange, "undo delete failed")
                })?;
                let deletion = OperationRef::from(&operation);
                let transaction = self.transaction.as_mut().expect("undo transaction");
                transaction.operations.push(operation);
                transaction.undo_actions.push(UndoAction::RestoreDeleted {
                    weight: weight.clone(),
                    unit,
                    deleted_counter: *counter,
                    deletion,
                });
            }
            UndoAction::RestoreDeleted {
                weight,
                unit,
                deleted_counter,
                deletion,
            } => {
                if self.replica.doc.contains(weight) {
                    return Ok(());
                }
                let own_delete_is_retained = self
                    .replica
                    .log
                    .get(&(deletion.origin, deletion.sequence))
                    .is_some_and(|operation| {
                        matches!(operation.kind, OpKind::Del)
                            && operation.weight == *weight
                            && operation.counter == *deleted_counter
                    });
                if !own_delete_is_retained {
                    return Ok(());
                }
                // A compensating insertion is valid only while this replica's
                // deletion is the sole retained delete of the original item.
                // If a collaborator also deleted it, restoring a fresh weight
                // would incorrectly defeat that collaborator's intention.
                if self.replica.log.values().any(|operation| {
                    OperationRef::from(operation) != *deletion
                        && matches!(operation.kind, OpKind::Del)
                        && operation.weight == *weight
                        && operation.counter == *deleted_counter
                }) {
                    return Ok(());
                }
                let operation = if weight.site == self.replica.site {
                    // Scenario 3: the site encoded into this weight may reuse
                    // it under its monotonically increasing insertion counter.
                    self.replica
                        .try_local_insert_at_weight(weight.clone(), *unit)?
                } else {
                    // A different site cannot reuse the exact weight because
                    // insertion counters are scoped to their weight-owning
                    // site. Mint a fresh local weight at the deleted weight's
                    // stable lower bound instead.
                    let index = self.replica.doc.lower_bound(weight);
                    self.replica.try_local_insert(
                        index,
                        *unit,
                        self.limits.max_allocation_attempts,
                    )?
                };
                let identifier_too_deep =
                    operation.weight.sc.len() > self.limits.max_identifier_depth;
                let action = UndoAction::DeleteInserted {
                    weight: operation.weight.clone(),
                    counter: operation.counter,
                };
                let transaction = self.transaction.as_mut().expect("undo transaction");
                transaction.operations.push(operation);
                transaction.undo_actions.push(action);
                if identifier_too_deep {
                    return Err(EngineError::new(
                        ErrorCode::IdentifierTooDeep,
                        "undo allocated an identifier beyond the resource policy",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Decode and atomically apply one canonical update. Snapshot handling is
    /// exposed separately because its receipt and history semantics differ.
    pub fn apply_bytes(&mut self, bytes: &[u8]) -> Result<ApplyReceipt, EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "cannot import while a local transaction is active",
            ));
        }
        let message = Message::decode_with_limits(bytes, &self.limits)?;
        let update = match message {
            Message::Update(update) => update,
            _ => {
                return Err(EngineError::new(
                    ErrorCode::MalformedEncoding,
                    "apply_bytes expects an operation update",
                ))
            }
        };
        self.apply_update(update)
    }

    pub fn apply_update(&mut self, update: Update) -> Result<ApplyReceipt, EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "cannot import while a local transaction is active",
            ));
        }
        if update.len() > self.limits.max_operations_per_update {
            return Err(EngineError::new(
                ErrorCode::TooManyOperations,
                "update exceeds the operation limit",
            ));
        }
        let mut admitted = Vec::new();
        let mut admitted_insertions = BTreeSet::new();
        for operation in update.operations() {
            self.validate_operation(operation)?;
            if let Some(existing) = self.replica.log.get(&(operation.origin, operation.seq)) {
                if existing != operation {
                    return Err(EngineError::new(
                        ErrorCode::OperationIdentityConflict,
                        "operation identity conflicts with retained history",
                    ));
                }
                continue;
            }
            if self
                .replica
                .version
                .contains(operation.origin, operation.seq)
            {
                continue;
            }
            if matches!(operation.kind, OpKind::Ins { .. }) {
                let insertion = (operation.weight.site, operation.counter);
                if self
                    .replica
                    .insertion_version
                    .contains(insertion.0, insertion.1)
                    || !admitted_insertions.insert(insertion)
                {
                    return Err(EngineError::new(
                        ErrorCode::OperationIdentityConflict,
                        "insertion counter is already bound to another operation",
                    ));
                }
            }
            admitted.push(operation);
        }

        let retained_after = self
            .replica
            .log
            .len()
            .checked_add(admitted.len())
            .ok_or_else(|| {
                EngineError::new(
                    ErrorCode::IntegerOverflow,
                    "retained operation count overflow",
                )
            })?;
        if retained_after > self.limits.max_retained_operations {
            return Err(EngineError::new(
                ErrorCode::TooManyOperations,
                "update would exceed retained operation history",
            ));
        }
        let projected_version = self.replica.version.project_notes(
            admitted
                .iter()
                .map(|operation| (operation.origin, operation.seq)),
        );
        let projected_insertions = self
            .replica
            .insertion_version
            .project_notes(admitted_insertions.iter().copied());
        for projection in [projected_version, projected_insertions] {
            if projection.site_count > self.limits.max_version_sites {
                return Err(EngineError::new(
                    ErrorCode::TooManyVersionSites,
                    "update would exceed the receipt site limit",
                ));
            }
            if projection.sparse_count > self.limits.max_sparse_receipts {
                return Err(EngineError::new(
                    ErrorCode::TooManySparseReceipts,
                    "update would exceed the sparse receipt limit",
                ));
            }
        }

        let projection = self
            .replica
            .project_admission(&admitted, &admitted_insertions);
        if projection.visible_units > self.limits.max_document_units {
            return Err(EngineError::new(
                ErrorCode::DocumentTooLarge,
                "update would exceed the visible document limit",
            ));
        }
        if projection.pending_operations > self.limits.max_pending_operations {
            return Err(EngineError::new(
                ErrorCode::TooManyPendingOperations,
                "update would exceed the pending causal queue",
            ));
        }
        if projection.deferred_deletes > self.limits.max_deferred_deletes {
            return Err(EngineError::new(
                ErrorCode::TooManyDeferredDeletes,
                "update would exceed the deferred deletion limit",
            ));
        }

        let accepted_operations: Vec<_> = admitted
            .iter()
            .map(|operation| OperationRef::from(*operation))
            .collect();
        let journal_bytes = if admitted.is_empty() {
            None
        } else {
            let operations = admitted
                .iter()
                .map(|operation| (*operation).clone())
                .collect();
            let bytes = Message::Update(Update::new(operations)?).encode();
            if bytes.len() > self.limits.max_message_bytes {
                return Err(EngineError::new(
                    ErrorCode::MessageTooLarge,
                    "canonical journal update exceeds the message limit",
                ));
            }
            Some(bytes)
        };
        let before_pending: BTreeSet<_> = self
            .replica
            .pending
            .iter()
            .map(OperationRef::from)
            .collect();
        let before_revision = self.replica.visible_revision();
        self.replica
            .admit_validated_operations(admitted.into_iter().cloned());

        let after_pending: BTreeSet<_> = self
            .replica
            .pending
            .iter()
            .map(OperationRef::from)
            .collect();
        let applied_operations: Vec<_> = accepted_operations
            .iter()
            .copied()
            .filter(|identity| !after_pending.contains(identity))
            .collect();
        let buffered_operations: Vec<_> = accepted_operations
            .iter()
            .copied()
            .filter(|identity| after_pending.contains(identity))
            .collect();
        let newly_ready_operations: Vec<_> =
            before_pending.difference(&after_pending).copied().collect();
        let duplicate_count = update.len().saturating_sub(accepted_operations.len());
        let outcome = if update.is_empty() {
            ApplyOutcome::Noop
        } else if accepted_operations.is_empty() {
            ApplyOutcome::Duplicate
        } else if applied_operations.is_empty()
            && buffered_operations.len() == accepted_operations.len()
            && duplicate_count == 0
        {
            ApplyOutcome::Buffered
        } else if buffered_operations.is_empty() && duplicate_count == 0 {
            ApplyOutcome::Applied
        } else {
            ApplyOutcome::Mixed
        };

        let visible_changed = before_revision != self.replica.visible_revision();
        Ok(ApplyReceipt {
            outcome,
            accepted_operations,
            applied_operations,
            buffered_operations,
            newly_ready_operations,
            version: self.replica.version.clone(),
            visible_changed,
            journal_bytes,
        })
    }

    /// Export a causally closed compact merge base. Pending operations and
    /// sparse receipts require a full archive instead.
    pub fn export_compact_snapshot(&self) -> Result<Vec<u8>, EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "cannot export a snapshot from an uncommitted transaction",
            ));
        }
        if !self.replica.version.is_contiguous() || !self.replica.pending.is_empty() {
            return Err(EngineError::new(
                ErrorCode::SnapshotNotCausallyClosed,
                "compact snapshot requires a contiguous version and empty pending queue",
            ));
        }
        let bytes = Message::Snapshot(self.replica.snapshot()).encode();
        if bytes.len() > self.limits.max_message_bytes {
            return Err(EngineError::new(
                ErrorCode::MessageTooLarge,
                "compact snapshot exceeds the message limit",
            ));
        }
        Ok(bytes)
    }

    /// Export restart-complete state, retained reconnect history, and the
    /// exact pending subset. This is the durable browser checkpoint format.
    pub fn export_full_snapshot(&self) -> Result<Vec<u8>, EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "cannot export a snapshot from an uncommitted transaction",
            ));
        }
        let mut retained_operations: Vec<_> = self.replica.log.values().cloned().collect();
        retained_operations.sort_by_key(|operation| (operation.origin, operation.seq));
        let mut pending_operations: Vec<_> = self
            .replica
            .pending
            .iter()
            .map(OperationRef::from)
            .collect();
        pending_operations.sort();
        let snapshot = FullSnapshot::new(
            self.replica.snapshot(),
            self.history_floor.clone(),
            retained_operations,
            pending_operations,
        )?;
        let bytes = Message::FullSnapshot(snapshot).encode();
        if bytes.len() > self.limits.max_message_bytes {
            return Err(EngineError::new(
                ErrorCode::MessageTooLarge,
                "full snapshot exceeds the message limit",
            ));
        }
        Ok(bytes)
    }

    pub fn apply_snapshot_bytes(&mut self, bytes: &[u8]) -> Result<SnapshotReceipt, EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "cannot import a snapshot inside a local transaction",
            ));
        }
        let message = Message::decode_with_limits(bytes, &self.limits)?;
        let mut staged = self.clone();
        let before_units = staged.utf16_units();
        let (kind, undo) = match message {
            Message::Snapshot(snapshot) => {
                let undo = staged.merge_compact_snapshot(&snapshot)?;
                (SnapshotKind::Compact, undo)
            }
            Message::FullSnapshot(snapshot) => staged.merge_full_snapshot(&snapshot)?,
            _ => {
                return Err(EngineError::new(
                    ErrorCode::MalformedEncoding,
                    "apply_snapshot_bytes expects a snapshot envelope",
                ))
            }
        };
        staged.validate_state()?;
        let receipt = SnapshotReceipt {
            kind,
            version: staged.version(),
            visible_changed: before_units != staged.utf16_units(),
            undo,
        };
        *self = staged;
        Ok(receipt)
    }

    fn merge_compact_snapshot(
        &mut self,
        snapshot: &Snapshot,
    ) -> Result<UndoDisposition, EngineError> {
        let rebased = self
            .replica
            .merge_snapshot(snapshot)
            .map_err(|error| match error {
                SnapshotMergeError::SnapshotHasSequenceGaps => EngineError::new(
                    ErrorCode::SnapshotHasSequenceGaps,
                    "compact snapshot version has sequence gaps",
                ),
                SnapshotMergeError::MissingLocalHistory => EngineError::new(
                    ErrorCode::MissingLocalHistory,
                    "compact snapshot cannot replay retained local state",
                ),
                SnapshotMergeError::CorruptRetainedHistory => EngineError::new(
                    ErrorCode::OperationIdentityConflict,
                    "retained local history conflicts during compact rebase",
                ),
                SnapshotMergeError::SnapshotStateConflict => EngineError::new(
                    ErrorCode::SnapshotStateConflict,
                    "equal compact-snapshot receipts describe different state",
                ),
            })?;
        // Pure insertion undo remains valid across a rebase. A compact base
        // cannot prove complete delete provenance, so resurrection records are
        // invalidated rather than risking reversal of a collaborator's delete.
        if rebased {
            self.history_floor.merge(&snapshot.version);
            Ok(self.reconcile_undo_after_compaction())
        } else {
            Ok(UndoDisposition::Preserved)
        }
    }

    fn merge_full_snapshot(
        &mut self,
        snapshot: &FullSnapshot,
    ) -> Result<(SnapshotKind, UndoDisposition), EngineError> {
        let pristine = self.replica.version == Version::default()
            && self.replica.log.is_empty()
            && self.replica.pending.is_empty()
            && self.replica.doc.is_empty()
            && self.replica.delete_log.is_empty()
            && self.replica.insertion_version == Version::default();
        if pristine {
            self.replica.install_snapshot(&snapshot.state);
            self.replica.log.clear();
            self.replica.pending.clear();
            // Replaying on top of the materialized state is idempotent for a
            // valid archive and derives the pending queue instead of trusting a
            // second state representation. Admission drains each weight once.
            self.replica
                .restore_validated_operations(snapshot.retained_operations());
            let actual_pending: BTreeSet<_> = self
                .replica
                .pending
                .iter()
                .map(OperationRef::from)
                .collect();
            let declared_pending: BTreeSet<_> =
                snapshot.pending_operations.iter().copied().collect();
            if actual_pending != declared_pending {
                return Err(EngineError::new(
                    ErrorCode::NonCanonicalEncoding,
                    "full snapshot pending identities disagree with replayed state",
                ));
            }
            self.history_floor = snapshot.history_floor.clone();
            self.clear_undo();
            return Ok((SnapshotKind::Full, UndoDisposition::Cleared));
        }

        // A nonempty replica may merge an archive only when it already has
        // the compacted prefix. Otherwise the omitted operations are
        // unavailable and installing the materialized state could erase local
        // edits or mis-handle a reused weight.
        if !self.replica.version.covers(&snapshot.history_floor) {
            return Err(EngineError::new(
                ErrorCode::MissingLocalHistory,
                "receiver does not cover the full snapshot history floor",
            ));
        }
        for operations in snapshot
            .retained_operations()
            .chunks(self.limits.max_operations_per_update)
        {
            self.apply_update(Update::new(operations.to_vec())?)?;
        }
        // This path unions retained operations into the current base; it does
        // not replace state or discard local bytes. The local history floor
        // therefore remains the exact boundary below which this document
        // cannot serve reconnect deltas.
        Ok((SnapshotKind::Full, UndoDisposition::Preserved))
    }

    /// Record a caller-certified causal prefix in materialized state and drop
    /// its individual operations. Peers behind this floor must receive a
    /// snapshot; `export_update` will return `HistoryUnavailable` for them.
    pub fn prune_history_through(&mut self, acknowledged: &Version) -> Result<usize, EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "cannot compact history inside a local transaction",
            ));
        }
        if !acknowledged.is_contiguous() || !self.replica.version.covers(acknowledged) {
            return Err(EngineError::new(
                ErrorCode::SnapshotNotCausallyClosed,
                "compaction acknowledgement is not a covered causal prefix",
            ));
        }
        if self
            .replica
            .pending
            .iter()
            .any(|operation| acknowledged.contains(operation.origin, operation.seq))
        {
            return Err(EngineError::new(
                ErrorCode::SnapshotNotCausallyClosed,
                "cannot prune an operation that is still pending",
            ));
        }
        let before = self.replica.log.len();
        self.replica
            .log
            .retain(|&(origin, sequence), _| !acknowledged.contains(origin, sequence));
        self.history_floor.merge(acknowledged);
        let removed = before - self.replica.log.len();
        if removed > 0 {
            self.reconcile_undo_after_compaction();
        }
        Ok(removed)
    }

    pub fn export_update(&self, remote: &Version) -> Result<Vec<u8>, EngineError> {
        if self.transaction.is_some() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "cannot export an update from an uncommitted transaction",
            ));
        }
        if !remote.covers(&self.history_floor) {
            return Err(EngineError::new(
                ErrorCode::HistoryUnavailable,
                "remote version predates compacted history; send a snapshot",
            ));
        }
        let update = Update::new(self.replica.ops_missing_from(remote))?;
        let bytes = Message::Update(update).encode();
        if bytes.len() > self.limits.max_message_bytes {
            return Err(EngineError::new(
                ErrorCode::MessageTooLarge,
                "reconnect delta exceeds the message limit",
            ));
        }
        Ok(bytes)
    }

    fn validate_operation(&self, operation: &Op) -> Result<(), EngineError> {
        if operation.origin == 0
            || operation.seq == 0
            || operation.counter == 0
            || operation.weight.site == Weight::EMPTY_SITE
        {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "operation contains a zero identity",
            ));
        }
        if operation.weight.sc.is_empty() {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "operation identifier path is empty",
            ));
        }
        if operation.weight.sc.len() > self.limits.max_identifier_depth {
            return Err(EngineError::new(
                ErrorCode::IdentifierTooDeep,
                "operation identifier exceeds the depth limit",
            ));
        }
        if matches!(operation.kind, OpKind::Ins { .. }) && operation.origin != operation.weight.site
        {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "insertion origin must own its ESBT weight",
            ));
        }
        Ok(())
    }

    fn validate_state(&self) -> Result<(), EngineError> {
        self.validate_replica(&self.replica)
    }

    fn validate_replica(&self, replica: &Replica) -> Result<(), EngineError> {
        if replica.len() > self.limits.max_document_units {
            return Err(EngineError::new(
                ErrorCode::DocumentTooLarge,
                "document exceeds the UTF-16 unit limit",
            ));
        }
        if replica.pending.len() > self.limits.max_pending_operations {
            return Err(EngineError::new(
                ErrorCode::TooManyPendingOperations,
                "pending causal queue exceeds the limit",
            ));
        }
        if replica.delete_log.len() > self.limits.max_deferred_deletes {
            return Err(EngineError::new(
                ErrorCode::TooManyDeferredDeletes,
                "delete log exceeds the limit",
            ));
        }
        if replica.log.len() > self.limits.max_retained_operations {
            return Err(EngineError::new(
                ErrorCode::TooManyOperations,
                "retained operation history exceeds the limit",
            ));
        }
        for operation in replica.log.values() {
            self.validate_operation(operation)?;
        }
        if replica.version.site_count() > self.limits.max_version_sites {
            return Err(EngineError::new(
                ErrorCode::TooManyVersionSites,
                "replica version has too many sites",
            ));
        }
        if replica.version.sparse_receipt_count() > self.limits.max_sparse_receipts {
            return Err(EngineError::new(
                ErrorCode::TooManySparseReceipts,
                "replica version has too many sparse receipts",
            ));
        }
        if replica.insertion_version.site_count() > self.limits.max_version_sites {
            return Err(EngineError::new(
                ErrorCode::TooManyVersionSites,
                "insertion receipt summary has too many sites",
            ));
        }
        if replica.insertion_version.sparse_receipt_count() > self.limits.max_sparse_receipts {
            return Err(EngineError::new(
                ErrorCode::TooManySparseReceipts,
                "insertion receipt summary has too many sparse counters",
            ));
        }
        for (weight, _, _) in replica.doc.atoms() {
            if weight.sc.len() > self.limits.max_identifier_depth {
                return Err(EngineError::new(
                    ErrorCode::IdentifierTooDeep,
                    "live identifier exceeds the depth limit",
                ));
            }
        }
        for (weight, _) in &replica.delete_log {
            if weight.sc.len() > self.limits.max_identifier_depth {
                return Err(EngineError::new(
                    ErrorCode::IdentifierTooDeep,
                    "deleted identifier exceeds the depth limit",
                ));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn replica(&self) -> &Replica {
        &self.replica
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_transaction_emits_one_retry_safe_update() {
        let mut a = Document::with_defaults(1).expect("document");
        a.begin_transaction(Some(7)).expect("begin");
        assert!(a.insert(0, "hello", None).expect("insert").is_none());
        assert!(a.insert(5, " 😀", None).expect("insert").is_none());
        let local = a
            .commit_transaction()
            .expect("commit")
            .expect("local update");
        assert_eq!(local.update.len(), 8);

        let mut b = Document::with_defaults(2).expect("document");
        let receipt = b.apply_bytes(&local.canonical_bytes).expect("apply");
        assert_eq!(receipt.outcome, ApplyOutcome::Applied);
        assert_eq!(a.text(), b.text());
        let duplicate = b.apply_bytes(&local.canonical_bytes).expect("retry");
        assert_eq!(duplicate.outcome, ApplyOutcome::Duplicate);
        assert!(duplicate.journal_bytes.is_none());
    }

    #[test]
    fn operation_identity_conflict_is_atomic() {
        let mut source = Document::with_defaults(1).expect("source");
        let bytes = source
            .insert(0, "A", None)
            .expect("insert")
            .expect("update")
            .canonical_bytes;
        let mut target = Document::with_defaults(2).expect("target");
        target.apply_bytes(&bytes).expect("first apply");
        let before = target.text();

        let mut conflicting = match Message::decode(&bytes).expect("decode") {
            Message::Update(update) => update.operations()[0].clone(),
            _ => unreachable!(),
        };
        conflicting.kind = OpKind::Ins { unit: b'Z' as u16 };
        let conflict = Message::Update(Update::new(vec![conflicting]).expect("update")).encode();
        assert_eq!(
            target.apply_bytes(&conflict).expect_err("conflict").code,
            ErrorCode::OperationIdentityConflict
        );
        assert_eq!(target.text(), before);
    }

    #[test]
    fn delete_before_insert_receipt_reports_buffer_and_release() {
        let mut source = Document::with_defaults(1).expect("source");
        let insert = source
            .insert(0, "A", None)
            .expect("insert")
            .expect("update");
        let delete = source.delete(0, 1, None).expect("delete").expect("update");
        let mut target = Document::with_defaults(2).expect("target");
        let buffered = target
            .apply_bytes(&delete.canonical_bytes)
            .expect("buffer delete");
        assert_eq!(buffered.outcome, ApplyOutcome::Buffered);
        let released = target
            .apply_bytes(&insert.canonical_bytes)
            .expect("apply insert");
        assert_eq!(released.newly_ready_operations.len(), 1);
        assert!(target.text().is_empty());
    }

    #[test]
    fn undo_is_local_compensation_and_redo_reuses_with_a_fresh_counter() {
        let mut a = Document::with_defaults(1).expect("a");
        let mut b = Document::with_defaults(2).expect("b");
        let from_a = a
            .insert(0, "from A. ", Some(1))
            .expect("a insert")
            .expect("a update");
        b.apply_bytes(&from_a.canonical_bytes).expect("sync a");
        let from_b = b
            .insert(b.len(), "from B.", Some(2))
            .expect("b insert")
            .expect("b update");
        a.apply_bytes(&from_b.canonical_bytes).expect("sync b");

        let undo = a.undo().expect("undo").expect("undo update");
        b.apply_bytes(&undo.canonical_bytes).expect("sync undo");
        assert_eq!(a.text(), "from B.");
        assert_eq!(a.text(), b.text());

        let redo = a.redo().expect("redo").expect("redo update");
        b.apply_bytes(&redo.canonical_bytes).expect("sync redo");
        assert_eq!(a.text(), "from A. from B.");
        assert_eq!(a.text(), b.text());
    }

    #[test]
    fn shared_undo_group_turns_a_typing_burst_into_one_step() {
        let mut document = Document::with_defaults(1).expect("document");
        for character in "burst".chars() {
            document
                .insert(document.len(), &character.to_string(), Some(42))
                .expect("type");
        }
        document.undo().expect("undo").expect("undo update");
        assert!(document.text().is_empty());
        document.redo().expect("redo").expect("redo update");
        assert_eq!(document.text(), "burst");
    }

    #[test]
    fn failed_limit_check_rolls_back_the_entire_transaction() {
        let limits = ResourceLimits {
            max_document_units: 3,
            ..ResourceLimits::default()
        };
        let mut document = Document::new(1, ReplicaConfig::default(), limits).expect("document");
        let error = document.insert(0, "four", None).expect_err("limit");
        assert_eq!(error.code, ErrorCode::DocumentTooLarge);
        assert!(document.is_empty());
        assert_eq!(document.replica().log.len(), 0);
    }

    #[test]
    fn full_snapshot_restores_pending_delete_and_retained_history() {
        let mut source = Document::with_defaults(1).expect("source");
        let insertion = source
            .insert(0, "A", None)
            .expect("insert")
            .expect("insert update");
        let deletion = source
            .delete(0, 1, None)
            .expect("delete")
            .expect("delete update");

        let mut partial = Document::with_defaults(2).expect("partial");
        partial
            .apply_bytes(&deletion.canonical_bytes)
            .expect("buffer delete");
        assert_eq!(partial.pending_len(), 1);
        assert_eq!(
            partial
                .export_compact_snapshot()
                .expect_err("not closed")
                .code,
            ErrorCode::SnapshotNotCausallyClosed
        );

        let archive = partial.export_full_snapshot().expect("full snapshot");
        let mut restored = Document::with_defaults(3).expect("restored");
        let receipt = restored
            .apply_snapshot_bytes(&archive)
            .expect("restore full snapshot");
        assert_eq!(receipt.kind, SnapshotKind::Full);
        assert_eq!(receipt.undo, UndoDisposition::Cleared);
        assert_eq!(restored.pending_len(), 1);

        restored
            .apply_bytes(&insertion.canonical_bytes)
            .expect("fill causal gap");
        assert!(restored.text().is_empty());
        assert_eq!(restored.pending_len(), 0);
    }

    #[test]
    fn compact_snapshot_merge_preserves_offline_local_update() {
        let mut server = Document::with_defaults(1).expect("server");
        server.insert(0, "base", None).expect("base");
        let initial = server.export_compact_snapshot().expect("initial compact");

        let mut client = Document::with_defaults(2).expect("client");
        client
            .apply_snapshot_bytes(&initial)
            .expect("install initial");
        client
            .insert(client.len(), "-offline", None)
            .expect("offline edit");

        server
            .insert(server.len(), "-remote", None)
            .expect("remote edit");
        let newer = server.export_compact_snapshot().expect("newer compact");
        let receipt = client.apply_snapshot_bytes(&newer).expect("merge compact");
        assert_eq!(receipt.undo, UndoDisposition::Preserved);
        assert!(client.text().contains("-offline"));
        assert!(client.text().contains("-remote"));

        let delta = client
            .export_update(&server.version())
            .expect("offline delta");
        server.apply_bytes(&delta).expect("apply offline delta");
        assert_eq!(client.text(), server.text());
    }

    #[test]
    fn pruned_history_requires_snapshot_for_an_older_peer() {
        let mut document = Document::with_defaults(1).expect("document");
        document.insert(0, "history", None).expect("insert");
        let acknowledged = document.version();
        assert!(
            document
                .prune_history_through(&acknowledged)
                .expect("prune")
                > 0
        );

        assert_eq!(
            document
                .export_update(&Version::default())
                .expect_err("history unavailable")
                .code,
            ErrorCode::HistoryUnavailable
        );
        let compact = document.export_compact_snapshot().expect("compact");
        let mut peer = Document::with_defaults(2).expect("peer");
        peer.apply_snapshot_bytes(&compact)
            .expect("snapshot fallback");
        assert_eq!(peer.text(), "history");
    }
}
