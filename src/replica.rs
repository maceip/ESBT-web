//! Algorithm 3 — per-replica control, plus join snapshot and op log.

use crate::allocator::Allocator;
use crate::clock::{SiteReceiptCheckpoint, Version};
use crate::error::{EngineError, ErrorCode};
use crate::op::{Op, OpKind};
use crate::rbtree::DocTree;
use crate::snapshot::{Atom, Snapshot};
use crate::weight::{SiteId, Weight};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

#[derive(Clone, Debug)]
pub struct ReplicaConfig {
    pub dmax: i64,
    pub base: u32,
    pub depth: u32,
}

impl Default for ReplicaConfig {
    fn default() -> Self {
        ReplicaConfig {
            dmax: 1 << 16,
            base: (1u32 << 31) - 1,
            depth: 256,
        }
    }
}

pub type WeightKey = Weight;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotMergeError {
    /// A compact snapshot that advertises out-of-order receipts is not a
    /// causally closed base. Its operation journal is required as well.
    SnapshotHasSequenceGaps,
    /// This replica knows state newer than the snapshot but no longer retains
    /// every operation needed to replay that state after installing the base.
    MissingLocalHistory,
    /// The retained local journal contradicted itself while it was being
    /// replayed over a compact base.
    CorruptRetainedHistory,
    /// Equal operation receipts described a different materialized state.
    SnapshotStateConflict,
}

/// Transient local intention state. A consecutive typing run keeps the first
/// ESBT weight as a site-distinct prefix and appends an intra-run component.
/// Concurrent runs then sort as units instead of alternating by character.
#[derive(Clone)]
struct LocalInsertRun {
    root: Weight,
    last: Weight,
    last_counter: u64,
    next_component: u32,
}

#[derive(Clone)]
pub(crate) struct LocalTransactionCheckpoint {
    local_sequence: u64,
    insertion_counter: u64,
    operation_receipts: SiteReceiptCheckpoint,
    insertion_receipts: SiteReceiptCheckpoint,
    insert_run: Option<LocalInsertRun>,
    visible_revision: u128,
}

#[derive(Clone, Copy)]
pub(crate) struct AdmissionProjection {
    pub visible_units: usize,
    pub pending_operations: usize,
    pub deferred_deletes: usize,
}

#[derive(Clone, Debug, Default)]
pub struct PendingQueue {
    by_weight: HashMap<Weight, VecDeque<Op>>,
    len: usize,
}

impl PendingQueue {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = &Op> {
        self.by_weight.values().flat_map(|queue| queue.iter())
    }

    pub fn clear(&mut self) {
        self.by_weight.clear();
        self.len = 0;
    }

    fn push_back(&mut self, operation: Op) {
        self.by_weight
            .entry(operation.weight.clone())
            .or_default()
            .push_back(operation);
        self.len = self.len.saturating_add(1);
    }

    fn take(&mut self, weight: &Weight) -> VecDeque<Op> {
        let queue = self.by_weight.remove(weight).unwrap_or_default();
        self.len = self.len.saturating_sub(queue.len());
        queue
    }

    fn replace(&mut self, weight: Weight, queue: VecDeque<Op>) {
        if let Some(previous) = self.by_weight.remove(&weight) {
            self.len = self.len.saturating_sub(previous.len());
        }
        if !queue.is_empty() {
            self.len = self.len.saturating_add(queue.len());
            self.by_weight.insert(weight, queue);
        }
    }

    fn for_weight(&self, weight: &Weight) -> Option<&VecDeque<Op>> {
        self.by_weight.get(weight)
    }

    fn weights(&self) -> Vec<Weight> {
        self.by_weight.keys().cloned().collect()
    }
}

struct ProjectedReplica<'a> {
    base: &'a Replica,
    live: HashMap<Weight, Option<u64>>,
    added_deletes: HashSet<(Weight, u64)>,
    removed_deletes: HashSet<(Weight, u64)>,
    visible_units: usize,
    deferred_deletes: usize,
}

impl<'a> ProjectedReplica<'a> {
    fn new(base: &'a Replica) -> Self {
        Self {
            base,
            live: HashMap::new(),
            added_deletes: HashSet::new(),
            removed_deletes: HashSet::new(),
            visible_units: base.len(),
            deferred_deletes: base.delete_log.len(),
        }
    }

    fn live_counter(&mut self, weight: &Weight) -> Option<u64> {
        if let Some(counter) = self.live.get(weight) {
            return *counter;
        }
        let counter = self.base.doc.find(weight).map(|(_, counter)| counter);
        self.live.insert(weight.clone(), counter);
        counter
    }

    fn set_live(&mut self, weight: &Weight, counter: Option<u64>) {
        self.live.insert(weight.clone(), counter);
    }

    fn has_delete(&self, identity: &(Weight, u64)) -> bool {
        if self.removed_deletes.contains(identity) {
            return false;
        }
        self.added_deletes.contains(identity) || self.base.delete_log.contains(identity)
    }

    fn add_delete(&mut self, identity: (Weight, u64)) {
        if self.has_delete(&identity) {
            return;
        }
        if !self.removed_deletes.remove(&identity) {
            self.added_deletes.insert(identity);
        }
        self.deferred_deletes = self.deferred_deletes.saturating_add(1);
    }

    fn remove_delete(&mut self, identity: &(Weight, u64)) -> bool {
        if !self.has_delete(identity) {
            return false;
        }
        if !self.added_deletes.remove(identity) {
            self.removed_deletes.insert(identity.clone());
        }
        self.deferred_deletes = self.deferred_deletes.saturating_sub(1);
        true
    }
}

#[derive(Clone)]
pub struct Replica {
    pub site: SiteId,
    pub alloc: Allocator,
    pub doc: DocTree,
    pub pending: PendingQueue,
    pub delete_log: HashSet<(WeightKey, u64)>,
    pub counter_map: HashMap<WeightKey, u64>,
    pub counter: u64,
    /// Exact insertion-counter receipts per weight-owning site. This is
    /// separate from `version`, whose sequences count both inserts and deletes.
    pub insertion_version: Version,
    pub version: Version,
    /// Reliable log: (origin, seq) → op. Needed so a peer can retransmit.
    pub log: HashMap<(SiteId, u64), Op>,
    pub local_seq: u64,
    local_insert_run: Option<LocalInsertRun>,
    visible_revision: u128,
}

impl Replica {
    pub fn new(site: SiteId, cfg: ReplicaConfig) -> Self {
        assert!(site != 0, "site 0 is reserved for sentinels");
        Replica {
            site,
            alloc: Allocator::new(cfg.dmax, cfg.base, cfg.depth),
            doc: DocTree::default(),
            pending: PendingQueue::default(),
            delete_log: HashSet::new(),
            counter_map: HashMap::new(),
            counter: 0,
            insertion_version: Version::default(),
            version: Version::default(),
            log: HashMap::new(),
            local_seq: 0,
            local_insert_run: None,
            visible_revision: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.doc.len()
    }

    pub fn is_empty(&self) -> bool {
        self.doc.is_empty()
    }

    pub fn text(&self) -> String {
        self.doc.text()
    }

    pub(crate) fn visible_revision(&self) -> u128 {
        self.visible_revision
    }

    /// Project the exact final queue, tombstone count, and visible length for
    /// an admitted update without mutating the document tree or retained log.
    pub(crate) fn project_admission(
        &self,
        operations: &[&Op],
        admitted_insertions: &BTreeSet<(SiteId, u64)>,
    ) -> AdmissionProjection {
        let mut additions: HashMap<Weight, VecDeque<Op>> = HashMap::new();
        for operation in operations {
            additions
                .entry(operation.weight.clone())
                .or_default()
                .push_back((*operation).clone());
        }
        self.project_buckets(additions, admitted_insertions)
    }

    /// Project causal work released by locally authored mutations that are
    /// already reflected in the tree but not yet allowed to alter the pending
    /// queues until the enclosing transaction commits.
    pub(crate) fn project_pending_drain(
        &self,
        weights: impl IntoIterator<Item = Weight>,
    ) -> AdmissionProjection {
        let additions = weights
            .into_iter()
            .map(|weight| (weight, VecDeque::new()))
            .collect();
        self.project_buckets(additions, &BTreeSet::new())
    }

    fn project_buckets(
        &self,
        additions: HashMap<Weight, VecDeque<Op>>,
        admitted_insertions: &BTreeSet<(SiteId, u64)>,
    ) -> AdmissionProjection {
        let mut state = ProjectedReplica::new(self);
        let mut pending_count = self.pending.len();

        // Readiness and reuse/tombstone resolution are local to one ESBT
        // weight. Project only touched buckets; every untouched bucket was
        // already drained to a fixed point when it was admitted.
        for (weight, new_operations) in additions {
            let mut pending = self
                .pending
                .for_weight(&weight)
                .cloned()
                .unwrap_or_default();
            pending_count = pending_count.saturating_sub(pending.len());
            pending.extend(new_operations);

            let mut progressed = true;
            while progressed {
                progressed = false;
                let mut index = 0usize;
                while index < pending.len() {
                    let operation = &pending[index];
                    let live = state.live_counter(&operation.weight);
                    let deletion = (operation.weight.clone(), operation.counter);
                    let has_delete = state.has_delete(&deletion);
                    let insertion_known = self
                        .insertion_version
                        .contains(operation.weight.site, operation.counter)
                        || admitted_insertions
                            .contains(&(operation.weight.site, operation.counter));
                    let ready = match operation.kind {
                        OpKind::Ins { .. } => live.is_none() || live == Some(operation.counter),
                        OpKind::Del => live == Some(operation.counter),
                    };
                    let ignored = if ready {
                        false
                    } else {
                        match operation.kind {
                            OpKind::Ins { .. } => {
                                has_delete
                                    || live.is_some_and(|counter| counter >= operation.counter)
                            }
                            OpKind::Del => {
                                has_delete
                                    || live.is_some_and(|counter| counter != operation.counter)
                                    || (live.is_none() && insertion_known)
                            }
                        }
                    };

                    if ready {
                        let Some(operation) = pending.remove(index) else {
                            break;
                        };
                        match operation.kind {
                            OpKind::Ins { .. } => {
                                let deletion = (operation.weight.clone(), operation.counter);
                                if !state.remove_delete(&deletion)
                                    && state.live_counter(&operation.weight).is_none()
                                {
                                    state.set_live(&operation.weight, Some(operation.counter));
                                    state.visible_units = state.visible_units.saturating_add(1);
                                }
                            }
                            OpKind::Del => {
                                if state.live_counter(&operation.weight) == Some(operation.counter)
                                {
                                    state.set_live(&operation.weight, None);
                                    state.visible_units = state.visible_units.saturating_sub(1);
                                }
                            }
                        }
                        progressed = true;
                    } else if ignored {
                        let Some(operation) = pending.remove(index) else {
                            break;
                        };
                        match operation.kind {
                            OpKind::Del => {
                                let target_is_pending = pending.iter().any(|pending| {
                                    matches!(pending.kind, OpKind::Ins { .. })
                                        && pending.counter == operation.counter
                                });
                                if target_is_pending
                                    || !(self
                                        .insertion_version
                                        .contains(operation.weight.site, operation.counter)
                                        || admitted_insertions
                                            .contains(&(operation.weight.site, operation.counter)))
                                {
                                    state.add_delete((operation.weight, operation.counter));
                                }
                            }
                            OpKind::Ins { .. } => {
                                state.remove_delete(&(operation.weight, operation.counter));
                            }
                        }
                        progressed = true;
                    } else {
                        index += 1;
                    }
                }
            }

            pending_count = pending_count.saturating_add(pending.len());
        }

        AdmissionProjection {
            visible_units: state.visible_units,
            pending_operations: pending_count,
            deferred_deletes: state.deferred_deletes,
        }
    }

    pub(crate) fn begin_local_transaction(
        &mut self,
    ) -> Result<LocalTransactionCheckpoint, EngineError> {
        if !self.alloc.begin_transaction() {
            return Err(EngineError::new(
                ErrorCode::TransactionAlreadyActive,
                "allocator transaction is already active",
            ));
        }
        Ok(LocalTransactionCheckpoint {
            local_sequence: self.local_seq,
            insertion_counter: self.counter,
            operation_receipts: self.version.checkpoint_site(self.site),
            insertion_receipts: self.insertion_version.checkpoint_site(self.site),
            insert_run: self.local_insert_run.clone(),
            visible_revision: self.visible_revision,
        })
    }

    pub(crate) fn commit_local_transaction(&mut self) {
        self.alloc.commit_transaction();
    }

    pub(crate) fn rollback_local_transaction(&mut self, checkpoint: LocalTransactionCheckpoint) {
        self.alloc.rollback_transaction();
        self.local_seq = checkpoint.local_sequence;
        self.counter = checkpoint.insertion_counter;
        self.version
            .restore_site(self.site, checkpoint.operation_receipts);
        self.insertion_version
            .restore_site(self.site, checkpoint.insertion_receipts);
        self.local_insert_run = checkpoint.insert_run;
        self.visible_revision = checkpoint.visible_revision;
    }

    fn neighbors(&self, index: usize) -> (Weight, Weight) {
        let left = if index == 0 {
            Weight::begin()
        } else {
            self.doc
                .get_at(index - 1)
                .map(|(w, _, _)| w.clone())
                .unwrap_or_else(Weight::begin)
        };
        let right = if index >= self.doc.len() {
            Weight::end()
        } else {
            self.doc
                .get_at(index)
                .map(|(w, _, _)| w.clone())
                .unwrap_or_else(Weight::end)
        };
        (left, right)
    }

    fn try_stamp(&mut self) -> Result<u64, EngineError> {
        self.local_seq = self.local_seq.checked_add(1).ok_or_else(|| {
            EngineError::new(ErrorCode::IntegerOverflow, "local sequence exhausted")
        })?;
        self.version.note(self.site, self.local_seq);
        Ok(self.local_seq)
    }

    /// Insert one UTF-16 code unit at a CodeMirror-compatible offset.
    pub fn local_insert(&mut self, index: usize, unit: u16) -> Op {
        let attempts = self.doc.len().saturating_add(4_096);
        self.try_local_insert(index, unit, attempts)
            .expect("legacy Replica::local_insert allocation failed")
    }

    /// Typed production insertion. `Document` wraps this in its mutation
    /// journal so an enclosing transaction can roll back without cloning the
    /// document tree or retained operation log.
    pub fn try_local_insert(
        &mut self,
        index: usize,
        unit: u16,
        max_attempts: usize,
    ) -> Result<Op, EngineError> {
        if self.counter == u64::MAX || self.local_seq == u64::MAX {
            return Err(EngineError::new(
                ErrorCode::IntegerOverflow,
                "local operation identity exhausted",
            ));
        }
        let index = index.min(self.doc.len());
        let (left, immediate_right) = self.neighbors(index);
        let w = if let Some(weight) = self.continue_local_insert_run(index, &immediate_right) {
            weight
        } else {
            let mut allocated = None;
            let mut previous_candidate = None;
            for _ in 0..max_attempts.max(1) {
                if let Some(weight) = self.alloc.create_weight(&left, &immediate_right, self.site) {
                    if !self.doc.contains(&weight) {
                        allocated = Some(weight);
                        break;
                    }
                    if previous_candidate.as_ref() == Some(&weight) {
                        break;
                    }
                    previous_candidate = Some(weight);
                } else {
                    break;
                }
            }
            let mut weight = allocated.ok_or_else(|| {
                EngineError::new(
                    ErrorCode::AllocationExhausted,
                    "the requested ESBT gap has no available identifier",
                )
            })?;

            // NEWSEQ can choose the same midpoint at two sites before its
            // fixed-depth tie is needed. Reserve a site-specific child of that
            // midpoint when it remains in the chosen gap, so the run roots do
            // not differ only by the final site tie-break.
            let mut reserved = weight.clone();
            reserved.sc.extend(self.alloc.site_discriminator(self.site));
            let run_reserved =
                left < reserved && reserved < immediate_right && !self.doc.contains(&reserved);
            if run_reserved {
                weight = reserved;
            }
            self.local_insert_run = run_reserved.then(|| LocalInsertRun {
                root: weight.clone(),
                last: weight.clone(),
                last_counter: 0,
                next_component: 1,
            });
            weight
        };
        self.counter = self.counter.checked_add(1).ok_or_else(|| {
            EngineError::new(ErrorCode::IntegerOverflow, "insertion counter exhausted")
        })?;
        let c = self.counter;
        self.insertion_version.note(self.site, c);
        self.counter_map.insert(w.clone(), c);
        let seq = self.try_stamp()?;
        let op = Op::ins(w, unit, c, self.site, seq);
        self.log.insert((self.site, seq), op.clone());
        self.apply_ready(&op);
        if !self.alloc.transaction_active() {
            self.drain_weight(&op.weight);
        }
        if let Some(run) = self.local_insert_run.as_mut() {
            if run.last == op.weight {
                run.last_counter = c;
            }
        }
        Ok(op)
    }

    pub fn local_delete(&mut self, index: usize) -> Option<Op> {
        self.try_local_delete(index).ok().flatten()
    }

    pub fn try_local_delete(&mut self, index: usize) -> Result<Option<Op>, EngineError> {
        self.local_insert_run = None;
        if index >= self.doc.len() {
            return Ok(None);
        }
        let Some((w, _, _)) = self.doc.get_at(index) else {
            return Ok(None);
        };
        let w = w.clone();
        let Some(&c) = self.counter_map.get(&w) else {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "live item is missing its insertion counter",
            ));
        };
        let seq = self.try_stamp()?;
        let op = Op::del(w.clone(), c, self.site, seq);
        self.log.insert((self.site, seq), op.clone());
        self.apply_ready(&op);
        if !self.alloc.transaction_active() {
            self.drain_weight(&op.weight);
        }
        Ok(Some(op))
    }

    /// Reuse an exact released ESBT weight with a fresh insertion counter.
    /// This is the paper's Scenario 3 mechanism and is used by compensating
    /// undo of a deletion.
    pub fn try_local_insert_at_weight(
        &mut self,
        weight: Weight,
        unit: u16,
    ) -> Result<Op, EngineError> {
        self.local_insert_run = None;
        if weight.site == Weight::EMPTY_SITE
            || weight.site != self.site
            || self.doc.contains(&weight)
        {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "exact weight reuse requires its originating site and a released weight",
            ));
        }
        self.counter = self.counter.checked_add(1).ok_or_else(|| {
            EngineError::new(ErrorCode::IntegerOverflow, "insertion counter exhausted")
        })?;
        let counter = self.counter;
        self.insertion_version.note(self.site, counter);
        let sequence = self.try_stamp()?;
        let operation = Op::ins(weight.clone(), unit, counter, self.site, sequence);
        self.counter_map.insert(weight, counter);
        self.log.insert((self.site, sequence), operation.clone());
        self.apply_ready(&operation);
        if !self.alloc.transaction_active() {
            self.drain_weight(&operation.weight);
        }
        Ok(operation)
    }

    fn continue_local_insert_run(&mut self, index: usize, right: &Weight) -> Option<Weight> {
        let run = self.local_insert_run.as_mut()?;
        let (left, _, left_counter) = index.checked_sub(1).and_then(|i| self.doc.get_at(i))?;
        if left != &run.last || left_counter != run.last_counter {
            self.local_insert_run = None;
            return None;
        }

        let mut sc = run.root.sc.clone();
        sc.push(run.next_component);
        let candidate = Weight::new(run.root.f, run.root.sn, sc, self.site);
        if !(run.last < candidate && candidate < *right) || self.doc.contains(&candidate) {
            self.local_insert_run = None;
            return None;
        }

        run.last = candidate.clone();
        run.next_component = run.next_component.checked_add(1)?;
        Some(candidate)
    }

    pub fn local_insert_str(&mut self, index: usize, s: &str) -> Vec<Op> {
        let start = index.min(self.doc.len());
        let mut out = Vec::new();
        for (offset, unit) in s.encode_utf16().enumerate() {
            out.push(self.local_insert(start + offset, unit));
        }
        out
    }

    pub fn local_delete_range(&mut self, start: usize, n: usize) -> Vec<Op> {
        let mut out = Vec::new();
        for _ in 0..n {
            if start >= self.doc.len() {
                break;
            }
            if let Some(op) = self.local_delete(start) {
                out.push(op);
            }
        }
        out
    }

    pub fn receive(&mut self, op: Op) {
        if op.origin == self.site {
            return;
        }
        let _ = self.import_operation(op);
    }

    pub(crate) fn import_operation(&mut self, op: Op) -> Result<bool, EngineError> {
        if let Some(existing) = self.log.get(&(op.origin, op.seq)) {
            if existing == &op {
                return Ok(false);
            }
            return Err(EngineError::new(
                ErrorCode::OperationIdentityConflict,
                "operation identity is already bound to different bytes",
            ));
        }
        // A compact snapshot materializes every operation in its version but
        // intentionally omits the old operation bytes. Network retries for
        // that covered prefix are duplicates and must not be reapplied or
        // appended to a fresh journal.
        if self.version.contains(op.origin, op.seq) {
            return Ok(false);
        }
        if op.origin == 0 || op.seq == 0 || op.counter == 0 || op.weight.site == Weight::EMPTY_SITE
        {
            return Err(EngineError::new(
                ErrorCode::InvalidOperation,
                "operation contains a zero identity",
            ));
        }
        if matches!(op.kind, OpKind::Ins { .. }) {
            if op.origin != op.weight.site {
                return Err(EngineError::new(
                    ErrorCode::InvalidOperation,
                    "insertion origin does not own its ESBT weight",
                ));
            }
            if self.insertion_version.contains(op.weight.site, op.counter) {
                return Err(EngineError::new(
                    ErrorCode::OperationIdentityConflict,
                    "insertion counter is already bound to an earlier insertion",
                ));
            }
        }
        self.admit_validated_operation(op);
        Ok(true)
    }

    /// Reconstruct retained journal and pending state after a full archive has
    /// been structurally decoded and cross-validated against its snapshot.
    pub(crate) fn restore_validated_operations(&mut self, operations: &[Op]) {
        self.admit_validated_operations(operations.iter().cloned());
    }

    /// Apply an operation after the caller has resolved every fallible identity
    /// and resource decision. Keeping this phase infallible lets a document
    /// reject an update before its first mutation without cloning all state.
    pub(crate) fn admit_validated_operation(&mut self, op: Op) {
        self.admit_validated_operations(std::iter::once(op));
    }

    /// Admit an already validated atomic batch and drain each affected weight
    /// once. This keeps a large transaction linear in its own operations rather
    /// than repeatedly rescanning the same causal bucket after every item.
    pub(crate) fn admit_validated_operations(&mut self, operations: impl IntoIterator<Item = Op>) {
        let mut touched = BTreeSet::new();
        for op in operations {
            if matches!(op.kind, OpKind::Ins { .. }) {
                self.insertion_version.note(op.weight.site, op.counter);
            }
            if op.origin == self.site {
                self.local_seq = self.local_seq.max(op.seq);
                if matches!(op.kind, OpKind::Ins { .. }) {
                    self.counter = self.counter.max(op.counter);
                }
            }
            self.log.insert((op.origin, op.seq), op.clone());
            self.version.note(op.origin, op.seq);
            touched.insert(op.weight.clone());
            self.pending.push_back(op);
        }
        for weight in touched {
            self.drain_weight(&weight);
        }
    }

    pub fn drain(&mut self) {
        for weight in self.pending.weights() {
            self.drain_weight(&weight);
        }
    }

    pub(crate) fn drain_weights<'a>(&mut self, weights: impl IntoIterator<Item = &'a Weight>) {
        for weight in weights {
            self.drain_weight(weight);
        }
    }

    fn drain_weight(&mut self, weight: &Weight) {
        let mut pending = self.pending.take(weight);
        let mut progressed = true;
        while progressed {
            progressed = false;
            let mut i = 0;
            while i < pending.len() {
                if self.is_causally_ready(&pending[i]) {
                    let Some(op) = pending.remove(i) else {
                        break;
                    };
                    self.apply_ready(&op);
                    progressed = true;
                } else if self.should_ignore(&pending[i]) {
                    let Some(op) = pending.remove(i) else {
                        break;
                    };
                    match op.kind {
                        OpKind::Del => {
                            // A mismatched delete is a deferred tombstone only
                            // when its target insertion has not arrived or is
                            // still pending behind an older occupant. A fully
                            // resolved insertion receipt makes it redundant.
                            let target_is_pending = pending.iter().any(|pending| {
                                matches!(pending.kind, OpKind::Ins { .. })
                                    && pending.counter == op.counter
                            });
                            if target_is_pending
                                || !self.insertion_version.contains(op.weight.site, op.counter)
                            {
                                self.delete_log.insert((op.weight.clone(), op.counter));
                            }
                        }
                        OpKind::Ins { .. } => {
                            // An older reuse that arrived behind a newer live
                            // occupant can never become visible. Remember that
                            // suppression so its later delete cannot wait
                            // forever after the newer occupant is gone.
                            self.delete_log.remove(&(op.weight.clone(), op.counter));
                        }
                    }
                    progressed = true;
                } else {
                    i += 1;
                }
            }
        }
        self.pending.replace(weight.clone(), pending);
    }

    /// Algorithm 3, refined by Scenario 3: a deletion targets (ω, c),
    /// not ω alone. After reuse, S may contain ω with a newer counter.
    pub fn is_causally_ready(&self, op: &Op) -> bool {
        match op.kind {
            OpKind::Ins { .. } => match self.doc.find(&op.weight) {
                None => true,
                Some((_, counter)) if counter == op.counter => true,
                Some(_) => false,
            },
            OpKind::Del => {
                matches!(self.doc.find(&op.weight), Some((_, c)) if c == op.counter)
            }
        }
    }

    fn should_ignore(&self, op: &Op) -> bool {
        match op.kind {
            OpKind::Ins { .. } => {
                if self.delete_log.contains(&(op.weight.clone(), op.counter)) {
                    return true;
                }
                match self.doc.find(&op.weight) {
                    // Same insertion replay, or an older reuse arriving after
                    // a newer occupant already won this weight.
                    Some((_, counter)) => counter >= op.counter,
                    None => false,
                }
            }
            OpKind::Del => {
                if self.delete_log.contains(&(op.weight.clone(), op.counter)) {
                    return true;
                }
                match self.doc.find(&op.weight) {
                    Some((_, counter)) if counter != op.counter => true,
                    None => self.insertion_version.contains(op.weight.site, op.counter),
                    Some(_) => false,
                }
            }
        }
    }

    fn apply_ready(&mut self, op: &Op) {
        match op.kind {
            OpKind::Ins { unit } => {
                if self.delete_log.remove(&(op.weight.clone(), op.counter)) {
                    return;
                }
                if self.doc.contains(&op.weight) {
                    return;
                }
                if self.doc.insert(op.weight.clone(), unit, op.counter) {
                    self.counter_map.insert(op.weight.clone(), op.counter);
                    self.visible_revision = self.visible_revision.wrapping_add(1);
                }
            }
            OpKind::Del => {
                if self.doc.delete(&op.weight) {
                    self.counter_map.remove(&op.weight);
                    self.visible_revision = self.visible_revision.wrapping_add(1);
                }
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut delete_log: Vec<_> = self.delete_log.iter().cloned().collect();
        delete_log.sort();
        Snapshot {
            atoms: self
                .doc
                .atoms()
                .into_iter()
                .map(|(weight, unit, counter)| Atom {
                    weight,
                    unit,
                    counter,
                })
                .collect(),
            delete_log,
            version: self.version.clone(),
            insertions: self.insertion_version.clone(),
        }
    }

    pub fn install_snapshot(&mut self, snap: &Snapshot) {
        let previous_atoms = self.doc.atoms();
        self.doc = DocTree::default();
        self.counter_map.clear();
        self.delete_log.clear();
        self.pending.clear();
        self.log.clear();
        self.local_insert_run = None;
        for a in &snap.atoms {
            self.doc.insert(a.weight.clone(), a.unit, a.counter);
            self.counter_map.insert(a.weight.clone(), a.counter);
        }
        for (w, c) in &snap.delete_log {
            self.delete_log.insert((w.clone(), *c));
        }
        self.version = snap.version.clone();
        self.insertion_version = snap.insertions.clone();
        // A persisted replica must not reuse operation or insertion identities
        // regardless of which peer serialized the snapshot. A newly joining
        // site is absent from both tables and therefore starts at zero.
        self.local_seq = self.version.highest_seen(self.site);
        self.counter = self.insertion_version.highest_seen(self.site).max(
            snap.delete_log
                .iter()
                .filter(|(weight, _)| weight.site == self.site)
                .map(|(_, counter)| *counter)
                .max()
                .unwrap_or(0),
        );
        if previous_atoms != self.doc.atoms() {
            self.visible_revision = self.visible_revision.wrapping_add(1);
        }
    }

    /// Merge a causally closed compact snapshot without discarding edits the
    /// local replica made (or received) after that snapshot's version.
    ///
    /// The method deliberately uses the snapshot as a new base and replays
    /// retained operations that the base does not contain. Blind state union
    /// is not safe for ESBT weight reuse. If this replica compacted away an
    /// operation the incoming base lacks, the caller must choose a newer
    /// snapshot or restore the missing journal instead of guessing.
    /// Returns `true` when a new compact base was installed and `false` when
    /// the incoming snapshot was an exact advertisement of the current state.
    pub fn merge_snapshot(&mut self, snap: &Snapshot) -> Result<bool, SnapshotMergeError> {
        if !snap.version.is_contiguous() {
            return Err(SnapshotMergeError::SnapshotHasSequenceGaps);
        }

        if self.version.covers(&snap.version) && snap.version.covers(&self.version) {
            if &self.snapshot() == snap {
                return Ok(false);
            }
            return Err(SnapshotMergeError::SnapshotStateConflict);
        }

        let mut preserved: Vec<_> = self
            .log
            .values()
            .filter(|op| !snap.version.contains(op.origin, op.seq))
            .cloned()
            .collect();
        preserved.sort_by_key(|op| (op.origin, op.seq));

        let mut replayed_version = snap.version.clone();
        for op in &preserved {
            replayed_version.note(op.origin, op.seq);
        }
        if !replayed_version.covers(&self.version) {
            return Err(SnapshotMergeError::MissingLocalHistory);
        }

        let local_sequence = self.local_seq;
        let local_counter = self.counter;
        let insert_run = self.local_insert_run.clone();
        self.install_snapshot(snap);
        self.local_seq = self.local_seq.max(local_sequence);
        self.counter = self.counter.max(local_counter);
        for op in preserved {
            // Locally generated operations must be replayed too. `receive`
            // rejects same-site network echoes, so use the common importer.
            if self.import_operation(op).is_err() {
                return Err(SnapshotMergeError::CorruptRetainedHistory);
            }
        }
        self.local_insert_run = insert_run.filter(|run| {
            self.doc
                .find(&run.last)
                .is_some_and(|(_, counter)| counter == run.last_counter)
        });
        Ok(true)
    }

    /// Operations retained by this replica that `peer` has not integrated.
    ///
    /// Comparing membership rather than maximum sequence numbers is what lets
    /// reconnect repair a hole such as "received 2, still missing 1".
    pub fn ops_missing_from(&self, peer: &Version) -> Vec<Op> {
        let mut ops: Vec<_> = self
            .log
            .values()
            .filter(|op| !peer.contains(op.origin, op.seq))
            .cloned()
            .collect();
        ops.sort_by_key(|op| (op.origin, op.seq));
        ops
    }

    pub fn ops_in_range(&self, site: SiteId, from: u64, to: u64) -> Vec<Op> {
        let mut out = Vec::new();
        for seq in from..=to {
            if let Some(op) = self.log.get(&(site, seq)) {
                out.push(op.clone());
            }
        }
        out
    }

    pub fn hash_state(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mix = |h: &mut u64, b: u8| {
            *h ^= b as u64;
            *h = h.wrapping_mul(0x100000001b3);
        };
        for (w, unit, c) in self.doc.atoms() {
            for x in [w.f.p as u64, w.f.q as u64, w.sn as u64, c] {
                for b in x.to_le_bytes() {
                    mix(&mut h, b);
                }
            }
            for b in w.site.to_le_bytes() {
                mix(&mut h, b);
            }
            for d in &w.sc {
                for b in d.to_le_bytes() {
                    mix(&mut h, b);
                }
            }
            for b in unit.to_le_bytes() {
                mix(&mut h, b);
            }
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ReplicaConfig {
        ReplicaConfig {
            dmax: 5,
            base: 10,
            depth: 3,
        }
    }

    #[test]
    fn causal_and_concurrent() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());
        let ins = a.local_insert(0, 'A' as u16);
        let del = a.local_delete(0).unwrap();
        b.receive(del);
        assert_eq!(b.pending.len(), 1);
        b.receive(ins);
        assert_eq!(a.text(), b.text());
        assert_eq!(a.hash_state(), b.hash_state());
    }

    #[test]
    fn late_join_snapshot() {
        let mut a = Replica::new(1, cfg());
        a.local_insert_str(0, "Hello");
        a.local_delete_range(1, 2);
        let mut c = Replica::new(3, cfg());
        c.install_snapshot(&a.snapshot());
        assert_eq!(c.text(), a.text());
        let extra = a.local_insert(3, '!' as u16);
        c.receive(extra);
        assert_eq!(c.text(), a.text());
    }

    #[test]
    fn utf16_offsets_match_javascript_and_codemirror() {
        let mut replica = Replica::new(1, cfg());
        let operations = replica.local_insert_str(0, "a😀b");

        assert_eq!(operations.len(), 4);
        assert_eq!(replica.len(), 4);
        assert_eq!(replica.text(), "a😀b");

        let removed = replica.local_delete_range(1, 2);
        assert_eq!(removed.len(), 2);
        assert_eq!(replica.text(), "ab");
        assert_eq!(replica.len(), 2);
    }

    #[test]
    fn compact_snapshot_merge_preserves_unsynced_local_operations() {
        let mut server = Replica::new(1, cfg());
        server.local_insert_str(0, "base");
        let initial = server.snapshot();

        let mut client = Replica::new(2, cfg());
        client.install_snapshot(&initial);
        client.local_insert_str(client.len(), "-offline");

        server.local_insert_str(server.len(), "-remote");
        let newer = server.snapshot();
        client.merge_snapshot(&newer).expect("merge compact base");

        assert!(client.text().contains("-offline"));
        assert!(client.text().contains("-remote"));

        for op in client.ops_missing_from(&server.version) {
            server.receive(op);
        }
        assert_eq!(client.text(), server.text());
    }

    #[test]
    fn compact_snapshot_merge_fails_when_required_history_was_compacted() {
        let mut older = Replica::new(1, cfg());
        older.local_insert_str(0, "old");
        let old_snapshot = older.snapshot();

        older.local_insert_str(older.len(), "-new");
        let new_snapshot = older.snapshot();
        let mut client = Replica::new(2, cfg());
        client.install_snapshot(&new_snapshot);

        assert_eq!(
            client.merge_snapshot(&old_snapshot),
            Err(SnapshotMergeError::MissingLocalHistory)
        );
        assert_eq!(client.text(), "old-new");
    }

    #[test]
    fn compact_snapshot_merge_rejects_a_sparse_base() {
        let mut source = Replica::new(1, cfg());
        source.local_insert(0, 'A' as u16);
        let second = source.local_insert(1, 'B' as u16);
        let mut partial = Replica::new(2, cfg());
        partial.receive(second);

        let mut target = Replica::new(3, cfg());
        assert_eq!(
            target.merge_snapshot(&partial.snapshot()),
            Err(SnapshotMergeError::SnapshotHasSequenceGaps)
        );
        assert!(target.is_empty());
    }

    #[test]
    fn reconnect_repairs_a_same_site_hole() {
        let mut a = Replica::new(1, cfg());
        let first = a.local_insert(0, 'A' as u16);
        let second = a.local_insert(1, 'B' as u16);

        let mut b = Replica::new(2, cfg());
        b.receive(second);
        assert_eq!(b.text(), "B");
        assert_eq!(b.version.observed(1), 0);
        assert!(!b.version.contains(1, 1));
        assert!(b.version.contains(1, 2));

        let missing = a.ops_missing_from(&b.version);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].seq, first.seq);
        b.receive(missing[0].clone());

        assert_eq!(b.text(), "AB");
        assert_eq!(b.text(), a.text());
        assert_eq!(b.version.observed(1), 2);
    }

    #[test]
    fn every_same_site_insert_permutation_converges() {
        let mut a = Replica::new(1, cfg());
        let ops = [
            a.local_insert(0, 'A' as u16),
            a.local_insert(1, 'B' as u16),
            a.local_insert(2, 'C' as u16),
        ];
        let permutations = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];

        for (index, order) in permutations.into_iter().enumerate() {
            let mut replica = Replica::new(10 + index as u128, cfg());
            for op_index in order {
                replica.receive(ops[op_index].clone());
            }
            assert_eq!(replica.text(), "ABC", "order {order:?}");
            assert_eq!(replica.version.observed(1), 3, "order {order:?}");
            assert!(a.ops_missing_from(&replica.version).is_empty());
        }
    }

    #[test]
    fn concurrent_typing_keeps_each_word_contiguous() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());

        let from_a = a.local_insert_str(0, "cat");
        let from_b = b.local_insert_str(0, "dog");

        for op in from_b.into_iter().rev() {
            a.receive(op);
        }
        for op in from_a.into_iter().rev() {
            b.receive(op);
        }

        assert_eq!(a.text(), b.text());
        assert!(
            matches!(a.text().as_str(), "catdog" | "dogcat"),
            "concurrent words interleaved as {:?}",
            a.text()
        );
    }

    fn index_after(replica: &Replica, weight: &Weight) -> usize {
        replica
            .doc
            .atoms()
            .iter()
            .position(|(candidate, _, _)| candidate == weight)
            .expect("local insertion remains visible")
            + 1
    }

    #[test]
    fn live_concurrent_keystrokes_keep_each_word_contiguous() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());

        let c = a.local_insert(0, 'c' as u16);
        let d = b.local_insert(0, 'd' as u16);
        a.receive(d.clone());
        b.receive(c.clone());

        let a_char = a.local_insert(index_after(&a, &c.weight), 'a' as u16);
        let o_char = b.local_insert(index_after(&b, &d.weight), 'o' as u16);
        a.receive(o_char.clone());
        b.receive(a_char.clone());

        let t = a.local_insert(index_after(&a, &a_char.weight), 't' as u16);
        let g = b.local_insert(index_after(&b, &o_char.weight), 'g' as u16);
        a.receive(g);
        b.receive(t);

        assert_eq!(a.text(), b.text());
        assert!(
            matches!(a.text().as_str(), "catdog" | "dogcat"),
            "live concurrent words interleaved as {:?}",
            a.text()
        );
    }

    #[test]
    fn concurrent_words_stay_contiguous_inside_existing_text() {
        let mut seed = Replica::new(9, cfg());
        seed.local_insert_str(0, "[]");
        let snapshot = seed.snapshot();

        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());
        a.install_snapshot(&snapshot);
        b.install_snapshot(&snapshot);

        let from_a = a.local_insert_str(1, "cat");
        let from_b = b.local_insert_str(1, "dog");
        for op in from_b.into_iter().rev() {
            a.receive(op);
        }
        for op in from_a.into_iter().rev() {
            b.receive(op);
        }

        assert_eq!(a.text(), b.text());
        assert!(
            matches!(a.text().as_str(), "[catdog]" | "[dogcat]"),
            "nested concurrent words interleaved as {:?}",
            a.text()
        );
    }

    #[test]
    fn three_concurrent_runs_converge_without_interleaving() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());
        let mut c = Replica::new(3, cfg());
        let from_a = a.local_insert_str(0, "red");
        let from_b = b.local_insert_str(0, "green");
        let from_c = c.local_insert_str(0, "blue");

        for op in from_c.iter().chain(from_b.iter().rev()) {
            a.receive(op.clone());
        }
        for op in from_a.iter().rev().chain(from_c.iter()) {
            b.receive(op.clone());
        }
        for op in from_b.iter().chain(from_a.iter().rev()) {
            c.receive(op.clone());
        }

        assert_eq!(a.text(), b.text());
        assert_eq!(b.text(), c.text());
        for run in ["red", "green", "blue"] {
            assert!(
                a.text().contains(run),
                "run {run:?} split in {:?}",
                a.text()
            );
        }
    }

    #[test]
    fn concurrent_range_deletes_converge() {
        let mut seed = Replica::new(9, cfg());
        seed.local_insert_str(0, "abcdef");
        let snapshot = seed.snapshot();
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());
        a.install_snapshot(&snapshot);
        b.install_snapshot(&snapshot);

        let from_a = a.local_delete_range(1, 2);
        let from_b = b.local_delete_range(3, 2);
        for op in from_b.into_iter().rev() {
            a.receive(op);
        }
        for op in from_a.into_iter().rev() {
            b.receive(op);
        }

        assert_eq!(a.text(), "af");
        assert_eq!(a.text(), b.text());
    }

    fn next_random(state: &mut u32) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        *state
    }

    #[test]
    fn randomized_three_replica_delivery_converges_with_duplicates() {
        for seed in 1..=24u32 {
            let mut replicas = [
                Replica::new(1, cfg()),
                Replica::new(2, cfg()),
                Replica::new(3, cfg()),
            ];
            let mut state = seed.wrapping_mul(0x9e37_79b9);

            for step in 0..200 {
                let target = (next_random(&mut state) as usize) % replicas.len();
                let len = replicas[target].len();
                if len == 0 || !next_random(&mut state).is_multiple_of(4) {
                    let index = (next_random(&mut state) as usize) % (len + 1);
                    let unit = u16::from(b'a' + (step % 26) as u8);
                    replicas[target].local_insert(index, unit);
                } else {
                    let index = (next_random(&mut state) as usize) % len;
                    replicas[target].local_delete(index);
                }
            }

            let mut all_ops: Vec<Op> = replicas
                .iter()
                .flat_map(|replica| replica.log.values().cloned())
                .collect();
            all_ops.sort_by_key(|op| (op.origin, op.seq));
            for (target, replica) in replicas.iter_mut().enumerate() {
                let mut order = all_ops.clone();
                let mut shuffle = seed ^ ((target as u32 + 1) * 0x045d_9f3b);
                for i in (1..order.len()).rev() {
                    let j = (next_random(&mut shuffle) as usize) % (i + 1);
                    order.swap(i, j);
                }
                for op in order {
                    replica.receive(op.clone());
                    if next_random(&mut shuffle).is_multiple_of(7) {
                        replica.receive(op);
                    }
                }
                assert!(
                    replica.pending.is_empty(),
                    "seed {seed}, replica {target}, pending {:?}",
                    replica.pending
                );
            }

            let expected = replicas[0].text();
            assert_eq!(replicas[1].text(), expected, "seed {seed}, replica 1");
            assert_eq!(replicas[2].text(), expected, "seed {seed}, replica 2");
        }
    }

    #[test]
    fn local_insertions_preserve_the_requested_index_across_allocator_fallbacks() {
        let mut replica = Replica::new(1, cfg());
        let mut expected = Vec::new();
        let mut state = 0x9e37_79b9u32;

        for step in 0..500 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let index = (state as usize) % (expected.len() + 1);
            let unit = u16::from(b'a' + (step % 26) as u8);

            let (left, right) = replica.neighbors(index);
            let op = replica.local_insert(index, unit);
            assert!(
                left < op.weight && op.weight < right,
                "allocator escaped ({left}, {right}) with {} at insertion {step}",
                op.weight
            );
            expected.insert(index, unit);

            assert_eq!(
                replica.text(),
                String::from_utf16(&expected).expect("ASCII fixture is valid UTF-16"),
                "requested index was lost at insertion {step}"
            );
        }
    }

    #[test]
    fn inserting_between_concurrent_twins_never_drops_a_character() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());

        let from_a = a.local_insert(0, 'x' as u16);
        let from_b = b.local_insert(0, 'y' as u16);
        a.receive(from_b.clone());
        b.receive(from_a.clone());
        assert_eq!(a.text(), b.text());

        let middle_a = a.local_insert(1, 'M' as u16);
        let middle_b = b.local_insert(1, 'N' as u16);
        a.receive(middle_b);
        b.receive(middle_a);

        assert_eq!(a.text(), b.text());
        assert_eq!(a.len(), 4);
    }

    #[test]
    fn snapshot_roundtrip_preserves_a_reconnect_hole() {
        let mut a = Replica::new(1, cfg());
        let first = a.local_insert(0, 'A' as u16);
        let second = a.local_insert(1, 'B' as u16);

        let mut partial = Replica::new(2, cfg());
        partial.receive(second);
        let encoded = partial.snapshot().encode();
        let decoded = Snapshot::decode(&encoded).expect("decode snapshot");
        let mut restored = Replica::new(3, cfg());
        restored.install_snapshot(&decoded);

        assert_eq!(restored.version.observed(1), 0);
        assert!(!restored.version.contains(1, 1));
        assert!(restored.version.contains(1, 2));
        let missing = a.ops_missing_from(&restored.version);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].seq, first.seq);

        restored.receive(missing[0].clone());
        assert_eq!(restored.text(), "AB");
    }

    #[test]
    fn stable_site_resumes_counters_after_snapshot_restore() {
        let mut before = Replica::new(1, cfg());
        before.local_insert(0, 'A' as u16);
        before.local_insert(1, 'B' as u16);
        let snapshot = before.snapshot();

        let mut after = Replica::new(1, cfg());
        after.install_snapshot(&snapshot);
        let next = after.local_insert(2, 'C' as u16);

        assert_eq!(next.seq, 3);
        assert_eq!(next.counter, 3);
        assert_eq!(after.text(), "ABC");
    }

    #[test]
    fn three_way_sec() {
        let mut a = Replica::new(1, ReplicaConfig::default());
        let mut b = Replica::new(2, ReplicaConfig::default());
        let mut c = Replica::new(3, ReplicaConfig::default());
        let s = a.local_insert(0, '·' as u16);
        b.receive(s.clone());
        c.receive(s);
        let ia = a.local_insert(0, 'A' as u16);
        let ib = b.local_insert(1, 'B' as u16);
        let ic = c.local_insert(1, 'C' as u16);
        for r in [&mut a, &mut b, &mut c] {
            r.receive(ia.clone());
            r.receive(ib.clone());
            r.receive(ic.clone());
        }
        assert_eq!(a.hash_state(), b.hash_state());
        assert_eq!(b.hash_state(), c.hash_state());
    }

    #[test]
    fn reuse_after_delete() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());
        let i1 = a.local_insert(0, 'A' as u16);
        let d1 = a.local_delete(0).unwrap();
        let i2 = a.local_insert(0, 'B' as u16);
        b.receive(i2.clone());
        b.receive(d1.clone());
        b.receive(i1.clone());
        assert_eq!(a.text(), "B");
        assert_eq!(b.text(), "B");
        assert_ne!(i1.counter, i2.counter);
    }

    #[test]
    fn newer_reuse_waits_until_the_old_occupant_is_deleted() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());
        let old_insert = a.local_insert(0, 'A' as u16);
        let old_delete = a.local_delete(0).expect("delete old value");
        let new_insert = a.local_insert(0, 'B' as u16);
        assert_eq!(old_insert.weight, new_insert.weight);

        b.receive(old_insert);
        b.receive(new_insert);
        assert_eq!(b.text(), "A");
        assert_eq!(b.pending.len(), 1);

        b.receive(old_delete);
        assert_eq!(b.text(), "B");
        assert!(b.pending.is_empty());
        assert_eq!(a.text(), b.text());
    }

    #[test]
    fn superseded_reuse_does_not_leave_its_delete_pending() {
        let mut a = Replica::new(1, cfg());
        let mut b = Replica::new(2, cfg());
        let old_insert = a.local_insert(0, 'A' as u16);
        let old_delete = a.local_delete(0).expect("delete old value");
        let new_insert = a.local_insert(0, 'B' as u16);
        let new_delete = a.local_delete(0).expect("delete new value");

        b.receive(new_insert);
        b.receive(old_insert);
        b.receive(new_delete);
        b.receive(old_delete);

        assert!(b.text().is_empty());
        assert!(b.pending.is_empty());
        assert_eq!(a.text(), b.text());
    }
}
