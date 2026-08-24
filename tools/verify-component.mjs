#!/usr/bin/env node

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { componentWit } from "@bytecodealliance/jco-transpile/wasm-tools";
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(fileURLToPath(new URL("..", import.meta.url)));
const release = path.join(root, "target/wasm32-unknown-unknown/release");
const generated = path.join(root, "web/generated");
const componentPath = path.join(release, "esbt.component.wasm");
const extractedPath = path.join(release, "esbt.component.wit");
const sourcePath = path.join(root, "wit/esbt.wit");

const [component, extracted, source, generatedNames] = await Promise.all([
  readFile(componentPath),
  readFile(extractedPath, "utf8"),
  readFile(sourcePath, "utf8"),
  readdir(generated),
]);

const actualWit = await componentWit(new Uint8Array(component));
assert.equal(
  extracted,
  actualWit,
  "extracted WIT sidecar differs from the actual component binary",
);

assert.equal(
  component.subarray(0, 8).toString("hex"),
  "0061736d0d000100",
  "esbt.component.wasm is not a WebAssembly component binary",
);

assert.ok(
  actualWit.includes("export esbt:document/engine@1.0.0"),
  "component does not export the versioned ESBT engine interface",
);

const requiredWit = [
  "package esbt:document@1.0.0",
  "resource document",
  "default-config",
  "default-adaptive-dmax-config",
  "create",
  "wire-version",
  "empty-version",
  "classify-artifact",
  "version-covers",
  "apply-update",
  "apply-snapshot",
  "capture-causal-position",
  "resolve-causal-position",
];
for (const token of requiredWit) {
  assert.ok(source.includes(token), `source WIT is missing ${JSON.stringify(token)}`);
}

const interfaceName = generatedNames.find(
  (name) => name.startsWith("interfaces") === false && name === "esbt.d.ts",
);
assert.equal(interfaceName, "esbt.d.ts", "Jco root declaration is missing");
const interfaceFiles = await readdir(path.join(generated, "interfaces"));
const engineDeclarationName = interfaceFiles.find((name) => name.endsWith("-engine.d.ts"));
assert.ok(engineDeclarationName, "Jco engine declaration is missing");
const engineTypes = await readFile(
  path.join(generated, "interfaces", engineDeclarationName),
  "utf8",
);

// These assertions validate the canonical ABI's actual lifted JS value types,
// not merely a list of exported function names.
for (const token of [
  "export type Bytes = Uint8Array",
  "export type Utf16Units = Uint16Array",
  "low: bigint",
  "high: bigint",
  "export class Document",
  "replace(from: number, to: number, inserted: Utf16Units",
  "applyUpdate(update: Bytes): ApplyReceipt",
  "stateHash(): bigint",
]) {
  assert.ok(engineTypes.includes(token), `generated component types are missing ${token}`);
}

const coreNames = generatedNames.filter((name) => name.endsWith(".wasm")).sort();
assert.ok(coreNames.length >= 1, "Jco did not emit any core Wasm modules");
for (const name of coreNames) {
  const bytes = await readFile(path.join(generated, name));
  assert.equal(
    bytes.subarray(0, 8).toString("hex"),
    "0061736d01000000",
    `${name} is not a core WebAssembly module`,
  );
  // Parsing every generated core module catches value-section corruption and
  // unsupported output before a browser ever sees the release artifact.
  new WebAssembly.Module(bytes);
}

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
console.log(
  JSON.stringify({
    ok: true,
    componentBytes: component.length,
    componentSha256: sha256(component),
    witSha256: sha256(source),
    coreModules: coreNames,
  }),
);
