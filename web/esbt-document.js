/** High-level browser owner for the WIT-generated ESBT component. */

import { instantiate } from "./generated/esbt.js";

const DISPOSE = Symbol.dispose ?? Symbol.for("dispose");
const U64_MAX = 0xffff_ffff_ffff_ffffn;
const U128_MAX = (1n << 128n) - 1n;

export class EsbtError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "EsbtError";
    this.code = code;
  }
}

function callComponent(callback) {
  try {
    return callback();
  } catch (error) {
    const payload = error?.payload;
    if (payload && Number.isInteger(payload.code) && typeof payload.message === "string") {
      throw new EsbtError(payload.code, payload.message);
    }
    throw error;
  }
}

async function compileResponse(response, url) {
  if (!response.ok) {
    throw new Error(`esbt: failed to fetch ${url} (${response.status})`);
  }
  if (typeof WebAssembly.compileStreaming === "function") {
    try {
      return await WebAssembly.compileStreaming(Promise.resolve(response.clone()));
    } catch {
      // A development server may omit application/wasm. The buffered path is
      // still standards-compatible in Chromium, Firefox, and WebKit.
    }
  }
  return WebAssembly.compile(await response.arrayBuffer());
}

export class EsbtRuntime {
  constructor(engine) {
    this.engine = engine;
  }

  /**
   * Instantiate Jco's core-Wasm output. `getCoreModule` is injectable for
   * Node tests, service workers, caches, and integrity-verifying loaders.
   */
  static async load(options = {}) {
    if (typeof options === "string" || options instanceof URL) {
      options = { baseUrl: options };
    }
    const baseUrl = new URL(options.baseUrl ?? "./generated/", import.meta.url);
    const getCoreModule =
      options.getCoreModule ??
      (async (name) => {
        const url = new URL(name, baseUrl);
        return compileResponse(await fetch(url), url);
      });
    const root = await instantiate(getCoreModule, {});
    if (!root?.engine) throw new Error("esbt: component did not export the WIT engine interface");
    return new EsbtRuntime(root.engine);
  }

  defaultConfig() {
    return this.engine.defaultConfig();
  }

  resolveConfig(config = {}) {
    const defaults = this.defaultConfig();
    const strategy = config.strategy
      ? {
          kind: config.strategy.kind,
          boundary:
            config.strategy.kind === "midpoint" ? 0 : (config.strategy.boundary ?? 64),
        }
      : defaults.strategy;
    const adaptiveDmax = Object.hasOwn(config, "adaptiveDmax")
      ? config.adaptiveDmax == null
        ? undefined
        : { ...this.engine.defaultAdaptiveDmaxConfig(), ...config.adaptiveDmax }
      : defaults.adaptiveDmax;
    return {
      dmax: config.dmax ?? defaults.dmax,
      base: config.base ?? defaults.base,
      depth: config.depth ?? defaults.depth,
      strategy,
      adaptiveDmax,
      limits: { ...defaults.limits, ...(config.limits ?? {}) },
    };
  }

  classifyArtifact(bytes) {
    return callComponent(() => this.engine.classifyArtifact(asBytes(bytes)));
  }

  emptyVersion() {
    return this.engine.emptyVersion().slice();
  }
}

export class EsbtDocument {
  static async create(options = {}) {
    const runtime =
      options.runtime ??
      (await EsbtRuntime.load({
        baseUrl: options.componentBaseUrl,
        getCoreModule: options.getCoreModule,
      }));
    const site = normalizeSiteId(options.siteId);
    const component = callComponent(() =>
      runtime.engine.create(site, runtime.resolveConfig(options.config)),
    );
    return new EsbtDocument(runtime, component, siteToHex(site));
  }

  constructor(runtime, component, siteId) {
    this.runtime = runtime;
    this.component = component;
    this.siteId = siteId;
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
    this.component[DISPOSE]?.();
    this.destroyed = true;
    this.localUpdateListeners.clear();
    this.changeListeners.clear();
  }

  get length() {
    this.assertLive();
    return this.component.length();
  }

  getText() {
    this.assertLive();
    return decodeUtf16(this.component.text());
  }

  stateHash() {
    this.assertLive();
    return this.component.stateHash();
  }

  get pendingOperations() {
    this.assertLive();
    return this.component.pendingOperations();
  }

  get retainedOperations() {
    this.assertLive();
    return this.component.retainedOperations();
  }

  version() {
    this.assertLive();
    return this.component.version().slice();
  }

  historyFloor() {
    this.assertLive();
    return this.component.historyFloor().slice();
  }

  currentDmax() {
    this.assertLive();
    return this.component.currentDmax();
  }

  transact(callback, options = {}) {
    this.assertLive();
    if (this.transactionDepth > 0) {
      this.transactionDepth += 1;
      try {
        const value = callback();
        if (value && typeof value.then === "function") {
          throw new TypeError("esbt: transact callback must be synchronous");
        }
        return value;
      } finally {
        this.transactionDepth -= 1;
      }
    }

    callComponent(() => this.component.beginTransaction(normalizeUndoGroup(options.undoGroup)));
    this.transactionDepth = 1;
    this.transactionOrigin = options.origin;
    try {
      const value = callback();
      if (value && typeof value.then === "function") {
        throw new TypeError("esbt: transact callback must be synchronous");
      }
      this.transactionDepth = 0;
      const change = callComponent(() => this.component.commitTransaction());
      this.consumeLocalChange(change, this.transactionOrigin);
      return value;
    } catch (error) {
      this.transactionDepth = 0;
      try {
        callComponent(() => this.component.abortTransaction());
      } catch {
        // A failing Rust edit atomically rolls its active transaction back.
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
    const start = checkedIndex(index);
    const count = checkedIndex(length);
    const end = start + count;
    if (!Number.isSafeInteger(end) || end > 0xffff_ffff) {
      throw new RangeError("esbt: deletion endpoint exceeds u32");
    }
    return this.replaceRange(start, end, "", options);
  }

  replaceRange(from, to, insertedText, options = {}) {
    this.assertLive();
    const change = callComponent(() =>
      this.component.replace(
        checkedIndex(from),
        checkedIndex(to),
        encodeUtf16(String(insertedText)),
        normalizeUndoGroup(options.undoGroup),
      ),
    );
    return this.consumeLocalChange(change, options.origin);
  }

  setText(text, options = {}) {
    return this.replaceRange(0, this.length, text, options);
  }

  indexToAnchor(index, affinity = "after") {
    this.assertLive();
    return callComponent(() =>
      this.component.anchor(checkedIndex(index), checkedAffinity(affinity)).slice(),
    );
  }

  anchorToIndex(anchor) {
    this.assertLive();
    return callComponent(() => this.component.resolveAnchor(asBytes(anchor)));
  }

  captureCausalPosition(index, affinity = "after") {
    this.assertLive();
    return callComponent(() =>
      this.component
        .captureCausalPosition(checkedIndex(index), checkedAffinity(affinity))
        .slice(),
    );
  }

  resolveCausalPosition(position) {
    this.assertLive();
    return callComponent(() => this.component.resolveCausalPosition(asBytes(position))) ?? null;
  }

  insertAtAnchor(anchor, text, options = {}) {
    this.assertLive();
    const result = callComponent(() =>
      this.component.insertAtAnchor(
        asBytes(anchor),
        encodeUtf16(String(text)),
        normalizeUndoGroup(options.undoGroup),
      ),
    );
    const update = this.consumeLocalChange(result.change, options.origin);
    return { anchor: result.anchor.slice(), update };
  }

  applyUpdate(bytes) {
    this.assertLive();
    const receipt = callComponent(() => this.component.applyUpdate(asBytes(bytes)));
    const mapped = mapApplyReceipt(receipt);
    if (mapped.visibleEdits.length > 0) this.emitChange(mapped.visibleEdits, undefined, false);
    return mapped;
  }

  applySnapshot(bytes) {
    this.assertLive();
    const receipt = callComponent(() => this.component.applySnapshot(asBytes(bytes)));
    const mapped = mapSnapshotReceipt(receipt);
    if (mapped.visibleEdits.length > 0) this.emitChange(mapped.visibleEdits, undefined, false);
    return mapped;
  }

  import(bytes) {
    const artifact = asBytes(bytes);
    switch (this.runtime.classifyArtifact(artifact)) {
      case "update":
        return this.applyUpdate(artifact);
      case "compact-snapshot":
      case "full-snapshot":
        return this.applySnapshot(artifact);
      default:
        throw new EsbtError(4, "esbt: artifact is not importable document state");
    }
  }

  exportFullSnapshot() {
    this.assertLive();
    return callComponent(() => this.component.exportFullSnapshot().slice());
  }

  exportCompactSnapshot() {
    this.assertLive();
    return callComponent(() => this.component.exportCompactSnapshot().slice());
  }

  exportUpdate(remoteVersion = this.runtime.emptyVersion()) {
    this.assertLive();
    return callComponent(() => this.component.exportUpdate(asBytes(remoteVersion)).slice());
  }

  pruneHistoryThrough(version) {
    this.assertLive();
    return callComponent(() => this.component.pruneHistoryThrough(asBytes(version)));
  }

  get canUndo() {
    this.assertLive();
    return this.component.canUndo();
  }

  get canRedo() {
    this.assertLive();
    return this.component.canRedo();
  }

  undo(options = {}) {
    this.assertLive();
    const change = callComponent(() => this.component.undo());
    return this.consumeLocalChange(change, options.origin ?? "undo");
  }

  redo(options = {}) {
    this.assertLive();
    const change = callComponent(() => this.component.redo());
    return this.consumeLocalChange(change, options.origin ?? "redo");
  }

  onLocalUpdate(listener) {
    this.localUpdateListeners.add(listener);
    return () => this.localUpdateListeners.delete(listener);
  }

  onChange(listener) {
    this.changeListeners.add(listener);
    return () => this.changeListeners.delete(listener);
  }

  consumeLocalChange(change, origin) {
    if (!change) return null;
    const update = change.update.slice();
    const edits = change.visibleEdits.map(mapVisibleEdit);
    if (change.visibleChanged !== (edits.length > 0)) {
      throw new EsbtError(4, "esbt: local change disagrees with its visible edits");
    }
    this.emitLocalUpdate(update, edits, origin);
    return update;
  }

  emitLocalUpdate(update, edits, origin) {
    for (const listener of [...this.localUpdateListeners]) {
      try {
        listener(update.slice());
      } catch (error) {
        surfaceListenerError(error);
      }
    }
    if (edits.length > 0) this.emitChange(edits, origin, true);
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

function mapVisibleEdit(edit) {
  return { from: edit.from, to: edit.to, insert: decodeUtf16(edit.inserted) };
}

function mapOperationRef(identity) {
  return { origin: siteToHex(identity.origin), sequence: identity.sequence };
}

function mapApplyReceipt(receipt) {
  const visibleEdits = receipt.visibleEdits.map(mapVisibleEdit);
  if (receipt.visibleChanged !== (visibleEdits.length > 0)) {
    throw new EsbtError(4, "esbt: apply receipt disagrees with its visible edits");
  }
  return {
    outcome: receipt.outcome,
    acceptedOperations: receipt.acceptedOperations.map(mapOperationRef),
    appliedOperations: receipt.appliedOperations.map(mapOperationRef),
    bufferedOperations: receipt.bufferedOperations.map(mapOperationRef),
    newlyReadyOperations: receipt.newlyReadyOperations.map(mapOperationRef),
    version: receipt.version.slice(),
    visibleChanged: receipt.visibleChanged,
    visibleEdits,
    journalBytes: receipt.journal?.slice() ?? null,
  };
}

function mapSnapshotReceipt(receipt) {
  const visibleEdits = receipt.visibleEdits.map(mapVisibleEdit);
  if (receipt.visibleChanged !== (visibleEdits.length > 0)) {
    throw new EsbtError(4, "esbt: snapshot receipt disagrees with its visible edits");
  }
  return {
    kind: receipt.kind,
    version: receipt.version.slice(),
    visibleChanged: receipt.visibleChanged,
    visibleEdits,
    undo: receipt.undo,
  };
}

function normalizeSiteId(siteId) {
  let value;
  if (siteId === undefined || siteId === null) {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    if (bytes.every((byte) => byte === 0)) bytes[15] = 1;
    value = BigInt(`0x${bytesToHex(bytes)}`);
  } else if (typeof siteId === "string") {
    const hex = siteId.replaceAll("-", "").toLowerCase();
    if (!/^[0-9a-f]{32}$/.test(hex)) {
      throw new TypeError("esbt: siteId must be a 128-bit hexadecimal string");
    }
    value = BigInt(`0x${hex}`);
  } else if (siteId instanceof Uint8Array && siteId.length === 16) {
    value = BigInt(`0x${bytesToHex(siteId)}`);
  } else {
    throw new TypeError("esbt: siteId must be a 16-byte array or 32-digit hex string");
  }
  if (value <= 0n || value > U128_MAX) throw new TypeError("esbt: siteId is zero or out of range");
  return { low: value & U64_MAX, high: value >> 64n };
}

function siteToHex(site) {
  return ((site.high << 64n) | site.low).toString(16).padStart(32, "0");
}

function bytesToHex(bytes) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function normalizeUndoGroup(group) {
  if (group === undefined || group === null) return undefined;
  const value = BigInt(group);
  if (value < 0n || value > U64_MAX) throw new RangeError("esbt: undoGroup is outside u64");
  return value;
}

function checkedIndex(value) {
  if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new RangeError("esbt: index must be a nonnegative u32 integer");
  }
  return value;
}

function checkedAffinity(value) {
  if (value !== "before" && value !== "after") {
    throw new TypeError("esbt: affinity must be 'before' or 'after'");
  }
  return value;
}

function asBytes(value) {
  if (value instanceof Uint8Array) return value;
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  throw new TypeError("esbt: expected a byte array");
}

function encodeUtf16(text) {
  const units = new Uint16Array(text.length);
  for (let index = 0; index < text.length; index += 1) units[index] = text.charCodeAt(index);
  return units;
}

function decodeUtf16(units) {
  const chunks = [];
  const chunkSize = 16_384;
  for (let offset = 0; offset < units.length; offset += chunkSize) {
    chunks.push(String.fromCharCode(...units.subarray(offset, offset + chunkSize)));
  }
  return chunks.join("");
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
