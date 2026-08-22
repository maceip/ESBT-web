#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { EsbtDocument, EsbtRuntime } from "../web/esbt-document.js";

const artifact = new URL(
  "../target/wasm32-unknown-unknown/release/esbt.wasm",
  import.meta.url,
);
const bytes = await readFile(artifact);
const module = new WebAssembly.Module(bytes);
const exported = WebAssembly.Module.exports(module).map(({ name }) => name).sort();
const shared = new Set(["esbt_malloc", "esbt_free", "esbt_last_len", "esbt_last_ptr"]);
const abi = exported.filter((name) => name.startsWith("esbt_"));

assert.ok(abi.some((name) => name === "esbt_doc_create"));
assert.ok(
  abi.every((name) => shared.has(name) || name.startsWith("esbt_doc_")),
  `unexpected Wasm ABI exports: ${abi.filter((name) => !shared.has(name) && !name.startsWith("esbt_doc_")).join(", ")}`,
);

const instance = new WebAssembly.Instance(module, { env: {} });
const runtime = new EsbtRuntime(instance.exports);
const makeDocument = (site) => EsbtDocument.create({ runtime, siteId: site.padStart(32, "0") });
const [a, b, c] = await Promise.all([
  makeDocument("1"),
  makeDocument("2"),
  makeDocument("3"),
]);

let updateA;
let updateB;
a.onLocalUpdate((update) => (updateA = update));
b.onLocalUpdate((update) => (updateB = update));

a.transact(() => {
  a.insert(0, "cat");
});
b.transact(() => {
  b.insert(0, "dog");
});
assert.ok(updateA);
assert.ok(updateB);

a.applyUpdate(updateB);
b.applyUpdate(updateA);
assert.equal(a.getText(), b.getText());
assert.equal([...a.getText()].sort().join(""), [..."catdog"].sort().join(""));

const fullReceipt = c.applySnapshot(a.exportFullSnapshot());
assert.equal(fullReceipt.kind, "full");
assert.equal(c.getText(), a.getText());

const anchor = a.indexToAnchor(a.length, "after");
const anchored = a.insertAtAnchor(anchor, "😀");
assert.ok(anchored.update);
b.applyUpdate(anchored.update);
c.applyUpdate(anchored.update);
assert.equal(a.getText(), b.getText());
assert.equal(b.getText(), c.getText());
assert.equal(a.anchorToIndex(anchored.anchor), a.length);

const beforeUndo = a.getText();
const undo = a.undo();
assert.ok(undo);
b.applyUpdate(undo);
c.applyUpdate(undo);
assert.equal(a.getText(), b.getText());
const redo = a.redo();
assert.ok(redo);
b.applyUpdate(redo);
c.applyUpdate(redo);
assert.equal(a.getText(), beforeUndo);
assert.equal(a.getText(), b.getText());
assert.equal(b.getText(), c.getText());

const d = await makeDocument("4");
d.applySnapshot(a.exportFullSnapshot());
const dVersion = d.version();
a.insert(a.length, "-reconnect");
const reconnect = a.exportUpdate(dVersion);
const reconnectReceipt = d.applyUpdate(reconnect);
assert.equal(reconnectReceipt.visibleChanged, true);
assert.equal(d.getText(), a.getText());
const reconnectText = d.getText();

for (const document of [a, b, c, d]) document.destroy();

console.log(
  JSON.stringify({
    ok: true,
    wasmBytes: bytes.byteLength,
    abiExports: abi,
    reconnectText,
  }),
);
