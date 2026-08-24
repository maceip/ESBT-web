#!/usr/bin/env node

import { transpileBytes } from "@bytecodealliance/jco-transpile";
import {
  componentNew,
  componentWit,
} from "@bytecodealliance/jco-transpile/wasm-tools";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";

const [corePath, componentPath, generatedDirectory, extractedWitPath] =
  process.argv.slice(2).map((value) => path.resolve(value));
if (!extractedWitPath) {
  throw new Error(
    "usage: build-component <core.wasm> <component.wasm> <generated-dir> <component.wit>",
  );
}

const core = new Uint8Array(await readFile(corePath));
const component = await componentNew(core);
const extractedWit = await componentWit(component);
const output = await transpileBytes(component, {
  name: "esbt",
  instantiation: "async",
  strict: true,
  noComponentErrorWrapping: false,
  namespacedExports: false,
  nodejsCompat: false,
  wasiShim: false,
  base64Cutoff: 0,
  emitTypescriptDeclarations: true,
  outDir: generatedDirectory,
});

const textDecoder = new TextDecoder();
const textEncoder = new TextEncoder();
const normalizeGeneratedText = (filePath, bytes) => {
  if (!filePath.endsWith(".js") && !filePath.endsWith(".d.ts")) return bytes;
  const source = textDecoder.decode(bytes);
  return textEncoder.encode(`${source.replace(/[ \t]+$/gmu, "").trimEnd()}\n`);
};

if (output.imports.length !== 0) {
  throw new Error(`ESBT component unexpectedly imports ${output.imports.join(", ")}`);
}
if (!output.exports.some(([name, kind]) => name === "engine" && kind === "instance")) {
  throw new Error("ESBT component does not export the engine instance");
}

await rm(generatedDirectory, { force: true, recursive: true });
await Promise.all([
  mkdir(path.dirname(componentPath), { recursive: true }),
  mkdir(generatedDirectory, { recursive: true }),
  mkdir(path.dirname(extractedWitPath), { recursive: true }),
]);
await Promise.all([
  writeFile(componentPath, component),
  writeFile(extractedWitPath, extractedWit),
  ...Object.entries(output.files).map(async ([filePath, bytes]) => {
    const destination = path.resolve(filePath);
    const relative = path.relative(generatedDirectory, destination);
    if (relative.startsWith("..") || path.isAbsolute(relative)) {
      throw new Error(`transpiler output escapes the generated directory: ${filePath}`);
    }
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, normalizeGeneratedText(filePath, bytes));
  }),
]);
