#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { EsbtDocument, EsbtError, EsbtRuntime } from "../web/esbt-document.js";

const generated = new URL("../web/generated/", import.meta.url);
const loadedModules = new Set();
const runtime = await EsbtRuntime.load({
  getCoreModule: async (name) => {
    loadedModules.add(name);
    return WebAssembly.compile(await readFile(new URL(name, generated)));
  },
});

assert.equal(runtime.engine.wireVersion(), 1);
assert.deepEqual([...loadedModules].sort(), ["esbt.core.wasm", "esbt.core2.wasm", "esbt.core3.wasm"]);
assert.equal(runtime.classifyArtifact(runtime.emptyVersion()), "version");

const goldenText = await readFile(
  new URL("../tests/golden/esbt-codec.txt", import.meta.url),
  "utf8",
);
for (const line of goldenText.split("\n")) {
  if (!line || line.startsWith("#")) continue;
  const [kind, hex] = line.split(" ");
  const bytes = Uint8Array.from(Buffer.from(hex, "hex"));
  assert.equal(runtime.classifyArtifact(bytes), kind, `component classifies ${kind} golden`);
}

const makeDocument = (site, config) =>
  EsbtDocument.create({ runtime, siteId: site.padStart(32, "0"), config });
const [a, b, c] = await Promise.all([makeDocument("1"), makeDocument("2"), makeDocument("3")]);

let updateA;
let updateB;
a.onLocalUpdate((update) => (updateA = update));
b.onLocalUpdate((update) => (updateB = update));

a.transact(() => a.insert(0, "cat"));
b.transact(() => b.insert(0, "dog"));
assert.equal(runtime.classifyArtifact(updateA), "update");
assert.equal(runtime.classifyArtifact(updateB), "update");

a.applyUpdate(updateB);
b.applyUpdate(updateA);
assert.equal(a.getText(), b.getText());
assert.equal([...a.getText()].sort().join(""), [..."catdog"].sort().join(""));

const fullSnapshot = a.exportFullSnapshot();
assert.equal(runtime.classifyArtifact(fullSnapshot), "full-snapshot");
const fullReceipt = c.applySnapshot(fullSnapshot);
assert.equal(fullReceipt.kind, "full");
assert.equal(c.getText(), a.getText());

const anchor = a.indexToAnchor(a.length, "after");
assert.equal(runtime.classifyArtifact(anchor), "anchor");
const anchored = a.insertAtAnchor(anchor, "😀");
assert.ok(anchored.update);
b.applyUpdate(anchored.update);
c.applyUpdate(anchored.update);
assert.equal(a.getText(), b.getText());
assert.equal(b.getText(), c.getText());
assert.equal(a.anchorToIndex(anchored.anchor), a.length);

const causal = a.captureCausalPosition(a.length, "after");
assert.equal(runtime.classifyArtifact(causal), "causal-position");
const unseen = await makeDocument("4");
assert.equal(unseen.resolveCausalPosition(causal), null);
unseen.applySnapshot(a.exportFullSnapshot());
assert.equal(unseen.resolveCausalPosition(causal), a.length);

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

const reconnecting = await makeDocument("5");
reconnecting.applySnapshot(a.exportFullSnapshot());
const reconnectVersion = reconnecting.version();
a.insert(a.length, "-reconnect");
const reconnect = a.exportUpdate(reconnectVersion);
const reconnectReceipt = reconnecting.applyUpdate(reconnect);
assert.equal(reconnectReceipt.visibleChanged, true);
assert.equal(reconnecting.getText(), a.getText());

const configured = await makeDocument("6", {
  dmax: 64,
  strategy: { kind: "boundary-low", boundary: 32 },
  adaptiveDmax: { floor: 64, window: 16 },
  limits: { maxDocumentUnits: 4 },
});
assert.equal(configured.currentDmax(), 64);
configured.insert(0, "abcd");
assert.throws(() => configured.insert(4, "e"), (error) => error instanceof EsbtError && error.code === 15);
assert.equal(configured.getText(), "abcd");

assert.ok(a.retainedOperations > 0);
const beforePrune = a.retainedOperations;
const pruned = a.pruneHistoryThrough(a.version());
assert.equal(pruned, beforePrune);
assert.equal(a.retainedOperations, 0);
assert.equal(runtime.classifyArtifact(a.historyFloor()), "version");
assert.throws(
  () => a.exportUpdate(runtime.emptyVersion()),
  (error) => error instanceof EsbtError && error.code === 21,
);

// Clean-break proof: none of the former split envelopes is guessed.
for (const magic of ["ESBM", "ESBS", "ESBF", "ESBA"]) {
  assert.throws(
    () => runtime.classifyArtifact(
      Uint8Array.from([...Buffer.from(magic, "ascii"), 3, 0, 5, 0, 0, 0, 0]),
    ),
    (error) => error instanceof EsbtError && error.code === 4,
  );
}

for (const document of [a, b, c, unseen, reconnecting, configured]) document.destroy();

console.log(
  JSON.stringify({
    ok: true,
    wireVersion: runtime.engine.wireVersion(),
    coreModules: [...loadedModules].sort(),
  }),
);
