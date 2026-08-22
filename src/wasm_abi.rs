//! Production opaque-document Wasm C ABI.

use crate::anchor::{Affinity, Anchor};
use crate::clock::Version;
use crate::config::DocumentConfig;
use crate::document::{Document, LocalUpdate, SnapshotKind, SnapshotReceipt, UndoDisposition};
use crate::error::{EngineError, ErrorCode};
use std::cell::RefCell;
use std::collections::HashMap;

const MAX_DOCUMENTS: usize = 1_024;
const MAX_ABI_ALLOCATION: usize = 16 * 1024 * 1024;

thread_local! {
    static LAST: RefCell<Vec<u8>> = RefCell::new(Vec::new());
    static LAST_ERROR: RefCell<u32> = const { RefCell::new(0) };
    static ALLOCATIONS: RefCell<HashMap<usize, (usize, usize)>> = RefCell::new(HashMap::new());
    static DOCUMENTS: RefCell<DocumentStore> = RefCell::new(DocumentStore::default());
}

#[derive(Default)]
struct DocumentStore {
    next_handle: u32,
    documents: HashMap<u32, Document>,
}

impl DocumentStore {
    fn insert(&mut self, document: Document) -> Result<u32, EngineError> {
        if self.documents.len() >= MAX_DOCUMENTS {
            return Err(EngineError::new(
                ErrorCode::InvalidHandle,
                "Wasm document handle limit reached",
            ));
        }
        for _ in 0..=MAX_DOCUMENTS {
            self.next_handle = self.next_handle.wrapping_add(1).max(1);
            if !self.documents.contains_key(&self.next_handle) {
                let handle = self.next_handle;
                self.documents.insert(handle, document);
                return Ok(handle);
            }
        }
        Err(EngineError::new(
            ErrorCode::InvalidHandle,
            "no free Wasm document handle",
        ))
    }
}

fn store(b: Vec<u8>) -> i32 {
    LAST.with(|l| {
        let n = b.len() as i32;
        *l.borrow_mut() = b;
        n
    })
}

fn clear_error() {
    LAST_ERROR.with(|code| *code.borrow_mut() = 0);
}

fn fail(error: EngineError) -> i32 {
    LAST_ERROR.with(|code| *code.borrow_mut() = error.code as u32);
    store(error.to_string().into_bytes());
    -(error.code as i32)
}

fn invalid_handle() -> EngineError {
    EngineError::new(ErrorCode::InvalidHandle, "unknown document handle")
}

fn with_document<R>(
    handle: u32,
    operation: impl FnOnce(&mut Document) -> Result<R, EngineError>,
) -> Result<R, EngineError> {
    DOCUMENTS.with(|documents| {
        let mut documents = documents.borrow_mut();
        let document = documents
            .documents
            .get_mut(&handle)
            .ok_or_else(invalid_handle)?;
        operation(document)
    })
}

fn site_from_words(words: [u32; 4]) -> u128 {
    u128::from(words[0])
        | (u128::from(words[1]) << 32)
        | (u128::from(words[2]) << 64)
        | (u128::from(words[3]) << 96)
}

fn group_from_words(has_group: u32, low: u32, high: u32) -> Option<u64> {
    (has_group != 0).then_some(u64::from(low) | (u64::from(high) << 32))
}

fn read_input(pointer: *const u8, length: u32) -> Result<Vec<u8>, EngineError> {
    let length = length as usize;
    if length > MAX_ABI_ALLOCATION {
        return Err(EngineError::new(
            ErrorCode::MessageTooLarge,
            "ABI input exceeds allocation policy",
        ));
    }
    if length == 0 {
        return Ok(Vec::new());
    }
    if pointer.is_null() {
        return Err(EngineError::malformed("null ABI input pointer"));
    }
    let start = pointer as usize;
    let end = start.checked_add(length).ok_or_else(|| {
        EngineError::new(ErrorCode::IntegerOverflow, "ABI pointer range overflow")
    })?;
    let memory_bytes = core::arch::wasm32::memory_size::<0>()
        .checked_mul(65_536)
        .ok_or_else(|| EngineError::new(ErrorCode::IntegerOverflow, "Wasm memory overflow"))?;
    if end > memory_bytes {
        return Err(EngineError::malformed(
            "ABI input range is outside Wasm memory",
        ));
    }
    // The range is within the module's linear memory. Copy before any call
    // that might grow memory and invalidate a borrowed view.
    Ok(unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec())
}

fn decode_utf16(bytes: &[u8]) -> Result<Vec<u16>, EngineError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(EngineError::malformed(
            "UTF-16 input has an odd byte length",
        ));
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect())
}

fn encode_utf16(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len().saturating_mul(2));
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

fn store_local_update(update: Option<LocalUpdate>) -> i32 {
    clear_error();
    match update {
        Some(update) => {
            store(update.canonical_bytes);
            1
        }
        None => {
            store(Vec::new());
            0
        }
    }
}

fn encode_snapshot_receipt(receipt: &SnapshotReceipt) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_le_bytes());
    out.push(match receipt.kind {
        SnapshotKind::Full => 1,
        SnapshotKind::Compact => 2,
    });
    out.push(u8::from(receipt.visible_changed));
    out.push(match receipt.undo {
        UndoDisposition::Preserved => 1,
        UndoDisposition::Cleared => 2,
        UndoDisposition::PartiallyPreserved => 3,
    });
    let version = receipt.version.encode();
    out.extend_from_slice(&(version.len() as u32).to_le_bytes());
    out.extend_from_slice(&version);
    out
}

#[no_mangle]
pub extern "C" fn esbt_malloc(n: u32) -> *mut u8 {
    let length = n as usize;
    if length == 0 || length > MAX_ABI_ALLOCATION {
        return core::ptr::null_mut();
    }
    let mut v = Vec::new();
    if v.try_reserve_exact(length).is_err() {
        return core::ptr::null_mut();
    }
    v.resize(length, 0u8);
    let p = v.as_mut_ptr();
    let capacity = v.capacity();
    std::mem::forget(v);
    ALLOCATIONS.with(|allocations| {
        allocations
            .borrow_mut()
            .insert(p as usize, (length, capacity));
    });
    p
}

#[no_mangle]
pub unsafe extern "C" fn esbt_free(p: *mut u8, n: u32) {
    if p.is_null() || n == 0 {
        return;
    }
    let length = n as usize;
    let capacity = ALLOCATIONS.with(|allocations| {
        let mut allocations = allocations.borrow_mut();
        match allocations.get(&(p as usize)).copied() {
            Some((owned_length, capacity)) if owned_length == length => {
                allocations.remove(&(p as usize));
                Some(capacity)
            }
            _ => None,
        }
    });
    if let Some(capacity) = capacity {
        drop(Vec::from_raw_parts(p, length, capacity));
    }
}

#[no_mangle]
pub extern "C" fn esbt_last_len() -> i32 {
    LAST.with(|l| l.borrow().len() as i32)
}

#[no_mangle]
pub extern "C" fn esbt_last_ptr() -> *const u8 {
    LAST.with(|l| l.borrow().as_ptr())
}

/* ------------------------------------------------------------------------- */
/* Production opaque-document ABI                                           */

#[no_mangle]
pub extern "C" fn esbt_doc_last_error_code() -> u32 {
    LAST_ERROR.with(|code| *code.borrow())
}

#[no_mangle]
pub extern "C" fn esbt_doc_create(site_0: u32, site_1: u32, site_2: u32, site_3: u32) -> i32 {
    let site = site_from_words([site_0, site_1, site_2, site_3]);
    match Document::with_defaults(site)
        .and_then(|document| DOCUMENTS.with(|documents| documents.borrow_mut().insert(document)))
    {
        Ok(handle) => {
            clear_error();
            handle as i32
        }
        Err(error) => fail(error),
    }
}

/// Create a document from an encoded `DocumentConfig` (see `src/config.rs`
/// for the exact byte layout). This is the browser's access to every policy
/// native callers already have: `Dmax`, base, depth, the allocation
/// strategy, the adaptive-`Dmax` controller, and per-document resource
/// ceilings. Malformed or non-canonical config bytes fail typed.
#[no_mangle]
pub extern "C" fn esbt_doc_create_configured(
    site_0: u32,
    site_1: u32,
    site_2: u32,
    site_3: u32,
    config_pointer: *const u8,
    config_length: u32,
) -> i32 {
    let site = site_from_words([site_0, site_1, site_2, site_3]);
    let bytes = match read_input(config_pointer, config_length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    match DocumentConfig::decode(&bytes)
        .and_then(|config| Document::new(site, config.replica, config.limits))
        .and_then(|document| DOCUMENTS.with(|documents| documents.borrow_mut().insert(document)))
    {
        Ok(handle) => {
            clear_error();
            handle as i32
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_destroy(handle: u32) -> i32 {
    let removed = DOCUMENTS.with(|documents| documents.borrow_mut().documents.remove(&handle));
    if removed.is_some() {
        clear_error();
        0
    } else {
        fail(invalid_handle())
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_len(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.len())) {
        Ok(length) => {
            clear_error();
            length as i32
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_hash(handle: u32) -> u32 {
    match with_document(handle, |document| Ok(document.state_hash())) {
        Ok(hash) => {
            clear_error();
            hash as u32
        }
        Err(error) => {
            fail(error);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_pending(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.pending_len())) {
        Ok(length) => {
            clear_error();
            length as i32
        }
        Err(error) => fail(error),
    }
}

/// Store exact little-endian UTF-16 units in the shared result buffer.
#[no_mangle]
pub extern "C" fn esbt_doc_text_utf16(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.utf16_units())) {
        Ok(units) => {
            clear_error();
            store(encode_utf16(&units))
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_site(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.site())) {
        Ok(site) => {
            clear_error();
            store(site.to_le_bytes().to_vec())
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_version(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.version().encode())) {
        Ok(version) => {
            clear_error();
            store(version)
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_begin(
    handle: u32,
    has_undo_group: u32,
    group_low: u32,
    group_high: u32,
) -> i32 {
    let group = group_from_words(has_undo_group, group_low, group_high);
    match with_document(handle, |document| document.begin_transaction(group)) {
        Ok(()) => {
            clear_error();
            store(Vec::new());
            0
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_commit(handle: u32) -> i32 {
    match with_document(handle, Document::commit_transaction) {
        Ok(update) => store_local_update(update),
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_abort(handle: u32) -> i32 {
    match with_document(handle, Document::abort_transaction) {
        Ok(()) => {
            clear_error();
            store(Vec::new());
            0
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_insert_utf16(
    handle: u32,
    index: u32,
    pointer: *const u8,
    byte_length: u32,
    has_undo_group: u32,
    group_low: u32,
    group_high: u32,
) -> i32 {
    let bytes = match read_input(pointer, byte_length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    let units = match decode_utf16(&bytes) {
        Ok(units) => units,
        Err(error) => return fail(error),
    };
    let group = group_from_words(has_undo_group, group_low, group_high);
    match with_document(handle, |document| {
        document.insert_utf16(index as usize, &units, group)
    }) {
        Ok(update) => store_local_update(update),
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_delete(
    handle: u32,
    index: u32,
    length: u32,
    has_undo_group: u32,
    group_low: u32,
    group_high: u32,
) -> i32 {
    let group = group_from_words(has_undo_group, group_low, group_high);
    match with_document(handle, |document| {
        document.delete(index as usize, length as usize, group)
    }) {
        Ok(update) => store_local_update(update),
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_replace_utf16(
    handle: u32,
    from: u32,
    to: u32,
    pointer: *const u8,
    byte_length: u32,
    has_undo_group: u32,
    group_low: u32,
    group_high: u32,
) -> i32 {
    let bytes = match read_input(pointer, byte_length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    let units = match decode_utf16(&bytes) {
        Ok(units) => units,
        Err(error) => return fail(error),
    };
    let group = group_from_words(has_undo_group, group_low, group_high);
    match with_document(handle, |document| {
        document.replace_range_utf16(from as usize, to as usize, &units, group)
    }) {
        Ok(update) => store_local_update(update),
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_apply(handle: u32, pointer: *const u8, length: u32) -> i32 {
    let bytes = match read_input(pointer, length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    match with_document(handle, |document| document.apply_bytes(&bytes)) {
        Ok(receipt) => {
            clear_error();
            store(receipt.encode())
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_export_update(
    handle: u32,
    version_pointer: *const u8,
    version_length: u32,
) -> i32 {
    let bytes = match read_input(version_pointer, version_length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    match with_document(handle, |document| {
        let version = Version::decode_with_limits(&bytes, document.limits())?;
        document.export_update(&version)
    }) {
        Ok(update) => {
            clear_error();
            store(update)
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_export_full_snapshot(handle: u32) -> i32 {
    match with_document(handle, |document| document.export_full_snapshot()) {
        Ok(snapshot) => {
            clear_error();
            store(snapshot)
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_export_compact_snapshot(handle: u32) -> i32 {
    match with_document(handle, |document| document.export_compact_snapshot()) {
        Ok(snapshot) => {
            clear_error();
            store(snapshot)
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_apply_snapshot(handle: u32, pointer: *const u8, length: u32) -> i32 {
    let bytes = match read_input(pointer, length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    match with_document(handle, |document| document.apply_snapshot_bytes(&bytes)) {
        Ok(receipt) => {
            clear_error();
            store(encode_snapshot_receipt(&receipt))
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_anchor(handle: u32, index: u32, affinity: u32) -> i32 {
    let affinity = match affinity {
        1 => Affinity::Before,
        2 => Affinity::After,
        _ => {
            return fail(EngineError::new(
                ErrorCode::InvalidAnchor,
                "unknown anchor affinity",
            ))
        }
    };
    match with_document(handle, |document| {
        Ok(document.anchor(index as usize, affinity)?.encode())
    }) {
        Ok(anchor) => {
            clear_error();
            store(anchor)
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_resolve_anchor(handle: u32, pointer: *const u8, length: u32) -> i32 {
    let bytes = match read_input(pointer, length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    match with_document(handle, |document| {
        let anchor = Anchor::decode_with_limits(&bytes, document.limits())?;
        Ok(document.resolve_anchor(&anchor))
    }) {
        Ok(index) => {
            clear_error();
            index as i32
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_insert_at_anchor_utf16(
    handle: u32,
    anchor_pointer: *const u8,
    anchor_length: u32,
    text_pointer: *const u8,
    text_byte_length: u32,
    has_undo_group: u32,
    group_low: u32,
    group_high: u32,
) -> i32 {
    let anchor_bytes = match read_input(anchor_pointer, anchor_length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    let text_bytes = match read_input(text_pointer, text_byte_length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    let units = match decode_utf16(&text_bytes) {
        Ok(units) => units,
        Err(error) => return fail(error),
    };
    let group = group_from_words(has_undo_group, group_low, group_high);
    match with_document(handle, |document| {
        let anchor = Anchor::decode_with_limits(&anchor_bytes, document.limits())?;
        document.insert_utf16_at_anchor(&anchor, &units, group)
    }) {
        Ok((update, caret)) => {
            let anchor = caret.encode();
            let update = update
                .map(|update| update.canonical_bytes)
                .unwrap_or_default();
            let mut result = Vec::new();
            result.extend_from_slice(&(anchor.len() as u32).to_le_bytes());
            result.extend_from_slice(&anchor);
            result.extend_from_slice(&(update.len() as u32).to_le_bytes());
            result.extend_from_slice(&update);
            clear_error();
            store(result)
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_can_undo(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.can_undo())) {
        Ok(value) => {
            clear_error();
            i32::from(value)
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_can_redo(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.can_redo())) {
        Ok(value) => {
            clear_error();
            i32::from(value)
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_undo(handle: u32) -> i32 {
    match with_document(handle, Document::undo) {
        Ok(update) => store_local_update(update),
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_redo(handle: u32) -> i32 {
    match with_document(handle, Document::redo) {
        Ok(update) => store_local_update(update),
        Err(error) => fail(error),
    }
}

/// Operations retained for reconnect/delta export — the quantity that grows
/// without bound unless the client drives `esbt_doc_prune_history`. Poll it
/// to schedule compaction.
#[no_mangle]
pub extern "C" fn esbt_doc_retained_operations(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.retained_operations())) {
        Ok(count) => {
            clear_error();
            count.min(i32::MAX as usize) as i32
        }
        Err(error) => fail(error),
    }
}

/// Store the encoded history floor: the causal prefix below which this
/// document can no longer serve reconnect deltas and peers need a snapshot.
#[no_mangle]
pub extern "C" fn esbt_doc_history_floor(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.history_floor().encode())) {
        Ok(floor) => {
            clear_error();
            store(floor)
        }
        Err(error) => fail(error),
    }
}

/// Store the current `Dmax` as 8 little-endian bytes (it can exceed `i32`
/// when the adaptive controller reaches its ceiling).
#[no_mangle]
pub extern "C" fn esbt_doc_current_dmax(handle: u32) -> i32 {
    match with_document(handle, |document| Ok(document.current_dmax())) {
        Ok(dmax) => {
            clear_error();
            store(dmax.to_le_bytes().to_vec())
        }
        Err(error) => fail(error),
    }
}

#[no_mangle]
pub extern "C" fn esbt_doc_prune_history(
    handle: u32,
    version_pointer: *const u8,
    version_length: u32,
) -> i32 {
    let bytes = match read_input(version_pointer, version_length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(error),
    };
    match with_document(handle, |document| {
        let version = Version::decode_with_limits(&bytes, document.limits())?;
        document.prune_history_through(&version)
    }) {
        Ok(pruned) => {
            clear_error();
            pruned.min(i32::MAX as usize) as i32
        }
        Err(error) => fail(error),
    }
}
