/** Production browser owner for the Rust `Document` Wasm API. */

import { checkedEsbtExports } from "./esbt-abi.generated.js";

const textDecoder = new TextDecoder();
const MAX_ABI_BYTES = 64 * 1024 * 1024;

export class EsbtError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "EsbtError";
    this.code = code;
  }
}

export class EsbtRuntime {
  constructor(exports) {
    this.exports = exports;
  }

  static async load(url = "./esbt.wasm") {
    const response = await fetch(url);
    if (!response.ok) throw new Error(`esbt: failed to fetch Wasm (${response.status})`);
    const fallback = response.clone();
    if (typeof WebAssembly.instantiateStreaming === "function") {
      try {
        const { module, instance } = await WebAssembly.instantiateStreaming(response, { env: {} });
        return new EsbtRuntime(checkedEsbtExports(module, instance.exports));
      } catch {
        // Development servers sometimes omit application/wasm.
      }
    }
    const { module, instance } = await WebAssembly.instantiate(await fallback.arrayBuffer(), { env: {} });
    return new EsbtRuntime(checkedEsbtExports(module, instance.exports));
  }

  memory() {
    return new Uint8Array(this.exports.memory.buffer);
  }

  last() {
    const length = this.exports.esbt_last_len();
    const pointer = this.exports.esbt_last_ptr();
    return this.memory().slice(pointer, pointer + length);
  }

  check(result) {
    if (result >= 0) return result;
    const code = this.exports.esbt_doc_last_error_code() >>> 0;
    throw new EsbtError(code, textDecoder.decode(this.last()) || `esbt error ${code}`);
  }

  withBytes(bytes, callback) {
    const input = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    if (input.length > MAX_ABI_BYTES) {
      throw new EsbtError(7, "esbt: input exceeds the Wasm message limit");
    }
    if (input.length === 0) return callback(0, 0);
    const pointer = this.exports.esbt_malloc(input.length);
    if (!pointer) throw new EsbtError(7, "esbt: Wasm input allocation failed");
    try {
      this.memory().set(input, pointer);
      return callback(pointer, input.length);
    } finally {
      this.exports.esbt_free(pointer, input.length);
    }
  }

  withTwoBuffers(first, second, callback) {
    return this.withBytes(first, (firstPointer, firstLength) =>
      this.withBytes(second, (secondPointer, secondLength) =>
        callback(firstPointer, firstLength, secondPointer, secondLength),
      ),
    );
  }
}

// Canonical LEB128, matching the engine codec (non-minimal forms rejected).
function pushVarint(bytes, value) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new EsbtError(4, "esbt: config values must be non-negative safe integers");
  }
  do {
    const group = value % 128;
    value = Math.floor(value / 128);
    bytes.push(value > 0 ? group | 0x80 : group);
  } while (value > 0);
}

const STRATEGY_TAGS = {
  midpoint: 0,
  "boundary-low": 1,
  "boundary-high": 2,
  "alternating-by-depth": 3,
};

const LIMIT_FIELDS = [
  "maxMessageBytes",
  "maxOperationsPerUpdate",
  "maxIdentifierDepth",
  "maxVersionSites",
  "maxSparseReceipts",
  "maxSnapshotItems",
  "maxPendingOperations",
  "maxDeferredDeletes",
  "maxDocumentUnits",
  "maxAllocationAttempts",
  "maxRetainedOperations",
  "maxUndoTransactions",
];

const DEFAULT_LIMITS = {
  maxMessageBytes: 16 * 1024 * 1024,
  maxOperationsPerUpdate: 100_000,
  maxIdentifierDepth: 1_024,
  maxVersionSites: 65_536,
  maxSparseReceipts: 1_000_000,
  maxSnapshotItems: 2_000_000,
  maxPendingOperations: 250_000,
  maxDeferredDeletes: 2_000_000,
  maxDocumentUnits: 2_000_000,
  maxAllocationAttempts: 65_536,
  maxRetainedOperations: 4_000_000,
  maxUndoTransactions: 10_000,
};

/**
 * Encode a document configuration for `esbt_doc_create_configured`
 * (config format v1; the exact layout is documented in `src/config.rs`).
 *
 * All fields are optional; defaults match `Document.with_defaults`:
 * `{ dmax, base, depth, strategy: { kind, boundary }, adaptiveDmax:
 * { floor, ceiling, window, holdoffWindows }, limits: { ...LIMIT_FIELDS } }`.
 */
export function encodeDocumentConfig(config = {}) {
  const bytes = [1, 0]; // format version u16 LE
  const flags = (config.adaptiveDmax ? 0b01 : 0) | 0b10; // limits always sent
  bytes.push(flags);
  pushVarint(bytes, config.dmax ?? 65_536);
  pushVarint(bytes, config.base ?? 2_147_483_647);
  pushVarint(bytes, config.depth ?? 256);
  const strategy = config.strategy ?? { kind: "midpoint" };
  const tag = STRATEGY_TAGS[strategy.kind];
  if (tag === undefined) throw new EsbtError(4, `esbt: unknown strategy ${strategy.kind}`);
  bytes.push(tag);
  if (tag !== 0) pushVarint(bytes, strategy.boundary ?? 64);
  if (config.adaptiveDmax) {
    pushVarint(bytes, config.adaptiveDmax.floor ?? 16);
    pushVarint(bytes, config.adaptiveDmax.ceiling ?? 2_147_483_648);
    pushVarint(bytes, config.adaptiveDmax.window ?? 256);
    pushVarint(bytes, config.adaptiveDmax.holdoffWindows ?? 4);
  }
  const limits = { ...DEFAULT_LIMITS, ...(config.limits ?? {}) };
  for (const field of LIMIT_FIELDS) pushVarint(bytes, limits[field]);
  return new Uint8Array(bytes);
}

export class EsbtDocument {
  static async create(options = {}) {
    const runtime = options.runtime ?? (await EsbtRuntime.load(options.wasmUrl));
    const siteWords = normalizeSiteId(options.siteId);
    const handle = options.config
      ? runtime.withBytes(encodeDocumentConfig(options.config), (pointer, length) =>
          runtime.check(
            runtime.exports.esbt_doc_create_configured(...siteWords, pointer, length),
          ),
        )
      : runtime.check(runtime.exports.esbt_doc_create(...siteWords));
    if (handle === 0) throw new EsbtError(24, "esbt: document creation returned no handle");
    return new EsbtDocument(runtime, handle, siteWords);
  }

  constructor(runtime, handle, siteWords) {
    this.runtime = runtime;
    this.handle = handle >>> 0;
    this.siteId = wordsToHex(siteWords);
    this.localUpdateListeners = new Set();
    this.changeListeners = new Set();
    this.transactionDepth = 0;
    this.transactionOrigin = undefined;
    this.destroyed = false;
  }

  assertLive() {
    if (this.destroyed) throw new EsbtError(24, "esbt: document has been destroyed");
  }

  destroy() {
    if (this.destroyed) return;
    this.runtime.check(this.runtime.exports.esbt_doc_destroy(this.handle));
    this.destroyed = true;
    this.localUpdateListeners.clear();
    this.changeListeners.clear();
  }

  get length() {
    this.assertLive();
    return this.runtime.check(this.runtime.exports.esbt_doc_len(this.handle));
  }

  getText() {
    this.assertLive();
    this.runtime.check(this.runtime.exports.esbt_doc_text_utf16(this.handle));
    return decodeUtf16(this.runtime.last());
  }

  stateHash() {
    this.assertLive();
    return this.runtime.exports.esbt_doc_hash(this.handle) >>> 0;
  }

  get pendingOperations() {
    this.assertLive();
    return this.runtime.check(this.runtime.exports.esbt_doc_pending(this.handle));
  }

  version() {
    this.assertLive();
    this.runtime.check(this.runtime.exports.esbt_doc_version(this.handle));
    return this.runtime.last();
  }

  transact(fn, options = {}) {
    this.assertLive();
    if (this.transactionDepth > 0) {
      this.transactionDepth += 1;
      try {
        const value = fn();
        if (value && typeof value.then === "function") {
          throw new TypeError("esbt: transact callback must be synchronous");
        }
        return value;
      } finally {
        this.transactionDepth -= 1;
      }
    }

    const [hasGroup, low, high] = encodeUndoGroup(options.undoGroup);
    this.runtime.check(
      this.runtime.exports.esbt_doc_begin(this.handle, hasGroup, low, high),
    );
    this.transactionDepth = 1;
    this.transactionOrigin = options.origin;
    try {
      const value = fn();
      if (value && typeof value.then === "function") {
        throw new TypeError("esbt: transact callback must be synchronous");
      }
      this.transactionDepth = 0;
      const result = this.runtime.check(this.runtime.exports.esbt_doc_commit(this.handle));
      this.consumeLocalResult(result, this.transactionOrigin);
      return value;
    } catch (error) {
      this.transactionDepth = 0;
      try {
        this.runtime.check(this.runtime.exports.esbt_doc_abort(this.handle));
      } catch (_) {
        // The Rust edit path already rolls back a transaction that fails.
      }
      throw error;
    } finally {
      this.transactionOrigin = undefined;
    }
  }

  insert(index, text, options = {}) {
    return this.replaceRange(index, index, text, options);
  }

  delete(index, length, options = {}) {
    this.assertLive();
    const [hasGroup, low, high] = encodeUndoGroup(options.undoGroup);
    const result = this.runtime.check(
      this.runtime.exports.esbt_doc_delete(
        this.handle,
        checkedIndex(index),
        checkedIndex(length),
        hasGroup,
        low,
        high,
      ),
    );
    return this.consumeLocalResult(result, options.origin);
  }

  replaceRange(from, to, insertedText, options = {}) {
    this.assertLive();
    const bytes = encodeUtf16(String(insertedText));
    const [hasGroup, low, high] = encodeUndoGroup(options.undoGroup);
    const result = this.runtime.withBytes(bytes, (pointer, length) =>
      this.runtime.check(
        this.runtime.exports.esbt_doc_replace_utf16(
          this.handle,
          checkedIndex(from),
          checkedIndex(to),
          pointer,
          length,
          hasGroup,
          low,
          high,
        ),
      ),
    );
    return this.consumeLocalResult(result, options.origin);
  }

  setText(text, options = {}) {
    return this.replaceRange(0, this.length, text, options);
  }

  indexToAnchor(index, affinity = "after") {
    this.assertLive();
    const encodedAffinity = affinity === "before" ? 1 : affinity === "after" ? 2 : 0;
    const result = this.runtime.check(
      this.runtime.exports.esbt_doc_anchor(
        this.handle,
        checkedIndex(index),
        encodedAffinity,
      ),
    );
    if (result < 1) throw new EsbtError(25, "esbt: anchor creation returned no bytes");
    return this.runtime.last();
  }

  anchorToIndex(anchor) {
    this.assertLive();
    return this.runtime.withBytes(anchor, (pointer, length) =>
      this.runtime.check(
        this.runtime.exports.esbt_doc_resolve_anchor(this.handle, pointer, length),
      ),
    );
  }

  insertAtAnchor(anchor, text, options = {}) {
    this.assertLive();
    const textBytes = encodeUtf16(String(text));
    const [hasGroup, low, high] = encodeUndoGroup(options.undoGroup);
    const resultLength = this.runtime.withTwoBuffers(
      anchor,
      textBytes,
      (anchorPointer, anchorLength, textPointer, textLength) =>
        this.runtime.check(
          this.runtime.exports.esbt_doc_insert_at_anchor_utf16(
            this.handle,
            anchorPointer,
            anchorLength,
            textPointer,
            textLength,
            hasGroup,
            low,
            high,
          ),
        ),
    );
    if (resultLength < 8) throw new EsbtError(4, "esbt: malformed insert-at-anchor result");
    const bytes = this.runtime.last();
    const reader = new ByteReader(bytes);
    const nextAnchor = reader.bytes(reader.u32());
    const update = reader.bytes(reader.u32());
    reader.finish();
    if (update.length > 0) {
      this.emitLocalUpdate(update, this.readVisibleEdits(), options.origin);
    }
    return { anchor: nextAnchor, update: update.length > 0 ? update : null };
  }

  applyUpdate(bytes) {
    this.assertLive();
    const receiptBytes = this.runtime.withBytes(bytes, (pointer, length) => {
      this.runtime.check(this.runtime.exports.esbt_doc_apply(this.handle, pointer, length));
      return this.runtime.last();
    });
    const receipt = decodeApplyReceipt(receiptBytes);
    receipt.visibleEdits = this.readVisibleEdits();
    if (receipt.visibleChanged !== (receipt.visibleEdits.length > 0)) {
      throw new EsbtError(4, "esbt: apply receipt disagrees with visible edits");
    }
    if (receipt.visibleEdits.length > 0) {
      this.emitChange(receipt.visibleEdits, undefined, false);
    }
    return receipt;
  }

  applySnapshot(bytes) {
    this.assertLive();
    const receiptBytes = this.runtime.withBytes(bytes, (pointer, length) => {
      this.runtime.check(
        this.runtime.exports.esbt_doc_apply_snapshot(this.handle, pointer, length),
      );
      return this.runtime.last();
    });
    const receipt = decodeSnapshotReceipt(receiptBytes);
    receipt.visibleEdits = this.readVisibleEdits();
    if (receipt.visibleChanged !== (receipt.visibleEdits.length > 0)) {
      throw new EsbtError(4, "esbt: snapshot receipt disagrees with visible edits");
    }
    if (receipt.visibleEdits.length > 0) {
      this.emitChange(receipt.visibleEdits, undefined, false);
    }
    return receipt;
  }

  import(bytes) {
    const tag = envelopeTag(bytes);
    if (tag === 3 || tag === 6) return this.applySnapshot(bytes);
    if (tag === 5) return this.applyUpdate(bytes);
    throw new EsbtError(4, "esbt: unsupported import envelope");
  }

  exportFullSnapshot() {
    this.assertLive();
    this.runtime.check(this.runtime.exports.esbt_doc_export_full_snapshot(this.handle));
    return this.runtime.last();
  }

  exportCompactSnapshot() {
    this.assertLive();
    this.runtime.check(this.runtime.exports.esbt_doc_export_compact_snapshot(this.handle));
    return this.runtime.last();
  }

  exportUpdate(remoteVersion = new Uint8Array([0, 0, 0, 0])) {
    this.assertLive();
    return this.runtime.withBytes(remoteVersion, (pointer, length) => {
      this.runtime.check(
        this.runtime.exports.esbt_doc_export_update(this.handle, pointer, length),
      );
      return this.runtime.last();
    });
  }

  pruneHistoryThrough(version) {
    this.assertLive();
    return this.runtime.withBytes(version, (pointer, length) =>
      this.runtime.check(
        this.runtime.exports.esbt_doc_prune_history(this.handle, pointer, length),
      ),
    );
  }

  /** Retained journal size — the quantity compaction policy must bound. */
  get retainedOperations() {
    this.assertLive();
    return this.runtime.check(
      this.runtime.exports.esbt_doc_retained_operations(this.handle),
    );
  }

  /** Encoded causal prefix below which reconnect deltas are unavailable. */
  historyFloor() {
    this.assertLive();
    this.runtime.check(this.runtime.exports.esbt_doc_history_floor(this.handle));
    return this.runtime.last();
  }

  /** Current Dmax (moves over time when the adaptive controller is on). */
  currentDmax() {
    this.assertLive();
    this.runtime.check(this.runtime.exports.esbt_doc_current_dmax(this.handle));
    const bytes = this.runtime.last();
    return Number(new DataView(bytes.buffer, bytes.byteOffset, 8).getBigInt64(0, true));
  }

  get canUndo() {
    this.assertLive();
    return this.runtime.check(this.runtime.exports.esbt_doc_can_undo(this.handle)) === 1;
  }

  get canRedo() {
    this.assertLive();
    return this.runtime.check(this.runtime.exports.esbt_doc_can_redo(this.handle)) === 1;
  }

  undo(options = {}) {
    this.assertLive();
    const result = this.runtime.check(this.runtime.exports.esbt_doc_undo(this.handle));
    return this.consumeLocalResult(result, options.origin ?? "undo");
  }

  redo(options = {}) {
    this.assertLive();
    const result = this.runtime.check(this.runtime.exports.esbt_doc_redo(this.handle));
    return this.consumeLocalResult(result, options.origin ?? "redo");
  }

  onLocalUpdate(listener) {
    this.localUpdateListeners.add(listener);
    return () => this.localUpdateListeners.delete(listener);
  }

  onChange(listener) {
    this.changeListeners.add(listener);
    return () => this.changeListeners.delete(listener);
  }

  consumeLocalResult(result, origin) {
    if (result === 0) return null;
    const update = this.runtime.last();
    this.emitLocalUpdate(update, this.readVisibleEdits(), origin);
    return update;
  }

  emitLocalUpdate(update, edits, origin) {
    const stable = update.slice();
    for (const listener of [...this.localUpdateListeners]) {
      try {
        listener(stable.slice());
      } catch (error) {
        surfaceListenerError(error);
      }
    }
    if (edits.length > 0) this.emitChange(edits, origin, true);
  }

  readVisibleEdits() {
    this.runtime.check(this.runtime.exports.esbt_doc_visible_edits(this.handle));
    return decodeVisibleEdits(this.runtime.last());
  }

  emitChange(edits, origin, local) {
    if (this.changeListeners.size === 0) return;
    const event = { edits: edits.map((edit) => ({ ...edit })), origin, local };
    for (const listener of [...this.changeListeners]) {
      try {
        listener(event);
      } catch (error) {
        surfaceListenerError(error);
      }
    }
  }
}

function surfaceListenerError(error) {
  if (typeof globalThis.reportError === "function") {
    globalThis.reportError(error);
    return;
  }
  queueMicrotask(() => {
    throw error;
  });
}

function normalizeSiteId(siteId) {
  if (siteId === undefined || siteId === null) {
    const words = crypto.getRandomValues(new Uint32Array(4));
    if (words.every((word) => word === 0)) words[0] = 1;
    return [...words];
  }
  if (typeof siteId === "string") {
    const hex = siteId.replaceAll("-", "").toLowerCase();
    if (!/^[0-9a-f]{32}$/.test(hex) || /^0+$/.test(hex)) {
      throw new TypeError("esbt: siteId must be a nonzero 128-bit hexadecimal string");
    }
    const bytes = Uint8Array.from(hex.match(/../g), (part) => Number.parseInt(part, 16));
    return bytesToWords(bytes);
  }
  if (siteId instanceof Uint8Array && siteId.length === 16) {
    if (siteId.every((byte) => byte === 0)) throw new TypeError("esbt: siteId is zero");
    return bytesToWords(siteId);
  }
  throw new TypeError("esbt: siteId must be a 16-byte array or 32-digit hex string");
}

function bytesToWords(bigEndianBytes) {
  const words = [];
  for (let word = 0; word < 4; word++) {
    let value = 0;
    for (let byte = 0; byte < 4; byte++) {
      value = (value << 8) | bigEndianBytes[(3 - word) * 4 + byte];
    }
    words.push(value >>> 0);
  }
  return words;
}

function wordsToHex(words) {
  return [...words]
    .reverse()
    .map((word) => (word >>> 0).toString(16).padStart(8, "0"))
    .join("");
}

function encodeUndoGroup(group) {
  if (group === undefined || group === null) return [0, 0, 0];
  const value = BigInt(group);
  if (value < 0n || value > 0xffff_ffff_ffff_ffffn) {
    throw new RangeError("esbt: undoGroup is outside u64");
  }
  return [1, Number(value & 0xffff_ffffn), Number(value >> 32n)];
}

function checkedIndex(value) {
  if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new RangeError("esbt: index must be a nonnegative u32 integer");
  }
  return value >>> 0;
}

function encodeUtf16(text) {
  const bytes = new Uint8Array(text.length * 2);
  const view = new DataView(bytes.buffer);
  for (let index = 0; index < text.length; index++) {
    view.setUint16(index * 2, text.charCodeAt(index), true);
  }
  return bytes;
}

function decodeUtf16(bytes) {
  if (bytes.length % 2 !== 0) throw new EsbtError(4, "esbt: odd UTF-16 result length");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const chunks = [];
  const chunkSize = 16_384;
  for (let offset = 0; offset < bytes.length / 2; offset += chunkSize) {
    const end = Math.min(bytes.length / 2, offset + chunkSize);
    const units = new Array(end - offset);
    for (let index = offset; index < end; index++) {
      units[index - offset] = view.getUint16(index * 2, true);
    }
    chunks.push(String.fromCharCode(...units));
  }
  return chunks.join("");
}

function decodeVisibleEdits(bytes) {
  const reader = new ByteReader(bytes);
  if (reader.u16() !== 1) throw new EsbtError(5, "esbt: unsupported visible-edit receipt");
  const count = reader.u32();
  const edits = [];
  for (let index = 0; index < count; index++) {
    const from = reader.u32();
    const to = reader.u32();
    const units = reader.u32();
    if (to < from || units > 1_000_000) {
      throw new EsbtError(4, "esbt: invalid visible-edit range");
    }
    edits.push({ from, to, insert: decodeUtf16(reader.bytes(units * 2)) });
  }
  reader.finish();
  return edits;
}

function envelopeTag(bytes) {
  if (
    !(bytes instanceof Uint8Array) ||
    bytes.length < 11 ||
    bytes[0] !== 0x45 ||
    bytes[1] !== 0x53 ||
    bytes[2] !== 0x42 ||
    bytes[3] !== 0x4d
  ) {
    return -1;
  }
  return bytes[6];
}

function decodeApplyReceipt(bytes) {
  const reader = new ByteReader(bytes);
  if (reader.u16() !== 1) throw new EsbtError(5, "esbt: unsupported apply receipt");
  const outcomes = ["invalid", "applied", "duplicate", "buffered", "mixed", "noop"];
  const outcome = outcomes[reader.u8()];
  const visibleChanged = reader.u8() === 1;
  const lists = [];
  for (let list = 0; list < 4; list++) {
    const identities = [];
    const count = reader.u32();
    for (let index = 0; index < count; index++) {
      identities.push({ origin: reader.siteId(), sequence: reader.u64() });
    }
    lists.push(identities);
  }
  const version = reader.bytes(reader.u32());
  const journal = reader.bytes(reader.u32());
  reader.finish();
  return {
    outcome,
    visibleChanged,
    acceptedOperations: lists[0],
    appliedOperations: lists[1],
    bufferedOperations: lists[2],
    newlyReadyOperations: lists[3],
    version,
    journalBytes: journal.length > 0 ? journal : null,
    visibleEdits: [],
  };
}

function decodeSnapshotReceipt(bytes) {
  const reader = new ByteReader(bytes);
  if (reader.u16() !== 1) throw new EsbtError(5, "esbt: unsupported snapshot receipt");
  const kind = reader.u8() === 1 ? "full" : "compact";
  const visibleChanged = reader.u8() === 1;
  const undo = ["invalid", "preserved", "cleared", "partially-preserved"][reader.u8()];
  if (!undo) throw new EsbtError(4, "esbt: invalid snapshot undo disposition");
  const version = reader.bytes(reader.u32());
  reader.finish();
  return { kind, visibleChanged, undo, version, visibleEdits: [] };
}

class ByteReader {
  constructor(bytes) {
    this.value = bytes;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    this.offset = 0;
  }

  require(length) {
    if (this.offset + length > this.value.length) {
      throw new EsbtError(4, "esbt: truncated Wasm result");
    }
  }

  u8() {
    this.require(1);
    return this.value[this.offset++];
  }

  u16() {
    this.require(2);
    const value = this.view.getUint16(this.offset, true);
    this.offset += 2;
    return value;
  }

  u32() {
    this.require(4);
    const value = this.view.getUint32(this.offset, true);
    this.offset += 4;
    return value;
  }

  u64() {
    this.require(8);
    const value = this.view.getBigUint64(this.offset, true);
    this.offset += 8;
    return value;
  }

  siteId() {
    const littleEndian = this.bytes(16);
    return [...littleEndian]
      .reverse()
      .map((byte) => byte.toString(16).padStart(2, "0"))
      .join("");
  }

  bytes(length) {
    this.require(length);
    const value = this.value.slice(this.offset, this.offset + length);
    this.offset += length;
    return value;
  }

  finish() {
    if (this.offset !== this.value.length) {
      throw new EsbtError(6, "esbt: trailing bytes in Wasm result");
    }
  }
}
