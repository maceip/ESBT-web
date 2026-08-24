#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";
import { EsbtDocument, EsbtRuntime } from "../web/esbt-document.js";

// Regression for the former hand-decoder's 1,000,000-unit visible-edit cap.
// Rust used to commit this change and then JavaScript rejected its receipt,
// splitting UI state from engine state. WIT now lifts `list<u16>` directly.
const unitCount = 1_000_001;
const generated = new URL("../web/generated/", import.meta.url);
const runtime = await EsbtRuntime.load({
  getCoreModule: async (name) =>
    WebAssembly.compile(await readFile(new URL(name, generated))),
});
const document = await EsbtDocument.create({
  runtime,
  siteId: "7".padStart(32, "0"),
  config: {
    limits: {
      maxMessageBytes: 128 * 1024 * 1024,
      maxOperationsPerUpdate: unitCount + 1,
      maxDocumentUnits: unitCount + 1,
    },
  },
});

let visibleLength = 0;
document.onChange(({ edits }) => {
  assert.equal(edits.length, 1);
  visibleLength = edits[0].insert.length;
});

const started = performance.now();
const update = document.insert(0, "x".repeat(unitCount));
const elapsedMs = performance.now() - started;

assert.ok(update instanceof Uint8Array);
assert.equal(runtime.classifyArtifact(update), "update");
assert.equal(document.length, unitCount);
assert.equal(visibleLength, unitCount);
assert.equal(document.getText().length, unitCount);
document.destroy();

console.log(JSON.stringify({ ok: true, unitCount, updateBytes: update.length, elapsedMs }));
